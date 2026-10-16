# VM を使う確認スクリプトの共通部分。source して使う。
#
# 確認用のスクリプトを読み取り専用のドライブ (/dev/vdd) として渡し、シリアルコンソールには
# 短いコマンドだけを流す (長い行を流すと文字が落ちることがある)。
# ゲスト側のスクリプトは結果を "==RESULT== 名前: ok|FAIL 出力" の行で出し、最後に VM を止める。

dev_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dev_root="$(cd "$dev_here/../../.." && pwd)"
dev_state="$dev_root/build/dev"
dev_logs="$dev_state/logs"
mkdir -p "$dev_logs"
export CHLOE_DEV_NAME="${CHLOE_DEV_NAME:-chloe-dev-test}"

dev_scripts="$(mktemp -d)"
trap 'rm -rf "$dev_scripts" "$dev_state/test.ext4"; docker kill "$CHLOE_DEV_NAME" >/dev/null 2>&1 || true' EXIT

# ゲスト側で使う関数。確認用のスクリプトの先頭で source する。
cat > "$dev_scripts/lib.sh" <<'EOF'
result() { printf '\n==RESULT== %s: %s\n' "$1" "$2"; }
check() {
    local name="$1"; shift
    if out="$("$@" 2>&1)"; then result "$name" "ok ${out//$'\n'/ | }"; else result "$name" "FAIL ${out//$'\n'/ | }"; fi
}
# 失敗することを期待する確認。
check_fails() {
    local name="$1"; shift
    if out="$("$@" 2>&1)"; then result "$name" "FAIL unexpectedly succeeded: ${out//$'\n'/ | }"; else result "$name" "ok ${out//$'\n'/ | }"; fi
}
systemctl is-system-running --wait >/dev/null
EOF

# $dev_scripts に置いたファイルから確認用のドライブを作る。
make_test_drive() {
    docker build -q -t chloe-guest-tools -f "$dev_root/images/guest/rootfs/tools.Dockerfile" \
        "$dev_root/images/guest/rootfs" >/dev/null
    docker run --rm --user "$(id -u):$(id -g)" -v "$dev_scripts:/src:ro" -v "$dev_state:/state" \
        chloe-guest-tools sh -c 'rm -f /state/test.ext4 && truncate -s 16M /state/test.ext4 && mkfs.ext4 -q -d /src /state/test.ext4'
}

# VM を起動して、確認用のドライブのスクリプトを実行する。
# 引数: ログの名前、ゲストで実行するスクリプト名、スクリプトへの引数
boot() {
    local name="$1" script="$2"; shift 2
    # プロセス置換にして、VM が止まったら入力側を待たずに戻るようにする。
    timeout "${BOOT_TIMEOUT:-300}" "$dev_here/run.sh" > "$dev_logs/$name.log" 2>&1 < <(
        sleep 25
        echo "mount -o ro /dev/vdd /mnt && bash /mnt/$script $*"
        sleep 600
    ) || true
    # ランナーのコンテナが root で書いたログを読めるようにする。
    docker run --rm -v "$dev_state:/state" chloe-guest-tools chown -R "$(id -u):$(id -g)" /state/logs /state/mitmproxy 2>/dev/null || true
    grep -a '^==RESULT==' "$dev_logs/$name.log" | sed 's/\r$//'
}

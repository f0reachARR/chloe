# VM を使う確認スクリプトの共通部分。source して使う。
#
# 確認用のスクリプトを読み取り専用のドライブ (/dev/vdd) として渡し、chloe-runner exec で
# guest-agent に実行させる。ゲスト側のスクリプトは結果を "==RESULT== 名前: ok|FAIL 出力" の行で出す。

dev_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dev_root="$(cd "$dev_here/../../.." && pwd)"
dev_state="$dev_root/build/dev"
dev_logs="$dev_state/logs"
mkdir -p "$dev_logs"
export CHLOE_DEV_NAME="${CHLOE_DEV_NAME:-chloe-dev-test}"
export CHLOE_VSOCK="$dev_state/vsock.sock"

cargo build --quiet --manifest-path "$dev_root/Cargo.toml" -p chloe-runner
runner="$dev_root/target/debug/chloe-runner"

dev_scripts="$(mktemp -d)"
trap 'rm -rf "$dev_scripts" "$dev_state/test.ext4"; docker kill "$CHLOE_DEV_NAME" >/dev/null 2>&1 || true' EXIT

# ゲスト側で使う関数。確認用のスクリプトの先頭で source する。
cat > "$dev_scripts/lib.sh" <<'EOF'
result() { printf '==RESULT== %s: %s\n' "$1" "$2"; }
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

# VM を起動して、確認用のドライブのスクリプトを root で実行し、VM を止める。
# 引数: ログの名前、ゲストで実行するスクリプト名、スクリプトへの引数
boot() {
    local name="$1" script="$2"; shift 2
    local log="$dev_logs/$name.log"
    start_vm
    "$runner" exec --user root -- \
        sh -c "mount -o ro /dev/vdd /mnt && bash /mnt/$script $*" > "$log" 2>&1 || true
    stop_vm
    grep -a '^==RESULT==' "$log"
}

# VM をバックグラウンドで起動し、guest-agent が応答するまで待つ。
start_vm() {
    CHLOE_DEV_DETACH=1 "$dev_here/run.sh" >/dev/null
    "$runner" exec --user root --wait 60 -- systemctl is-system-running --wait >/dev/null 2>&1 || true
}

# VM を再起動させて Firecracker を終わらせる。
stop_vm() {
    "$runner" exec --user root -- systemctl reboot >/dev/null 2>&1 || true
    timeout 60 docker wait "$CHLOE_DEV_NAME" >/dev/null 2>&1 || docker kill "$CHLOE_DEV_NAME" >/dev/null 2>&1 || true
    # ランナーのコンテナが root で書いたログを読めるようにする。
    docker run --rm -v "$dev_state:/state" chloe-guest-tools chown -R "$(id -u):$(id -g)" /state/logs /state/mitmproxy /state/egress 2>/dev/null || true
}

# ホスト側の確認。結果はゲスト側と同じ形式で出す。
host_check() {
    local name="$1" expected="$2"; shift 2
    local out
    out="$("$@" 2>&1)" || true
    if [[ "$out" == *"$expected"* ]]; then
        echo "==RESULT== $name: ok ${out//$'\n'/ | }"
    else
        echo "==RESULT== $name: FAIL expected '$expected', got: ${out//$'\n'/ | }"
    fi
}

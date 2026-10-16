#!/usr/bin/env bash
# VM を起動して段階 1 の終了条件を確かめる。
#
# 確認用のスクリプトを読み取り専用のドライブ (/dev/vdd) として渡し、シリアルコンソールには
# 短いコマンドだけを流す (長い行を流すと文字が落ちることがある)。
# 1 回目の起動でワークスペースと Docker のイメージに書き込み、2 回目の起動で残っていることを確かめる。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev"
logs="$state/logs"
mkdir -p "$logs"
export CHLOE_DEV_NAME=chloe-dev-smoke

scripts="$(mktemp -d)"
trap 'rm -rf "$scripts" "$state/test.ext4"; docker kill "$CHLOE_DEV_NAME" >/dev/null 2>&1 || true' EXIT

cat > "$scripts/check.sh" <<'EOF'
#!/bin/bash
# 結果は ==RESULT== で始まる行に出す。最後に VM を止める。
result() { printf '\n==RESULT== %s: %s\n' "$1" "$2"; }
check() {
    local name="$1"; shift
    if out="$("$@" 2>&1)"; then result "$name" "ok ${out//$'\n'/ | }"; else result "$name" "FAIL ${out//$'\n'/ | }"; fi
}

systemctl is-system-running --wait >/dev/null
case "$1" in
first)
    check system systemctl is-system-running
    for target in / /home/user /workspace /var/lib/docker /var/lib/containerd; do
        check "mount $target" findmnt -no SOURCE,FSTYPE --target "$target"
    done
    check claude claude --version
    check codex codex --version
    check hello-world sh -c 'docker run --rm hello-world | grep -m1 "Hello from Docker"'
    printf 'FROM busybox\nRUN echo built > /built\n' > /tmp/Dockerfile
    check build sh -c 'docker build -q -t chloe-smoke /tmp >/dev/null && docker run --rm chloe-smoke cat /built'
    check write sh -c 'echo persisted > /workspace/smoke.txt && sync && echo written'
    ;;
second)
    check workspace cat /workspace/smoke.txt
    check docker-image docker image inspect chloe-smoke --format '{{.Id}}'
    ;;
esac
result done ok
systemctl reboot
EOF

docker run --rm --user "$(id -u):$(id -g)" -v "$scripts:/src:ro" -v "$state:/state" chloe-guest-tools \
    sh -c 'truncate -s 16M /state/test.ext4 && mkfs.ext4 -q -d /src /state/test.ext4'

boot() {
    local name="$1"
    # プロセス置換にして、VM が止まったら入力側を待たずに戻るようにする。
    timeout 300 "$here/run.sh" > "$logs/$name.log" 2>&1 < <(
        sleep 25
        echo "mount -o ro /dev/vdd /mnt && bash /mnt/check.sh $name"
        sleep 300
    ) || true
    grep -a '^==RESULT==' "$logs/$name.log" | sed 's/\r$//'
}

echo "--- first boot"
boot first
echo "--- second boot"
boot second

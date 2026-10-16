#!/usr/bin/env bash
# VM を起動して段階 1 の終了条件を確かめる。
#
# 1 回目の起動でワークスペースと Docker のイメージに書き込み、2 回目の起動で残っていることを確かめる。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

cat > "$dev_scripts/smoke.sh" <<'EOF'
#!/bin/bash
source /mnt/lib.sh
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
    # Claude Code と Codex の内部サンドボックスが VM の中で動くか (bubblewrap、ユーザー namespace、Landlock)
    check bwrap-user sudo -u user bwrap --ro-bind / / --dev /dev --proc /proc --unshare-all --die-with-parent sh -c 'echo sandboxed as $(id -u)'
    check codex-sandbox sudo -u user sh -c 'cd /workspace && codex sandbox -- sh -c "touch /workspace/ok-write && echo wrote; touch /etc/ng 2>&1 || echo denied-etc"'
    check landlock sh -c 'dmesg | grep -i -m1 landlock || echo no-landlock-message'
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
make_test_drive

export CHLOE_NET_MODE=nat
echo "--- first boot"
boot smoke-first smoke.sh first
echo "--- second boot"
boot smoke-second smoke.sh second

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
    check mise sudo -iu user mise --version
    check mise-config sudo -iu user mise settings get disable_update_warning
    check hello-world sh -c 'docker run --rm hello-world | grep -m1 "Hello from Docker"'
    printf 'FROM busybox\nRUN echo built > /built\n' > /tmp/Dockerfile
    check build sh -c 'docker build -q -t chloe-smoke /tmp >/dev/null && docker run --rm chloe-smoke cat /built'
    # コーディングエージェントの内部サンドボックスが VM の中で動くか (bubblewrap、ユーザー namespace、Landlock)
    check bwrap-user sudo -u user bwrap --ro-bind / / --dev /dev --proc /proc --unshare-all --die-with-parent sh -c 'echo sandboxed as $(id -u)'
    check landlock sh -c 'dmesg | grep -i -m1 landlock || echo no-landlock-message'
    check write sh -c 'echo persisted > /workspace/smoke.txt && sync && echo written'
    ;;
second)
    check workspace cat /workspace/smoke.txt
    check docker-image docker image inspect chloe-smoke --format '{{.Id}}'
    ;;
esac
result done ok
EOF
make_test_drive

export CHLOE_NET_MODE=nat
echo "--- first boot"
boot smoke-first smoke.sh first
echo "--- second boot"
boot smoke-second smoke.sh second

echo "--- runner"
start_vm
host_check exit-code "status=3" sh -c '"$0" exec -- sh -c "exit 3"; echo "status=$?"' "$runner"
host_check stdin "HELLO" sh -c 'echo hello | "$0" exec -- tr a-z A-Z' "$runner"
host_check stderr "to-stderr" sh -c '"$0" exec -- sh -c "echo to-stderr >&2" 2>&1 >/dev/null' "$runner"
host_check default-user "(docker)" sh -c '"$0" exec -- id | grep -o "uid=[0-9]*(user).*"' "$runner"
host_check root-user "uid=0(root)" "$runner" exec --user root -- id
host_check cwd "/workspace" "$runner" exec -- pwd
host_check env "value-from-host" "$runner" exec -e CHLOE_TEST=value-from-host -- sh -c 'echo $CHLOE_TEST'
host_check tty "/dev/pts/" "$runner" exec -t -- tty
host_check tty-size "24 80" "$runner" exec -t -- stty size
# 端末つきのログインシェル。script コマンドでホスト側にも端末を用意して、入力を流し込む。
host_check shell "SHELL-OK user" sh -c 'printf "echo SHELL-OK \$(whoami)\nexit\n" | script -qec "$0 shell" /dev/null | grep -ao "SHELL-OK user"' "$runner"
stop_vm

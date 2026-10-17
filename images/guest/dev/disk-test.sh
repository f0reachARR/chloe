#!/usr/bin/env bash
# ディスクの大きさの確認。
#
# - ランナーは WORKSPACE_SIZE と DOCKER_SIZE の大きさでディスクを作る。
# - 次の起動で大きい値を渡すと、中身を残したままファイルシステムを広げる。
# - 小さい値を渡しても縮めない。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

rm -f "$dev_state/workspace.ext4" "$dev_state/docker.ext4"

cat > "$dev_scripts/disk.sh" <<'EOF'
#!/bin/bash
source /mnt/lib.sh
# ファイルシステムの大きさ (MiB) が範囲に入っているか。ext4 の管理領域のぶん、少し小さく出る。
size_between() {
    local mnt="$1" min="$2" max="$3" size
    size="$(df -B1M --output=size "$mnt" | tail -1 | tr -d ' ')"
    echo "${size}M"
    [ "$size" -ge "$min" ] && [ "$size" -le "$max" ]
}
case "$1" in
first)
    check workspace-size size_between /var/lib/chloe/workspace 1800 2048
    check docker-size size_between /var/lib/chloe/docker 1800 2048
    check marker sh -c 'echo kept > /workspace/chloe-marker && echo written'
    ;;
second)
    check workspace-grown size_between /var/lib/chloe/workspace 2800 3072
    check docker-not-shrunk size_between /var/lib/chloe/docker 1800 2048
    check marker-kept cat /workspace/chloe-marker
    ;;
esac
result done ok
EOF
make_test_drive

echo "--- create (2G and 2G)"
export WORKSPACE_SIZE=2G DOCKER_SIZE=2G
boot disk-first disk.sh first
echo "--- grow the workspace disk to 3G, ask for a 1G Docker disk"
export WORKSPACE_SIZE=3G DOCKER_SIZE=1G
boot disk-second disk.sh second
host_check workspace-file 3221225472 stat -c %s "$dev_state/workspace.ext4"
host_check docker-file 2147483648 stat -c %s "$dev_state/docker.ext4"

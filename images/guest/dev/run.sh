#!/usr/bin/env bash
# ビルドしたカーネルと rootfs で VM を起動し、シリアルコンソールにつなぐ。
#
# ワークスペースと Docker のイメージは build/dev/ に作り、起動をまたいで残す。
# VM のシェルには別の端末から `chloe-runner shell --vsock build/dev/vsock.sock` で入る。
# 終了するには VM の中で `sudo reboot` を実行する (reboot=k で Firecracker が終了する)。
set -euo pipefail

WORKSPACE_SIZE="${WORKSPACE_SIZE:-20G}"
DOCKER_SIZE="${DOCKER_SIZE:-40G}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
guest="$root/build/guest"
state="$root/build/dev"
mkdir -p "$state"

for f in "$guest/kernel/vmlinux" "$guest/rootfs.ext4"; do
    if [[ ! -f "$f" ]]; then
        echo "missing $f: run images/guest/kernel/build.sh and images/guest/rootfs/build.sh first" >&2
        exit 1
    fi
done

docker build -q -t chloe-guest-tools -f "$root/images/guest/rootfs/tools.Dockerfile" "$root/images/guest/rootfs" >/dev/null
for pair in "workspace:$WORKSPACE_SIZE" "docker:$DOCKER_SIZE"; do
    name="${pair%%:*}" size="${pair#*:}"
    img="$state/$name.ext4"
    if [[ ! -f "$img" ]]; then
        truncate -s "$size" "$img"
        docker run --rm --user "$(id -u):$(id -g)" -v "$state:/state" chloe-guest-tools \
            mkfs.ext4 -q -L "chloe-$name" "/state/$name.ext4"
    fi
done

# chloe-egress はホストでビルドしてコンテナに渡す (ホストの glibc はコンテナより古い)。
cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-egress

docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here" >/dev/null

# CHLOE_DEV_DETACH=1 ならバックグラウンドで起動する (確認スクリプトはこちらを使う)。
# そうでなければ、端末から起動したときだけ TTY を割り当ててシリアルコンソールにつなぐ。
if [[ "${CHLOE_DEV_DETACH:-}" == 1 ]]; then
    run_flags=(-d)
elif [[ -t 0 ]]; then
    run_flags=(-it)
else
    run_flags=(-i)
fi

exec docker run --rm "${run_flags[@]}" \
    --name "${CHLOE_DEV_NAME:-chloe-dev}" \
    -e CHLOE_NET_MODE="${CHLOE_NET_MODE:-nat}" \
    -e VCPUS="${VCPUS:-4}" \
    -e MEM_MIB="${MEM_MIB:-4096}" \
    --privileged \
    --device /dev/kvm \
    --device /dev/net/tun \
    -v "$guest:/guest:ro" \
    -v "$state:/state" \
    -v "$root/target/debug/chloe-egress:/opt/chloe/bin/chloe-egress:ro" \
    chloe-dev-runner

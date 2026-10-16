#!/usr/bin/env bash
# ビルドしたカーネルと rootfs で VM を起動し、シリアルコンソールにつなぐ。
#
# ワークスペースと Docker のイメージは build/dev/ に作り (ランナーのコンテナが作る)、起動をまたいで残す。
# CHLOE_GUEST_ENV にコンテナの中のパス (/state/...) を渡すと、その環境変数をゲストに入れる。
# VM のシェルには別の端末から `chloe-runner shell --vsock build/dev/vsock.sock` で入る。
# 終了するには VM の中で `sudo reboot` を実行する (reboot=k で Firecracker が終了する)。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
guest="$root/build/guest"
# CHLOE_DEV_STATE で状態のディレクトリを変えられる (確認を並べて実行するとき)。
state="${CHLOE_DEV_STATE:-$root/build/dev}"
mkdir -p "$state"

for f in "$guest/kernel/vmlinux" "$guest/rootfs.ext4"; do
    if [[ ! -f "$f" ]]; then
        echo "missing $f: run images/guest/kernel/build.sh and images/guest/rootfs/build.sh first" >&2
        exit 1
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
    -e WORKSPACE_SIZE="${WORKSPACE_SIZE:-20G}" \
    -e DOCKER_SIZE="${DOCKER_SIZE:-40G}" \
    -e CHLOE_ROOT_MODE="${CHLOE_ROOT_MODE:-ephemeral}" \
    -e CHLOE_GUEST_ENV="${CHLOE_GUEST_ENV:-}" \
    --privileged \
    --device /dev/kvm \
    --device /dev/net/tun \
    -v "$guest:/guest:ro" \
    -v "$state:/state" \
    -v "$root/target/debug/chloe-egress:/opt/chloe/bin/chloe-egress:ro" \
    chloe-dev-runner

#!/usr/bin/env bash
# ゲストのルートファイルシステム (rootfs.ext4) をビルドする。
#
# Dockerfile でビルドしたコンテナを tar に書き出し、mke2fs -d で ext4 にする。
# ホストでのループマウントも root 権限も要らない。
# 成果物は build/guest/rootfs.ext4 に出す。
set -euo pipefail

ROOTFS_SIZE="${ROOTFS_SIZE:-4G}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$root/build/guest"
cache="$root/build/cache"
mkdir -p "$out" "$cache"

cargo build --quiet --release --target x86_64-unknown-linux-musl \
    --manifest-path "$root/Cargo.toml" -p chloe-guest-agent
bin="$cache/bin"
mkdir -p "$bin"
cp "$root/target/x86_64-unknown-linux-musl/release/chloe-guest-agent" "$bin/"

docker build -t chloe-guest-rootfs --build-context chloe-bin="$bin" "$here"
docker build -q -t chloe-guest-tools -f "$here/tools.Dockerfile" "$here" >/dev/null

cid="$(docker create chloe-guest-rootfs)"
trap 'docker rm -f "$cid" >/dev/null' EXIT
docker export "$cid" > "$cache/rootfs.tar"

rm -f "$out/rootfs.ext4"
truncate -s "$ROOTFS_SIZE" "$out/rootfs.ext4"
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$cache:/cache:ro" \
    -v "$out:/out" \
    chloe-guest-tools bash -euo pipefail -c '
        mkfs.ext4 -q -L chloe-root -d /cache/rootfs.tar /out/rootfs.ext4
        e2fsck -fy /out/rootfs.ext4 >/dev/null
        resize2fs -M /out/rootfs.ext4 >/dev/null 2>&1
    '
rm -f "$cache/rootfs.tar"

echo "rootfs: $out/rootfs.ext4 ($(du -h --apparent-size "$out/rootfs.ext4" | cut -f1))"

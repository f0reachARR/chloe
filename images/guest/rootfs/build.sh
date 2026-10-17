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
# shellcheck source=../build-lib.sh
source "$here/../build-lib.sh"
steps 4

step "build chloe-guest-agent (musl)"
cargo build --release --target x86_64-unknown-linux-musl \
    --manifest-path "$root/Cargo.toml" -p chloe-guest-agent
bin="$cache/bin"
mkdir -p "$bin"
cp "$root/target/x86_64-unknown-linux-musl/release/chloe-guest-agent" "$bin/"

release="$(date -u +%Y%m%dT%H%M%SZ)-$(git -C "$root" rev-parse --short HEAD)"
step "build the root filesystem image (Ubuntu, Docker, Claude Code, Codex)"
# 端末でなければ (CI やログに残すとき)、各手順の出力をそのまま出す。
progress=auto
[ -t 2 ] || progress=plain
docker build --progress="$progress" -t chloe-guest-rootfs --build-context chloe-bin="$bin" \
    --build-arg CHLOE_RELEASE="$release" "$here"
docker build -q -t chloe-guest-tools -f "$here/tools.Dockerfile" "$here" >/dev/null

step "export the image"
cid="$(docker create chloe-guest-rootfs)"
trap 'docker rm -f "$cid" >/dev/null' EXIT
docker export "$cid" > "$cache/rootfs.tar"
echo "      $(du -h "$cache/rootfs.tar" | cut -f1) exported" >&2

step "write rootfs.ext4 ($ROOTFS_SIZE, then shrink)"
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

finish
echo "rootfs: $out/rootfs.ext4 ($(du -h --apparent-size "$out/rootfs.ext4" | cut -f1))"

#!/usr/bin/env bash
# Ubuntu の deb を作る。成果物は build/deb/ に出る。
#
#   packaging/deb/build.sh [--guest]
#
# Ubuntu 24.04 のコンテナの中で、ゲートウェイ、chloe-node、chloe-egress をビルドし、
# chloe-common、chloe-gateway、chloe-node を作る。--guest を付けると、build/guest の
# ゲストのイメージから chloe-guest も作る (先に images/guest/kernel/build.sh と
# images/guest/rootfs/build.sh を実行しておく)。
#
# 環境変数 DEB_MAINTAINER で control の Maintainer を変えられる。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/build/deb"
guest_args=()
if [[ "${1:-}" == --guest ]]; then
    for f in "$root/build/guest/kernel/vmlinux" "$root/build/guest/rootfs.ext4"; do
        if [[ ! -f "$f" ]]; then
            echo "missing $f: run images/guest/kernel/build.sh and images/guest/rootfs/build.sh first" >&2
            exit 1
        fi
    done
    guest_args=(--guest /src/build/guest)
fi

# 版は、Cargo のワークスペースの版、コミットの日付、コミットのハッシュから作る。
base="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
version="$base~git$(git -C "$root" log -1 --format=%cd --date=format:%Y%m%d).$(git -C "$root" rev-parse --short HEAD)"
if [[ -n "$(git -C "$root" status --porcelain)" ]]; then
    version="$version+dirty"
fi

context="$(mktemp -d)"
trap 'rm -rf "$context"' EXIT
cp "$here/builder.Dockerfile" "$root/rust-toolchain.toml" "$context/"
docker build -q -t chloe-deb-builder -f "$context/builder.Dockerfile" "$context" > /dev/null

mkdir -p "$out/cache/cargo"
docker run --rm --user "$(id -u):$(id -g)" \
    -v "$root:/src" -w /src \
    -e HOME=/tmp \
    -e CARGO_HOME=/src/build/deb/cache/cargo \
    -e CARGO_TARGET_DIR=/src/build/deb/target \
    -e SOURCE_DATE_EPOCH="$(git -C "$root" log -1 --format=%ct)" \
    -e DEB_MAINTAINER="${DEB_MAINTAINER:-}" \
    chloe-deb-builder bash -c '
        set -e
        cargo build --release --locked -p chloe-gateway -p chloe-node -p chloe-egress
        packaging/deb/package.sh "$0" /src/build/deb/target/release /src/build/deb "${@}"
        lintian --suppress-tags-from-file packaging/deb/lintian-suppress /src/build/deb/*.deb || true
    ' "$version" "${guest_args[@]}"

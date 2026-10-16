#!/usr/bin/env bash
# ゲストカーネル (vmlinux) をビルドする。
#
# Firecracker の検証用設定に chloe.config を重ね、コンテナの中でビルドする。
# 成果物は build/guest/kernel/ に出す。
set -euo pipefail

KERNEL_VERSION="${KERNEL_VERSION:-6.18.57}"
FIRECRACKER_VERSION="${FIRECRACKER_VERSION:-v1.17.0}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$root/build/guest/kernel"
cache="$root/build/cache"
mkdir -p "$out" "$cache"

major="${KERNEL_VERSION%%.*}"
tarball="linux-$KERNEL_VERSION.tar.xz"
base_config="microvm-kernel-ci-x86_64-${KERNEL_VERSION%.*}.config"

if [[ ! -f "$cache/$tarball" ]]; then
    curl -fsSL --output "$cache/$tarball.part" \
        "https://cdn.kernel.org/pub/linux/kernel/v$major.x/$tarball"
    curl -fsSL --output "$cache/sha256sums.asc" \
        "https://cdn.kernel.org/pub/linux/kernel/v$major.x/sha256sums.asc"
    expected="$(awk -v f="$tarball" '$2 == f { print $1 }' "$cache/sha256sums.asc")"
    actual="$(sha256sum "$cache/$tarball.part" | cut -d' ' -f1)"
    if [[ -z "$expected" || "$expected" != "$actual" ]]; then
        echo "checksum mismatch for $tarball" >&2
        exit 1
    fi
    mv "$cache/$tarball.part" "$cache/$tarball"
fi

curl -fsSL --output "$cache/$base_config" \
    "https://raw.githubusercontent.com/firecracker-microvm/firecracker/$FIRECRACKER_VERSION/resources/guest_configs/$base_config"
curl -fsSL --output "$cache/check-config.sh" \
    "https://raw.githubusercontent.com/moby/moby/master/contrib/check-config.sh"

docker build -q -t chloe-kernel-builder -f "$here/builder.Dockerfile" "$here" >/dev/null

docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$cache:/cache:ro" \
    -v "$here:/chloe:ro" \
    -v "$out:/out" \
    -e KERNEL_VERSION="$KERNEL_VERSION" \
    -e BASE_CONFIG="$base_config" \
    chloe-kernel-builder bash -euo pipefail -c '
        work=/tmp/build
        mkdir -p "$work"
        tar -C "$work" -xf "/cache/linux-$KERNEL_VERSION.tar.xz"
        cd "$work/linux-$KERNEL_VERSION"

        cp "/cache/$BASE_CONFIG" .config
        scripts/kconfig/merge_config.sh -m .config /chloe/chloe.config >/dev/null
        make olddefconfig >/dev/null

        # chloe.config の指定が依存関係で落とされていないかを確かめる。
        missing=0
        while IFS= read -r line; do
            [[ "$line" =~ ^CONFIG_ ]] || continue
            if ! grep -qxF "$line" .config; then
                echo "not applied: $line" >&2
                missing=1
            fi
        done < /chloe/chloe.config
        [[ "$missing" == 0 ]]

        make -j"$(nproc)" vmlinux >/dev/null
        cp vmlinux /out/vmlinux
        cp .config /out/config
    '

bash "$cache/check-config.sh" "$out/config" > "$out/check-config.txt" || true
echo "kernel: $out/vmlinux"
echo "docker check: $out/check-config.txt"

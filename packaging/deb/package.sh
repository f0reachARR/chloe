#!/bin/bash
# deb を組み立てる (build.sh が、ビルド用のコンテナの中でバイナリをビルドしてから呼ぶ)。
#
#   package.sh <版> <バイナリのディレクトリ> <出力先> [--guest <ゲストのイメージのディレクトリ>]
#
# Web の画面は、先に <出力先>/web にビルドしておく (build.sh が pnpm build を実行する)。
#
# deploy/ の unit と設定の例は、手で置くときの配置 (/usr/local) で書いてあるので、パスを
# パッケージの配置に書き換えて入れる。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
version="$1"
bin="$2"
out="$3"
guest=""
if [[ "${4:-}" == --guest ]]; then
    guest="$5"
fi
maintainer="${DEB_MAINTAINER:-chloe <chloe@localhost>}"
maintainer="${maintainer:-chloe <chloe@localhost>}"
cache="$out/cache"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$cache"
rm -f "$out"/*.deb

# 手で置くときのパスを、パッケージのパスに書き換えて写す。
deb_paths() {
    install -D -m "$3" /dev/null "$2"
    sed -e 's|/usr/local/bin/|/usr/bin/|g' \
        -e 's|/usr/local/lib/chloe/runner/|/usr/libexec/chloe/|g' \
        -e 's|/usr/local/lib/chloe/chloe-egress|/usr/libexec/chloe/chloe-egress|g' \
        -e 's|/usr/local/lib/chloe/guest|/usr/lib/chloe/guest|g' \
        -e 's|/usr/local/share/chloe/web|/usr/share/chloe/web|g' \
        "$1" > "$2"
}

# バイナリを写して、シンボルを落とす。
install_bin() {
    install -D -m 755 "$1" "$2"
    strip --strip-unneeded "$2"
}

# バイナリが使う共有ライブラリのパッケージ (control の Depends に入れる)。
shlibs() {
    local d="$work/shlibs"
    mkdir -p "$d/debian"
    printf 'Source: chloe\n\nPackage: chloe\nArchitecture: amd64\n' > "$d/debian/control"
    (cd "$d" && dpkg-shlibdeps -O "$@" 2> /dev/null) | sed -n 's/^shlibs:Depends=//p'
}

# Firecracker は公式のリリースのバイナリを使う。版はランナーのコンテナと合わせる。
firecracker() {
    local v tgz base
    v="$(sed -n 's/^ARG FIRECRACKER_VERSION=//p' "$root/images/guest/dev/runner.Dockerfile")"
    tgz="firecracker-$v-x86_64.tgz"
    base="https://github.com/firecracker-microvm/firecracker/releases/download/$v"
    if [[ ! -f "$cache/firecracker-$v" ]]; then
        (
            cd "$work"
            curl -fsSLO "$base/$tgz"
            curl -fsSLO "$base/$tgz.sha256.txt"
            sha256sum -c "$tgz.sha256.txt" > /dev/null
            tar -xzf "$tgz"
            cp "release-$v-x86_64/firecracker-$v-x86_64" "$cache/firecracker-$v"
        )
    fi
    install -D -m 755 "$cache/firecracker-$v" "$1"
    strip --strip-unneeded "$1"
}

# copyright と changelog を置く。
docs() {
    local name="$1" tree="$2" doc="$2/usr/share/doc/$1"
    mkdir -p "$doc"
    cat > "$doc/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: chloe
Source: https://github.com/ForteFibre/chloe

Files: *
Copyright: 2026 chloe contributors
License: MIT or Apache-2.0
 On Debian systems, the full text of the Apache License 2.0 can be found
 in /usr/share/common-licenses/Apache-2.0.
EOF
    if [[ -f "$tree/usr/libexec/chloe/firecracker" ]]; then
        cat >> "$doc/copyright" <<EOF

Files: usr/libexec/chloe/firecracker
Copyright: Amazon.com, Inc. or its affiliates
License: Apache-2.0
 On Debian systems, the full text of the Apache License 2.0 can be found
 in /usr/share/common-licenses/Apache-2.0.
EOF
    fi
    printf '%s (%s) unstable; urgency=medium\n\n  * Build from https://github.com/ForteFibre/chloe.\n\n -- %s  %s\n' \
        "$name" "$version" "$maintainer" "$(date -R -d "@${SOURCE_DATE_EPOCH:-$(date +%s)}")" |
        gzip -9n > "$doc/changelog.gz"
}

# パッケージを作る。引数: 名前、control と maintainer script のディレクトリ、中身、共有ライブラリの依存
build_deb() {
    local name="$1" src="$2" tree="$3" deps="$4"
    docs "$name" "$tree"
    if [[ -f "$src/lintian-overrides" ]]; then
        install -D -m 644 "$src/lintian-overrides" "$tree/usr/share/lintian/overrides/$name"
    fi
    mkdir -p "$tree/DEBIAN"
    local size
    size="$(du -sk --exclude=DEBIAN "$tree" | cut -f1)"
    sed -e "s|@VERSION@|$version|g" -e "s|@MAINTAINER@|$maintainer|g" -e "s|@SIZE@|$size|g" \
        -e "s|@SHLIBS@|$deps|g" "$src/control" > "$tree/DEBIAN/control"
    for script in postinst prerm postrm; do
        if [[ -f "$src/$script" ]]; then
            install -m 755 "$src/$script" "$tree/DEBIAN/$script"
        fi
    done
    if [[ -d "$tree/etc" ]]; then
        (cd "$tree" && find etc -type f | sed 's|^|/|' | sort) > "$tree/DEBIAN/conffiles"
    fi
    (cd "$tree" && find . -path ./DEBIAN -prune -o -type f -print0 | sort -z | xargs -0 md5sum | sed 's|  \./|  |') \
        > "$tree/DEBIAN/md5sums"
    dpkg-deb --root-owner-group -Zzstd --build "$tree" "$out/${name}_${version}_amd64.deb" > /dev/null
    echo "built $out/${name}_${version}_amd64.deb"
}

# chloe-common
t="$work/common"
install_bin "$bin/chloe-egress" "$t/usr/libexec/chloe/chloe-egress"
firecracker "$t/usr/libexec/chloe/firecracker"
for f in "$root"/images/guest/runner/*; do
    install -m 755 "$f" "$t/usr/libexec/chloe/"
done
for unit in chloe-host-net.service chloe-netns@.service chloe-runner@.service; do
    deb_paths "$root/deploy/systemd/$unit" "$t/usr/lib/systemd/system/$unit" 644
done
install -D -m 644 "$root/deploy/polkit/50-chloe.rules" "$t/usr/share/polkit-1/rules.d/50-chloe.rules"
install -D -m 644 "$root/deploy/tmpfiles/chloe.conf" "$t/usr/lib/tmpfiles.d/chloe.conf"
install -D -m 644 "$here/common/sysusers.conf" "$t/usr/lib/sysusers.d/chloe.conf"
deb_paths "$root/deploy/runner.env" "$t/etc/chloe/runner.env" 644
build_deb chloe-common "$here/common" "$t" "$(shlibs "$t/usr/libexec/chloe/chloe-egress" "$t/usr/libexec/chloe/firecracker")"

# chloe-gateway
t="$work/gateway"
install_bin "$bin/chloe-gateway" "$t/usr/bin/chloe-gateway"
deb_paths "$root/deploy/systemd/chloe-gateway.service" "$t/usr/lib/systemd/system/chloe-gateway.service" 644
deb_paths "$root/deploy/gateway.toml" "$t/etc/chloe/gateway.toml" 644
# Web の画面 (`[api] web_root`)。
mkdir -p "$t/usr/share/chloe/web"
cp -r "$out/web/." "$t/usr/share/chloe/web/"
find "$t/usr/share/chloe/web" -type d -exec chmod 755 {} + -o -type f -exec chmod 644 {} +
for f in "$root"/deploy/secrets/*.toml; do
    install -D -m 644 "$f" "$t/usr/share/doc/chloe-gateway/examples/secrets/${f##*/}"
done
build_deb chloe-gateway "$here/gateway" "$t" "$(shlibs "$t/usr/bin/chloe-gateway")"

# chloe-node
t="$work/node"
install_bin "$bin/chloe-node" "$t/usr/bin/chloe-node"
deb_paths "$root/deploy/systemd/chloe-node.service" "$t/usr/lib/systemd/system/chloe-node.service" 644
deb_paths "$root/deploy/node.toml" "$t/etc/chloe/node.toml" 644
build_deb chloe-node "$here/node" "$t" "$(shlibs "$t/usr/bin/chloe-node")"

# chloe-guest
if [[ -n "$guest" ]]; then
    t="$work/guest"
    install -D -m 644 "$guest/kernel/vmlinux" "$t/usr/lib/chloe/guest/kernel/vmlinux"
    install -D -m 644 "$guest/rootfs.ext4" "$t/usr/lib/chloe/guest/rootfs.ext4"
    build_deb chloe-guest "$here/guest" "$t" ""
fi

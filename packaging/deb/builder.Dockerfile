# deb を作る環境。Ubuntu 24.04 の glibc (2.39) でビルドし、24.04 と 26.04 で動かす。
FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang cmake curl dpkg-dev fakeroot file git libclang-dev \
        lintian pkg-config xz-utils zlib1g-dev zstd \
    && rm -rf /var/lib/apt/lists/*

# ツールチェーンはリポジトリの rust-toolchain.toml に合わせて、イメージを作るときに入れておく
# (ビルドはホストの利用者で動かすので、あとから入れられない)。
ENV RUSTUP_HOME=/usr/local/rustup PATH=/usr/local/cargo/bin:$PATH
COPY rust-toolchain.toml /tmp/toolchain/
RUN export CARGO_HOME=/usr/local/cargo \
    && curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
    && cd /tmp/toolchain && rustup toolchain install && rustup show \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo \
    && rm -rf /tmp/toolchain

# Web の画面のビルドに使う Node.js と pnpm。版は web/package.json の packageManager と合わせる。
ARG NODE_VERSION=24.22.1
ARG PNPM_VERSION=10.33.2
RUN curl -fsSLO "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-x64.tar.xz" \
    && curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/SHASUMS256.txt" \
        | grep " node-v$NODE_VERSION-linux-x64.tar.xz\$" | sha256sum -c - \
    && tar -xJf "node-v$NODE_VERSION-linux-x64.tar.xz" -C /usr/local --strip-components=1 \
    && rm "node-v$NODE_VERSION-linux-x64.tar.xz" \
    && npm install -g "pnpm@$PNPM_VERSION" \
    && node --version && pnpm --version

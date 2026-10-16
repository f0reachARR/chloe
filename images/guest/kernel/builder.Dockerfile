# ゲストカーネルをビルドするための環境。
FROM ubuntu:26.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bc bison build-essential ca-certificates cpio flex gawk kmod \
        libelf-dev libssl-dev python3 xz-utils \
    && rm -rf /var/lib/apt/lists/*

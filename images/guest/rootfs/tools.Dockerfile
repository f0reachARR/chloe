# ディスクイメージを作るための環境。mke2fs -d で tar を読むために libarchive が要る。
FROM ubuntu:26.04

ENV LANG=C.UTF-8

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        e2fsprogs libarchive13t64 \
    && rm -rf /var/lib/apt/lists/*

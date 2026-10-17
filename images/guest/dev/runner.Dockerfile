# 開発用に、特権コンテナの中で Firecracker と chloe-egress を動かす環境。
# ランナーの Pod と同じく、コンテナのネットワーク namespace の中に tap と nftables を作る。
FROM ubuntu:26.04

ARG FIRECRACKER_VERSION=v1.17.0

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl dnsmasq-base e2fsprogs iproute2 jq nftables \
    && rm -rf /var/lib/apt/lists/*

RUN cd /tmp \
    && tgz="firecracker-$FIRECRACKER_VERSION-x86_64.tgz" \
    && base="https://github.com/firecracker-microvm/firecracker/releases/download/$FIRECRACKER_VERSION" \
    && curl -fsSLO "$base/$tgz" \
    && curl -fsSLO "$base/$tgz.sha256.txt" \
    && sha256sum -c "$tgz.sha256.txt" \
    && tar -xzf "$tgz" \
    && install -m 0755 "release-$FIRECRACKER_VERSION-x86_64/firecracker-$FIRECRACKER_VERSION-x86_64" /usr/local/bin/firecracker \
    && install -m 0755 "release-$FIRECRACKER_VERSION-x86_64/jailer-$FIRECRACKER_VERSION-x86_64" /usr/local/bin/jailer \
    && rm -rf /tmp/*

COPY entrypoint.sh dns-update.sh /opt/chloe/
COPY policy.dev.json /opt/chloe/
ENTRYPOINT ["/opt/chloe/entrypoint.sh"]

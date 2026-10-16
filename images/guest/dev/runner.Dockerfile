# 段階 1 の確認用に、特権コンテナの中で Firecracker を動かす環境。
# ランナーの Pod と同じく、コンテナのネットワーク namespace の中に tap と nftables を作る。
FROM ubuntu:26.04

ARG FIRECRACKER_VERSION=v1.17.0
ARG MITMPROXY_VERSION=12.2.3

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl dnsmasq-base iproute2 nftables python3-venv \
    && rm -rf /var/lib/apt/lists/*

# 段階 2 の検証用プロキシ。段階 4 で chloe-egress に置き換える。
RUN python3 -m venv /opt/mitmproxy \
    && /opt/mitmproxy/bin/pip install --no-cache-dir "mitmproxy==$MITMPROXY_VERSION"

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

COPY entrypoint.sh /opt/chloe/
COPY proxy/ /opt/chloe/proxy/
ENTRYPOINT ["/opt/chloe/entrypoint.sh"]

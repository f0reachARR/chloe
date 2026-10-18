# systemd で動かすランナーを確かめるための「ホスト」。特権コンテナの中で systemd を PID 1 にして動かす。
# Docker は入れない。chloe のバイナリ、スクリプト、unit は、確認のスクリプトが bind mount で渡す。
FROM chloe-dev-runner AS runner

FROM ubuntu:24.04
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl dbus dnsmasq-base e2fsprogs iproute2 jq nftables polkitd procps python3 \
        systemd systemd-sysv \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -f -r kvm \
    && useradd --system --no-create-home --home-dir /var/lib/chloe --shell /usr/sbin/nologin --groups kvm chloe
COPY --from=runner /usr/local/bin/firecracker /usr/local/bin/firecracker
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]

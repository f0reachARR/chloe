# deb を入れて確かめるための「ホスト」。特権コンテナの中で systemd を PID 1 にして動かす。
# chloe の依存は入れない (deb を apt で入れるときに入る)。
ARG UBUNTU=24.04
FROM ubuntu:${UBUNTU}
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl dbus procps python3 systemd systemd-sysv \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /usr/sbin/policy-rc.d
# 公式のイメージはサービスの操作を断る policy-rc.d を入れている。実際のホストに合わせて消す。
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]

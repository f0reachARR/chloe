# ゲストのルートファイルシステム。
# VM には読み取り専用で付け、起動時に tmpfs を重ねる (overlay-init)。

# mise (ツールの版の管理)。リリースの署名を確かめてから取り出す。
FROM ubuntu:26.04 AS mise
ARG MISE_VERSION=2026.10.4
ARG MISE_MINISIGN_KEY=RWTC3g8W3z4RZK3V3qv7fa1QY4JEWyBtqIHW+85QlJpZc5yG+uNYNBSZ
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl minisign \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    base="https://github.com/jdx/mise/releases/download/v${MISE_VERSION}"; \
    asset="mise-v${MISE_VERSION}-linux-x64"; \
    cd /tmp; \
    curl -fsSLO "$base/SHASUMS256.txt"; \
    curl -fsSLO "$base/SHASUMS256.txt.minisig"; \
    curl -fsSLO "$base/$asset"; \
    minisign -Vm SHASUMS256.txt -P "$MISE_MINISIGN_KEY"; \
    grep " ./$asset\$" SHASUMS256.txt | sha256sum -c --strict; \
    install -m 755 "$asset" /mise

FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        systemd systemd-sysv systemd-resolved dbus udev kmod \
        iproute2 iptables nftables iputils-ping \
        ca-certificates curl gnupg jq zstd \
        git openssh-client tmux less vim-tiny sudo locales bash-completion python3 \
        bubblewrap e2fsprogs util-linux-extra \
    && rm -rf /var/lib/apt/lists/*

# Docker の公式パッケージ
RUN install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu resolute stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# コーディングエージェントなどのツールは、利用者が mise で入れる (ホームディレクトリに入り、
# ワークスペースのディスクに残る)。更新は mise の版を上げてイメージを作り直す。
COPY --from=mise /mise /usr/local/bin/mise

RUN locale-gen en_US.UTF-8 \
    && useradd --create-home --shell /bin/bash --groups docker,sudo user \
    && echo 'user ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/user \
    && mkdir -p /workspace /var/lib/chloe \
    && chown user:user /workspace

COPY files/ /
# guest-agent は build.sh が musl でビルドし、chloe-bin のビルドコンテキストとして渡す。
COPY --from=chloe-bin chloe-guest-agent /usr/local/sbin/chloe-guest-agent
RUN chmod 0755 /usr/sbin/overlay-init /usr/local/sbin/chloe-mount-disks /usr/local/sbin/chloe-load-config /usr/local/bin/chloe-activity \
    && systemctl enable systemd-networkd systemd-resolved docker \
        chloe-disks.service chloe-config.service chloe-guest-agent.service \
    && systemctl mask systemd-firstboot.service systemd-timesyncd.service

# イメージの版。ルートの変更を残すワークスペースで、共通イメージが変わったことを検出するのに使う。
ARG CHLOE_RELEASE=dev
RUN mkdir -p /etc/chloe && echo "$CHLOE_RELEASE" > /etc/chloe/release

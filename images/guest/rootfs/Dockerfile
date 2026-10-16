# ゲストのルートファイルシステム。
# VM には読み取り専用で付け、起動時に tmpfs を重ねる (overlay-init)。
FROM ubuntu:26.04

ARG CLAUDE_CODE_CHANNEL=stable
ARG CODEX_VERSION=rust-v0.163.0

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        systemd systemd-sysv systemd-resolved dbus udev kmod \
        iproute2 iptables nftables iputils-ping \
        ca-certificates curl gnupg jq zstd \
        git openssh-client tmux less vim-tiny sudo locales bash-completion \
        bubblewrap e2fsprogs \
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

# Claude Code (ネイティブバイナリ)。更新はイメージの作り直しで行う。
RUN base=https://downloads.claude.ai/claude-code-releases \
    && version="$(curl -fsSL "$base/$CLAUDE_CODE_CHANNEL")" \
    && checksum="$(curl -fsSL "$base/$version/manifest.json" | jq -r '.platforms["linux-x64"].checksum')" \
    && curl -fsSL -o /usr/local/bin/claude "$base/$version/linux-x64/claude" \
    && echo "$checksum  /usr/local/bin/claude" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/claude

# Codex
RUN curl -fsSL "https://github.com/openai/codex/releases/download/$CODEX_VERSION/codex-x86_64-unknown-linux-musl.tar.gz" \
        | tar -xz -C /usr/local/bin \
    && mv /usr/local/bin/codex-x86_64-unknown-linux-musl /usr/local/bin/codex \
    && chmod 0755 /usr/local/bin/codex

RUN locale-gen en_US.UTF-8 \
    && useradd --create-home --shell /bin/bash --groups docker,sudo user \
    && echo 'user ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/user \
    && mkdir -p /workspace /var/lib/chloe/workspace \
    && chown user:user /workspace

COPY files/ /
RUN chmod 0755 /usr/sbin/overlay-init /usr/local/sbin/chloe-mount-workspace \
    && systemctl enable systemd-networkd systemd-resolved docker chloe-workspace.service \
    && systemctl mask systemd-firstboot.service systemd-timesyncd.service

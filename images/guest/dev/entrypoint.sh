#!/bin/sh
# コンテナのネットワーク namespace に tap0 を作り、VM の通信の出口を用意してから Firecracker を起動する。
#
# CHLOE_NET_MODE で出口の作り方を選ぶ。
#   nat   : VM からの通信をそのまま NAT する (段階 1)。
#   proxy : 80 番と 443 番を chloe-egress へ転送し、それ以外を遮断する。
# どちらのモードでも、VM の DNS はこのコンテナの dnsmasq (172.29.0.1) が答える。
set -eu

CHLOE_NET_MODE="${CHLOE_NET_MODE:-nat}"
VCPUS="${VCPUS:-4}"
MEM_MIB="${MEM_MIB:-4096}"
logs=/state/logs
mkdir -p "$logs"

ip tuntap add tap0 mode tap
ip addr add 172.29.0.1/30 dev tap0
ip link set tap0 up

uplink="$(ip -o route show default | awk '{ print $5 }')"
upstream_dns="$(awk '/^nameserver/ { print $2; exit }' /etc/resolv.conf)"

case "$CHLOE_NET_MODE" in
nat)
    nft -f - <<EOF
table inet chloe {
    chain prerouting {
        type nat hook prerouting priority dstnat;
        iifname "tap0" meta l4proto { tcp, udp } th dport 53 dnat ip to 172.29.0.1:53
    }
    chain forward {
        type filter hook forward priority 0; policy drop;
        iifname "tap0" oifname "$uplink" accept
        iifname "$uplink" oifname "tap0" ct state established,related accept
    }
    chain postrouting {
        type nat hook postrouting priority srcnat;
        iifname "tap0" oifname "$uplink" masquerade
    }
}
EOF
    echo 1 > /proc/sys/net/ipv4/ip_forward
    ;;
proxy)
    http_port=8080 https_port=8443
    nft -f - <<EOF
table inet chloe {
    chain prerouting {
        type nat hook prerouting priority dstnat;
        iifname "tap0" meta l4proto { tcp, udp } th dport 53 dnat ip to 172.29.0.1:53
        iifname "tap0" tcp dport 80 redirect to :$http_port
        iifname "tap0" tcp dport 443 redirect to :$https_port
    }
    chain input {
        type filter hook input priority 0; policy accept;
        iifname "tap0" ct state established,related accept
        iifname "tap0" meta l4proto { tcp, udp } th dport 53 accept
        iifname "tap0" tcp dport { $http_port, $https_port } accept
        iifname "tap0" counter drop
    }
    chain forward {
        type filter hook forward priority 0; policy drop;
        iifname "tap0" counter drop
    }
}
EOF
    echo 0 > /proc/sys/net/ipv4/ip_forward

    # 検証用の偽の秘密情報。本物の API キーは使わない。
    export CHLOE_SECRET_TEST="${CHLOE_SECRET_TEST:-real-secret-for-test}"
    export CHLOE_SECRET_ANTHROPIC="${CHLOE_SECRET_ANTHROPIC:-sk-ant-api03-not-a-real-key}"
    export CHLOE_SECRET_OPENAI="${CHLOE_SECRET_OPENAI:-sk-proj-not-a-real-key}"

    /opt/chloe/bin/chloe-egress \
        --http-listen "0.0.0.0:$http_port" \
        --https-listen "0.0.0.0:$https_port" \
        --policy "${CHLOE_POLICY:-/opt/chloe/policy.dev.json}" \
        --ca-dir /state/egress \
        --log "$logs/requests.jsonl" \
        > "$logs/egress.log" 2>&1 &
    proxy_pid=$!
    # 待ち受けを始めるまで待つ。設定の読み込みに失敗するとプロキシは終了する。
    for _ in $(seq 100); do
        grep -qi "listening" "$logs/egress.log" 2>/dev/null && break
        sleep 0.1
    done
    sleep 0.5
    if ! kill -0 "$proxy_pid" 2>/dev/null; then
        echo "chloe-egress failed to start:" >&2
        cat "$logs/egress.log" >&2
        exit 1
    fi
    ;;
*)
    echo "unknown CHLOE_NET_MODE: $CHLOE_NET_MODE" >&2
    exit 1
    ;;
esac

dnsmasq \
    --keep-in-foreground --no-resolv --no-hosts \
    --listen-address=172.29.0.1 --bind-interfaces \
    --server="$upstream_dns" \
    --address="/ca.chloe.internal/172.29.0.1" \
    --log-facility="$logs/dnsmasq.log" --log-queries &

# 確認用のドライブ (/dev/vdd) があれば読み取り専用で付ける。
extra_drive=""
if [ -f /state/test.ext4 ]; then
    extra_drive=',
    { "drive_id": "test", "path_on_host": "/state/test.ext4", "is_root_device": false, "is_read_only": true }'
fi

cat > /tmp/vm.json <<EOF
{
  "boot-source": {
    "kernel_image_path": "/guest/kernel/vmlinux",
    "boot_args": "console=ttyS0 reboot=k panic=1 init=/sbin/overlay-init"
  },
  "drives": [
    { "drive_id": "rootfs", "path_on_host": "/guest/rootfs.ext4", "is_root_device": true, "is_read_only": true },
    { "drive_id": "workspace", "path_on_host": "/state/workspace.ext4", "is_root_device": false, "is_read_only": false },
    { "drive_id": "docker", "path_on_host": "/state/docker.ext4", "is_root_device": false, "is_read_only": false }$extra_drive
  ],
  "machine-config": { "vcpu_count": $VCPUS, "mem_size_mib": $MEM_MIB },
  "network-interfaces": [
    { "iface_id": "eth0", "guest_mac": "06:00:ac:1d:00:02", "host_dev_name": "tap0" }
  ],
  "vsock": { "guest_cid": 3, "uds_path": "/state/vsock.sock" }
}
EOF

rm -f /state/vsock.sock
# 開発用: ホストの利用者が chloe-runner で vsock につなげるようにする。
(
    while [ ! -S /state/vsock.sock ]; do sleep 0.1; done
    chmod 0666 /state/vsock.sock
) &
exec firecracker --no-api --config-file /tmp/vm.json

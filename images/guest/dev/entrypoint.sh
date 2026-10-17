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
# 状態のディレクトリに書くファイルは、ディレクトリの持ち主 (ホストの利用者) のものにする。
# root のファイルが残ると、ホストの利用者が消せなくなる。
owner="$(stat -c %u:%g /state)"
as_owner() { setpriv --reuid="${owner%%:*}" --regid="${owner##*:}" --clear-groups "$@"; }
mkdir -p "$logs" /state/egress
chown "$owner" "$logs" /state/egress
for f in requests.jsonl egress.log dnsmasq.log; do
    touch "$logs/$f"
    chown "$owner" "$logs/$f"
done

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

    # コントロールプレーンの問い合わせ用ソケットがあれば、本物の値はそこへ問い合わせる。
    # なければ (run.sh で直接起動したとき) 開発用に環境変数から読む。
    broker_arg=""
    if [ -S /state/broker.sock ]; then
        broker_arg="--broker /state/broker.sock"
    fi
    # shellcheck disable=SC2086
    as_owner /opt/chloe/bin/chloe-egress $broker_arg \
        --http-listen "0.0.0.0:$http_port" \
        --https-listen "0.0.0.0:$https_port" \
        --policy "${CHLOE_POLICY:-/opt/chloe/policy.dev.json}" \
        --ca-dir /state/egress \
        --log "$logs/requests.jsonl" \
        --control /state/egress.sock \
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

# proxy モードでは、許可リストにある名前だけを上流に問い合わせ、それ以外は NXDOMAIN を返す。
# DNS の問い合わせ名に情報を載せて持ち出す経路を塞ぐためである。
# 上流の一覧は --servers-file に書き、方針が変わったら dns-update.sh で書き直して読み直させる。
echo "$upstream_dns" > /run/chloe-upstream-dns
dns_servers="--server=$upstream_dns"
if [ "$CHLOE_NET_MODE" = proxy ]; then
    /opt/chloe/dns-update.sh "${CHLOE_POLICY:-/opt/chloe/policy.dev.json}" --no-reload
    dns_servers="--address=/#/ --servers-file=/run/dnsmasq-servers.conf"
fi
# shellcheck disable=SC2086
dnsmasq \
    --keep-in-foreground --no-resolv --no-hosts \
    --listen-address=172.29.0.1 --bind-interfaces \
    $dns_servers \
    --address="/ca.chloe.internal/172.29.0.1" \
    --log-facility="$logs/dnsmasq.log" --log-queries &
echo $! > /run/dnsmasq.pid

# ディスクの大きさ (20G、512M など) をバイト数にする。
to_bytes() {
    case "$1" in
    *G) echo $((${1%G} * 1024 * 1024 * 1024)) ;;
    *M) echo $((${1%M} * 1024 * 1024)) ;;
    *) echo "$1" ;;
    esac
}

# ext4 のイメージのファイルシステムの大きさ (バイト)。
fs_bytes() {
    dumpe2fs -h "$1" 2>/dev/null | {
        count=0 block=0
        while IFS=: read -r key value; do
            case "$key" in
            "Block count") count=$((value)) ;;
            "Block size") block=$((value)) ;;
            esac
        done
        echo $((count * block))
    }
}

# ワークスペースと Docker のディスクがなければ作り、指定より小さければ広げる (縮めはしない)。
# 持ち主は状態のディレクトリに合わせる。
for pair in "workspace:${WORKSPACE_SIZE:-20G}" "docker:${DOCKER_SIZE:-40G}"; do
    name="${pair%%:*}" size="${pair#*:}" img="/state/${pair%%:*}.ext4"
    if [ ! -f "$img" ]; then
        truncate -s "$size" "$img"
        mkfs.ext4 -q -L "chloe-$name" "$img"
        chown --reference=/state "$img"
    elif [ "$(fs_bytes "$img")" -lt "$(to_bytes "$size")" ]; then
        echo "growing the $name disk to $size" >&2
        # 広げる前に検査する。直せない誤りが残るなら (終了コード 4 以上)、壊さないよう起動しない。
        rc=0
        e2fsck -f -p "$img" >&2 || rc=$?
        if [ "$rc" -ge 4 ]; then
            echo "e2fsck could not fix the $name disk (exit code $rc); not starting" >&2
            exit 1
        fi
        # ファイルがすでに大きい (前に広げかけて止まった) ときは縮めない。
        if [ "$(stat -c %s "$img")" -lt "$(to_bytes "$size")" ]; then
            truncate -s "$size" "$img"
        fi
        resize2fs "$img" "$(($(to_bytes "$size") / 1024))K" >&2
    fi
done

# 設定用のドライブ (/dev/vdd)。ゲストの環境変数 (プレースホルダー) と CA 証明書を渡す。
config_dir="$(mktemp -d)"
if [ -n "${CHLOE_GUEST_ENV:-}" ]; then
    cp "$CHLOE_GUEST_ENV" "$config_dir/guest.env"
fi
if [ -f /state/egress/ca.pem ]; then
    cp /state/egress/ca.pem "$config_dir/ca.pem"
fi
rm -f /tmp/config.ext4
truncate -s 4M /tmp/config.ext4
mkfs.ext4 -q -L chloe-config -d "$config_dir" /tmp/config.ext4

# 確認用のドライブ (/dev/vde) があれば読み取り専用で付ける。
extra_drive=""
if [ -f /state/test.ext4 ]; then
    extra_drive=',
    { "drive_id": "test", "path_on_host": "/state/test.ext4", "is_root_device": false, "is_read_only": true }'
fi

# ルートの変更をワークスペースのディスクに残すかどうか (overlay-init が読む)。
root_arg=""
if [ "${CHLOE_ROOT_MODE:-ephemeral}" = persistent ]; then
    root_arg=" chloe.root=persistent"
fi

cat > /tmp/vm.json <<EOF
{
  "boot-source": {
    "kernel_image_path": "/guest/kernel/vmlinux",
    "boot_args": "console=ttyS0 reboot=k panic=1 init=/sbin/overlay-init$root_arg"
  },
  "drives": [
    { "drive_id": "rootfs", "path_on_host": "/guest/rootfs.ext4", "is_root_device": true, "is_read_only": true },
    { "drive_id": "workspace", "path_on_host": "/state/workspace.ext4", "is_root_device": false, "is_read_only": false },
    { "drive_id": "docker", "path_on_host": "/state/docker.ext4", "is_root_device": false, "is_read_only": false },
    { "drive_id": "config", "path_on_host": "/tmp/config.ext4", "is_root_device": false, "is_read_only": true }$extra_drive
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

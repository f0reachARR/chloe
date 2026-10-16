#!/bin/sh
# コンテナのネットワーク namespace に tap0 を作り、VM からの通信をコンテナの外へ NAT する。
# 段階 1 では宛先を絞らない。段階 2 でプロキシへの転送と遮断に置き換える。
set -eu

VCPUS="${VCPUS:-4}"
MEM_MIB="${MEM_MIB:-4096}"

ip tuntap add tap0 mode tap
ip addr add 10.0.0.1/30 dev tap0
ip link set tap0 up

uplink="$(ip -o route show default | awk '{ print $5 }')"
nft -f - <<EOF
table ip chloe {
    chain forward {
        type filter hook forward priority 0; policy drop;
        iifname "tap0" oifname "$uplink" accept
        iifname "$uplink" oifname "tap0" ct state established,related accept
    }
    chain postrouting {
        type nat hook postrouting priority 100;
        iifname "tap0" oifname "$uplink" masquerade
    }
}
EOF
echo 1 > /proc/sys/net/ipv4/ip_forward

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
    { "iface_id": "eth0", "guest_mac": "06:00:0a:00:00:02", "host_dev_name": "tap0" }
  ],
  "vsock": { "guest_cid": 3, "uds_path": "/state/vsock.sock" }
}
EOF

rm -f /state/vsock.sock
exec firecracker --no-api --config-file /tmp/vm.json

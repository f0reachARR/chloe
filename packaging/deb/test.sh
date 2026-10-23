#!/usr/bin/env bash
# 段階 12 の確認。build/deb の deb を、systemd を PID 1 にした特権コンテナ (Ubuntu) に apt で入れて確かめる。
#
#   packaging/deb/test.sh [Ubuntu の版 (24.04)]
#
# 先に packaging/deb/build.sh で deb を作っておく。ゲストのイメージは deb (chloe-guest) にせず、
# build/guest を /usr/lib/chloe/guest に bind mount して使う。
# 新しく入れる、ランナーを動かす、版を上げて入れ直す (動いているワークスペースが止まらない)、
# 消す、purge する、の順に確かめる。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
ubuntu="${1:-24.04}"
state="$root/build/deb/test-$ubuntu"
logs="$state/logs"
host="chloe-deb-test-$ubuntu"
keys_port=18951
work="$(mktemp -d)"

in_host() { docker exec "$host" "$@"; }
cleanup() {
    kill "${hold:-}" 2>/dev/null || true
    in_host journalctl --no-pager > "$logs/journal.log" 2>&1 || true
    docker rm -f "$host" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=../../images/guest/dev/gateway-lib.sh
source "$root/images/guest/dev/gateway-lib.sh"

debs=("$root"/build/deb/chloe-{common,gateway,node}_*_amd64.deb)
for f in "${debs[@]}"; do
    [[ -f "$f" ]] || { echo "missing $f: run packaging/deb/build.sh first" >&2; exit 1; }
done
version="$(dpkg-deb -f "${debs[0]}" Version)"

rm -rf "$state"
mkdir -p "$logs" "$work/debs" "$work/upgrade" "$work/keys"
cp "${debs[@]}" "$work/debs/"
ssh-keygen -q -t ed25519 -N '' -C alice -f "$work/alice"
cp "$work/alice.pub" "$work/keys/alice.keys"

# 版を上げた deb (中身は同じ)。更新の確認に使う。
upgrade="$version.1"
for f in "${debs[@]}"; do
    x="$work/x"
    rm -rf "$x"
    dpkg-deb -R "$f" "$x"
    sed -i -e "s/^Version: .*/Version: $upgrade/" -e "s/(= $version)/(= $upgrade)/" "$x/DEBIAN/control"
    name="$(dpkg-deb -f "$f" Package)"
    dpkg-deb --root-owner-group -Zzstd --build "$x" "$work/upgrade/${name}_${upgrade}_amd64.deb" >/dev/null
done

cat > "$work/gateway.toml" <<EOF
listen = "0.0.0.0:2222"
host_key = "/var/lib/chloe/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "/var/lib/chloe/chloe.db"
master_key = "/var/lib/chloe/master.key"
audit_log = "/var/log/chloe/audit.jsonl"
idle_timeout_secs = 5
allowed_users = ["alice"]
admins = ["alice"]

[[policy.rules]]
hosts = ["httpbin.org"]
action = "allow"

[runner]
kind = "systemd"

[api]
listen = "0.0.0.0:8080"
public_url = "http://chloe.test:8080"
web_root = "/usr/share/chloe/web"
EOF

docker build -q --build-arg UBUNTU="$ubuntu" -t "chloe-deb-test-host:$ubuntu" -f "$here/test-host.Dockerfile" "$here" >/dev/null
docker run -d --name "$host" --privileged --cgroupns=private --tmpfs /run --tmpfs /run/lock \
    -v "$work:/work:ro" -v "$root/build/guest:/usr/lib/chloe/guest:ro" \
    "chloe-deb-test-host:$ubuntu" >/dev/null
wait_until 30 docker exec "$host" sh -c 'systemctl is-system-running | grep -qE "running|degraded"'

apt_install() {
    in_host sh -c "apt-get update -q >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -q $* >/dev/null 2>&1 && echo installed"
}
ssh_opts=(-p 2222 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR -i "$work/alice")
host_ip="$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$host")"
as() { ssh "${ssh_opts[@]}" "alice@$host_ip" "$@"; }
main_pid() { in_host systemctl show -P MainPID "$1"; }

section "install"
check install "installed" apt_install /work/debs/*.deb
check user "kvm" in_host id chloe
check state-dir "chloe 700" in_host stat -c '%U %a' /var/lib/chloe/workspaces
check not-enabled "disabled" in_host systemctl is-enabled chloe-gateway
check not-started "inactive" in_host systemctl is-active chloe-gateway
check conffiles "/etc/chloe/gateway.toml" in_host dpkg-query -W -f='${Conffiles}' chloe-gateway
check firecracker-version "Firecracker" in_host /usr/libexec/chloe/firecracker --version
check polkit-rule "chloe-runner" in_host cat /usr/share/polkit-1/rules.d/50-chloe.rules

section "running"
in_host sh -c 'chgrp kvm /dev/kvm && chmod 660 /dev/kvm'
in_host cp /work/gateway.toml /etc/chloe/gateway.toml
in_host systemd-run --quiet --unit keys python3 -m http.server --bind 127.0.0.1 --directory /work/keys "$keys_port"
in_host systemctl enable --now chloe-gateway
wait_until 20 bash -c "exec 3<>/dev/tcp/$host_ip/2222"
check shell "hello" as 'echo hello'
# deb に入れた Web の画面を、ゲートウェイが出す。
check web-ui '<div id="root">' curl -s "http://$host_ip:8080/inbox"
check web-manifest '"name":"chloe"' curl -s "http://$host_ip:8080/manifest.webmanifest"
ssh "${ssh_opts[@]}" "alice@$host_ip" 'sleep 300' &
hold=$!
key="$(in_host ls /var/lib/chloe/workspaces | head -1)"
check runner "active" in_host systemctl is-active "chloe-runner@$key"
check users "chloe" in_host sh -c 'ps -o user= -C firecracker,chloe-egress,dnsmasq | sort -u'
check allowed-host "200" as "curl -s -o /dev/null -w '%{http_code}' https://httpbin.org/get"
check blocked-host "blocked" as "curl -s --max-time 10 https://example.com >/dev/null && echo reached || echo blocked"

section "upgrade"
runner_pid="$(main_pid "chloe-runner@$key")"
gateway_pid="$(main_pid chloe-gateway)"
check upgrade "installed" apt_install /work/upgrade/*.deb
check new-version "$upgrade" in_host dpkg-query -W -f='${Version}' chloe-gateway
check config-kept "idle_timeout_secs = 5" in_host cat /etc/chloe/gateway.toml
check runner-kept "same" sh -c '[ "$(docker exec "$0" systemctl show -P MainPID "chloe-runner@$1")" = "$2" ] && echo same' "$host" "$key" "$runner_pid"
check gateway-restarted "restarted" sh -c '[ "$(docker exec "$0" systemctl show -P MainPID chloe-gateway)" != "$1" ] && echo restarted' "$host" "$gateway_pid"
check host-net-kept "chloe-host" in_host nft list tables
wait_until 20 bash -c "exec 3<>/dev/tcp/$host_ip/2222"
check still-enabled "enabled" in_host systemctl is-enabled chloe-gateway
check adopted "hello" as 'echo hello'

section "remove"
kill "$hold" 2>/dev/null || true
check remove "removed" in_host sh -c 'DEBIAN_FRONTEND=noninteractive apt-get remove -y -q chloe-gateway chloe-node chloe-common >/dev/null 2>&1 && echo removed'
check gateway-stopped "inactive" in_host systemctl is-active chloe-gateway
check runner-stopped "inactive" in_host systemctl is-active "chloe-runner@$key"
check netns-removed "gone" in_host sh -c 'ip netns list 2>/dev/null | grep -q chloe && echo present || echo gone'
check host-net-removed "gone" in_host sh -c 'nft list tables | grep -q chloe-host && echo present || echo gone'
check wants-link-removed "gone" in_host sh -c '[ -e /etc/systemd/system/multi-user.target.wants/chloe-gateway.service ] && echo present || echo gone'
check config-left "gateway.toml" in_host ls /etc/chloe
check purge "purged" in_host sh -c 'DEBIAN_FRONTEND=noninteractive apt-get purge -y -q chloe-gateway chloe-node chloe-common >/dev/null 2>&1 && echo purged'
check config-purged "gone" in_host sh -c '[ -e /etc/chloe/gateway.toml ] || [ -e /etc/chloe/runner.env ] && echo present || echo gone'
check state-kept "master.key" in_host ls /var/lib/chloe
check user-kept "chloe" in_host id -un chloe

timing_summary

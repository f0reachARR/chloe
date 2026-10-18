#!/usr/bin/env bash
# 段階 11 の確認。ランナーを systemd の unit で動かす (設定の [runner] の kind = "systemd")。
#
# Docker のない、root の使えるホストの代わりに、特権コンテナの中で systemd を PID 1 にして動かす
# (systemd-host.Dockerfile)。ゲートウェイは deploy/ の unit で chloe の利用者として動かし、
# ランナーの unit、polkit の規則、ホストのネットワークの規則も deploy/ のものを使う。
# GitHub の代わりに、テスト用の公開鍵を返す HTTP サーバーをコンテナの中に立てる。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="${CHLOE_DEV_STATE:-$root/build/dev/systemd}"
logs="$state/logs"
work="$(mktemp -d)"
host="${CHLOE_DEV_NAME:-chloe-dev-systemd}"
keys_port=18931

cleanup() {
    kill "${alice_bg:-}" "${bob_bg:-}" 2>/dev/null || true
    docker logs "$host" > "$logs/host.log" 2>&1 || true
    in_host journalctl --no-pager -u chloe-gateway > "$logs/gateway.log" 2>&1 || true
    in_host journalctl --no-pager -u 'chloe-runner@*' -u 'chloe-netns@*' -u chloe-host-net > "$logs/runner.log" 2>&1 || true
    docker rm -f "$host" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"

in_host() { docker exec "$host" "$@"; }
as_chloe() { in_host runuser -u chloe -- "$@"; }

docker rm -f "$host" >/dev/null 2>&1 || true
rm -rf "$state"
mkdir -p "$logs" "$work/keys"
for user in alice bob; do
    ssh-keygen -q -t ed25519 -N '' -C "$user" -f "$work/$user"
    cp "$work/$user.pub" "$work/keys/$user.keys"
done

cat > "$work/gateway.toml" <<EOF
listen = "0.0.0.0:2222"
host_key = "/var/lib/chloe/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "/var/lib/chloe/chloe.db"
master_key = "/var/lib/chloe/master.key"
audit_log = "/var/log/chloe/audit.jsonl"
idle_timeout_secs = 5
allowed_users = ["alice", "bob"]
admins = ["alice"]

[[policy.rules]]
hosts = ["httpbin.org"]
action = "allow"

[runner]
kind = "systemd"
EOF

cat > "$work/runner.env" <<EOF
CHLOE_STATE_ROOT=/var/lib/chloe/workspaces
CHLOE_GUEST_DIR=/usr/local/lib/chloe/guest
CHLOE_EGRESS_BIN=/usr/local/lib/chloe/chloe-egress
CHLOE_NET_POOL=10.250.0.0/16
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null
docker build -q -t chloe-systemd-host -f "$here/systemd-host.Dockerfile" "$here" >/dev/null

units=()
for unit in chloe-gateway.service chloe-host-net.service chloe-netns@.service chloe-runner@.service; do
    units+=(-v "$root/deploy/systemd/$unit:/etc/systemd/system/$unit:ro")
done
docker run -d --name "$host" --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock \
    "${units[@]}" \
    -v "$root/deploy/polkit/50-chloe.rules:/etc/polkit-1/rules.d/50-chloe.rules:ro" \
    -v "$work/gateway.toml:/etc/chloe/gateway.toml:ro" \
    -v "$work/runner.env:/etc/chloe/runner.env:ro" \
    -v "$work/keys:/srv/keys:ro" \
    -v "$root/target/debug/chloe-gateway:/usr/local/bin/chloe-gateway:ro" \
    -v "$root/target/debug/chloe-egress:/usr/local/lib/chloe/chloe-egress:ro" \
    -v "$root/build/guest:/usr/local/lib/chloe/guest:ro" \
    -v "$root/images/guest/runner:/usr/local/lib/chloe/runner:ro" \
    chloe-systemd-host >/dev/null
wait_until 30 docker exec "$host" sh -c 'systemctl is-system-running | grep -qE "running|degraded"'
# コンテナの /dev はホストと別なので、/dev/kvm をコンテナの kvm のグループにする。
in_host sh -c 'chgrp kvm /dev/kvm && chmod 660 /dev/kvm'
in_host systemd-run --quiet --unit keys python3 -m http.server --bind 127.0.0.1 --directory /srv/keys "$keys_port"
in_host systemctl start chloe-gateway
host_ip="$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$host")"
wait_until 20 bash -c "exec 3<>/dev/tcp/$host_ip/2222"

ssh_opts=(-p 2222 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/$user" "$user@$host_ip" "$@"; }
# ランナーの識別子は、状態のディレクトリが一つずつ増えるので、新しいものから探す。
runner_keys() { in_host ls /var/lib/chloe/workspaces; }
# 期待する文字列は出力に含まれるかで比べるので、"active" が "inactive" に一致しないよう括弧で囲む。
unit_active() { echo "[$(in_host systemctl is-active "chloe-runner@$1")]"; }

section "starting a runner"
check gateway-user "chloe" in_host ps -o user= -C chloe-gateway
check alice-shell "hello" as alice 'echo hello'
# 確認のあいだにランナーが止まらないよう、接続を一つ開いたままにする。
ssh "${ssh_opts[@]}" -i "$work/alice" "alice@$host_ip" 'sleep 300' &
alice_bg=$!
alice_key="$(runner_keys | head -1)"
check runner-active "[active]" unit_active "$alice_key"
check netns-active "active" in_host systemctl is-active "chloe-netns@$alice_key"
check firecracker-user "chloe" in_host sh -c 'ps -o user= -C firecracker | sort -u'
check egress-user "chloe" in_host sh -c 'ps -o user= -C chloe-egress | sort -u'
check dnsmasq-user "chloe" in_host sh -c 'ps -o user= -C dnsmasq | sort -u'
check state-owner "chloe" in_host stat -c %U "/var/lib/chloe/workspaces/$alice_key/workspace.ext4"
check netns "chloe-$alice_key" in_host ip netns list
check veth "cw0" in_host ip -o link show cw0

section "egress and DNS"
check allowed-host "200" as alice "curl -s -o /dev/null -w '%{http_code}' https://httpbin.org/get"
check blocked-host "blocked" as alice "curl -s --max-time 10 https://example.com >/dev/null && echo reached || echo blocked"
check dns-allowed "httpbin.org" as alice 'getent hosts httpbin.org'
check dns-blocked "nxdomain" as alice 'getent hosts example.com || echo nxdomain'
check requests-logged '"host":"httpbin.org"' in_host cat "/var/lib/chloe/workspaces/$alice_key/logs/requests.jsonl"

section "host network rules"
# namespace からホストへの通信 (ゲートウェイの SSH) と、プライベートアドレスへの転送を落とす。
check netns-to-host "blocked" in_host sh -c "ip netns exec chloe-$alice_key timeout 5 bash -c 'exec 3<>/dev/tcp/10.250.0.1/2222' 2>/dev/null && echo reached || echo blocked"
docker_gw="$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}' "$host")"
check netns-to-private "blocked" in_host sh -c "ip netns exec chloe-$alice_key timeout 5 bash -c 'exec 3<>/dev/tcp/$docker_gw/22' 2>/dev/null && echo reached || echo blocked"
check private-dropped "dropped" in_host sh -c "nft list chain inet chloe-host forward | grep 'ip daddr' | grep -qE 'packets [1-9]' && echo dropped"
check netns-to-internet "reached" in_host sh -c "ip netns exec chloe-$alice_key curl -s -o /dev/null --max-time 10 https://httpbin.org/get && echo reached || echo blocked"

section "updating a running runner"
# シークレットを登録すると、差し替え先 (api.openai.com) が方針に加わり、VM の DNS の許可リストも変わる。
check dns-before-update "nxdomain" as alice 'getent hosts api.openai.com || echo nxdomain'
check secret-set "saved openai_api_key" sh -c 'printf "%s\n" "$0" | "$@"' "$(example_secret openai_api_key real-secret-O)" \
    ssh "${ssh_opts[@]}" -i "$work/alice" "alice@$host_ip" chloe secret set openai_api_key --spec
check egress-updated "policy updated" in_host cat "/var/lib/chloe/workspaces/$alice_key/logs/egress.log"
check dns-after-update "api.openai.com" as alice 'getent hosts api.openai.com'

section "two runners"
check bob-shell "hello" as bob 'echo hello'
ssh "${ssh_opts[@]}" -i "$work/bob" "bob@$host_ip" 'sleep 300' &
bob_bg=$!
bob_key="$(runner_keys | grep -vx "$alice_key")"
check bob-active "[active]" unit_active "$bob_key"
check second-veth "cw1" in_host ip -o link show cw1
check runners-listed "2" in_host sh -c "systemctl list-units --state=active --plain --no-legend 'chloe-runner@*' | wc -l"

section "gateway restart"
in_host systemctl restart chloe-gateway
wait_until 20 bash -c "exec 3<>/dev/tcp/$host_ip/2222"
check still-active "[active]" unit_active "$alice_key"
check adopted "hello" as alice 'echo hello'
# 再起動で切れた接続を開き直す。
ssh "${ssh_opts[@]}" -i "$work/alice" "alice@$host_ip" 'sleep 300' &
alice_bg=$!
ssh "${ssh_opts[@]}" -i "$work/bob" "bob@$host_ip" 'sleep 300' &
bob_bg=$!

section "polkit"
check deny-other-unit "denied" sh -c "docker exec $host runuser -u chloe -- systemctl --no-ask-password restart chloe-gateway 2>/dev/null && echo allowed || echo denied"
check deny-netns-unit "denied" sh -c "docker exec $host runuser -u chloe -- systemctl --no-ask-password stop chloe-netns@$alice_key 2>/dev/null && echo allowed || echo denied"
check deny-restart-verb "denied" sh -c "docker exec $host runuser -u chloe -- systemctl --no-ask-password restart chloe-runner@$alice_key 2>/dev/null && echo allowed || echo denied"

section "idle stop"
# 接続がなくなると idle_timeout_secs (5 秒) のあとにランナーを止め、namespace と veth を片づける。
kill "$alice_bg" 2>/dev/null || true
stopped() { [ "$(unit_active "$alice_key")" = "[inactive]" ]; }
wait_until 60 stopped || true
check runner-stopped "[inactive]" unit_active "$alice_key"
check netns-stopped "inactive" in_host systemctl is-active "chloe-netns@$alice_key"
check netns-removed "gone" in_host sh -c "ip netns list | grep -q chloe-$alice_key && echo present || echo gone"
check slot-freed "gone" in_host sh -c "ip link show cw0 >/dev/null 2>&1 && echo present || echo gone"
check restart-after-stop "hello" as alice 'echo hello'
check slot-reused "cw0" in_host ip -o link show cw0
check bob-kept "[active]" unit_active "$bob_key"
kill "$bob_bg" 2>/dev/null || true

timing_summary

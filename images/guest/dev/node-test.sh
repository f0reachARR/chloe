#!/usr/bin/env bash
# 段階 9 の確認。一台のマシンで chloe-node を二つ (n1、n2) 動かし、コントロールプレーン
# (ゲートウェイ) から二つのホストにワークスペースを置く。
#
# - chloe-node は参加用のトークンで証明書を受け取り、トークンは一度しか使えない。
# - ワークスペースは置いたワークスペースが少ないホストに置かれ、どちらのホストでも端末、
#   ポート転送、シークレットの差し替えが動く。
# - chloe-node の API はコントロールプレーンの証明書でしか使えず、ほかのホストに置いた
#   ランナーの本物の値は問い合わせられない。
# - つながらないホストのランナーを、reconcile は片づけない。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev/nodes"
logs="$state/logs"
work="$(mktemp -d)"
ssh_port=2227
keys_port=18911
internal_port=18910
forward_port=18914
declare -A node_port=([n1]=18912 [n2]=18913)
declare -A node_pid=()

cleanup() {
    kill "${keys_pid:-}" "${gateway_pid:-}" "${forward_pid:-}" "${node_pid[@]}" 2>/dev/null || true
    docker ps -aq --filter "name=^chloe-n[12]-" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"

docker ps -aq --filter "name=^chloe-n[12]-" | xargs -r docker rm -f >/dev/null
rm -rf "$state"
mkdir -p "$logs" "$work/keys"
for user in alice bob; do
    ssh-keygen -q -t ed25519 -N '' -C "$user" -f "$work/$user"
    cp "$work/$user.pub" "$work/keys/$user.keys"
done
python3 -m http.server --bind 127.0.0.1 --directory "$work/keys" "$keys_port" >/dev/null 2>&1 &
keys_pid=$!

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$state/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "$state/chloe.db"
master_key = "$state/master.key"
idle_timeout_secs = 5
reconcile_interval_secs = 2
allowed_users = ["alice", "bob"]

[[policy.rules]]
hosts = ["httpbin.org"]
action = "allow"

[cluster]
listen = "127.0.0.1:$internal_port"
server_names = ["127.0.0.1"]
pki_dir = "$state/pki"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress -p chloe-node
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null
gateway="$root/target/debug/chloe-gateway"
start_gateway() {
    "$gateway" --config "$work/gateway.toml" >> "$logs/gateway.log" 2>&1 &
    gateway_pid=$!
    wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/$ssh_port"
}
start_node() {
    "$root/target/debug/chloe-node" run --config "$work/$1.toml" >> "$logs/$1.log" 2>&1 &
    node_pid[$1]=$!
    wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/${node_port[$1]}"
}
start_gateway

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/$user" "$user@127.0.0.1" "$@"; }
tls() { echo "$state/$1/tls"; }
# 証明書を指定して内部 API や chloe-node の API を呼ぶ。
curl_as() {
    local who="$1"; shift
    curl -s --cacert "$(tls n1)/ca.pem" --cert "$(tls "$who")/cert.pem" --key "$(tls "$who")/key.pem" "$@"
}
container_of() { docker ps --format '{{.Names}}' --filter "name=^chloe-n[12]-$1\$"; }

section "joining"
for node in n1 n2; do
    "$gateway" --config "$work/gateway.toml" node add "$node" --url "https://127.0.0.1:${node_port[$node]}" \
        > "$work/$node.token" 2>/dev/null
    check "join-$node" "joined as $node" "$root/target/debug/chloe-node" join \
        --server "https://127.0.0.1:$internal_port" --token "$(cat "$work/$node.token")" --dir "$(tls "$node")"
    cat > "$work/$node.toml" <<EOF
listen = "127.0.0.1:${node_port[$node]}"
credentials = "$(tls "$node")"
control_plane = "https://127.0.0.1:$internal_port"

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$state/$node/workspaces"
name_prefix = "chloe-$node-"
EOF
    start_node "$node"
done
check token-once "refused to join" sh -c '"$0" join --server "https://127.0.0.1:$1" --token "$2" --dir "$3" 2>&1 || true' \
    "$root/target/debug/chloe-node" "$internal_port" "$(cat "$work/n1.token")" "$work/again"
wrong_fp="$(cut -d. -f1 "$work/n1.token").$(printf '0%.0s' $(seq 64))"
check wrong-ca "does not match" sh -c '"$0" join --server "https://127.0.0.1:$1" --token "$2" --dir "$3" 2>&1 || true' \
    "$root/target/debug/chloe-node" "$internal_port" "$wrong_fp" "$work/again"
check node-list "n2	https://127.0.0.1:${node_port[n2]}	joined" "$gateway" --config "$work/gateway.toml" node list

section "placement"
example_secret anthropic_api_key real-secret-A | as alice chloe secret set anthropic_api_key --spec >/dev/null
example_secret anthropic_api_key real-secret-B | as bob chloe secret set anthropic_api_key --spec >/dev/null
# 接続をつないだまま、二人のワークスペースを起動する。
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 300' &
alice_bg=$!
wait_until 90 as alice true
ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 'sleep 300' &
bob_bg=$!
wait_until 90 as bob true
check alice-on-n1 "chloe-n1-ws-1" container_of ws-1
check bob-on-n2 "chloe-n2-ws-2" container_of ws-2
check shell-n1 "user" as alice 'id -un'
check shell-n2 "user" as bob 'id -un'

section "secrets through the node"
anthropic() { as "$1" "curl -s -o /dev/null -w '%{http_code}' -H \"x-api-key: \$ANTHROPIC_API_KEY\" -H 'anthropic-version: 2023-06-01' https://api.anthropic.com/v1/models"; }
check secret-n1 "401" anthropic alice
check secret-n2 "401" anthropic bob
sleep 1
check injected-n1 '"injected":["anthropic_api_key-0"]' sh -c 'grep api.anthropic.com "$0" | tail -1' "$state/n1/workspaces/ws-1/logs/requests.jsonl"
check injected-n2 '"injected":["anthropic_api_key-0"]' sh -c 'grep api.anthropic.com "$0" | tail -1' "$state/n2/workspaces/ws-2/logs/requests.jsonl"
check served-relayed "secret served" sh -c 'strip_colors() { sed "s/\x1b\[[0-9;]*m//g" "$@"; }; strip_colors "$0" | grep -o "secret served.*anthropic_api_key" | tail -1' "$logs/gateway.log"
check no-secret-on-node "clean" sh -c '! grep -rq "real-secret" "$0" "$1" && echo clean' "$logs/n1.log" "$logs/n2.log"

section "port forwarding on a node"
as bob 'echo served > /workspace/served.txt; (setsid python3 -m http.server --bind 127.0.0.1 --directory /workspace 3000 > /dev/null 2>&1 < /dev/null &); sleep 1' >/dev/null
ssh "${ssh_opts[@]}" -i "$work/bob" -N -L "127.0.0.1:$forward_port:127.0.0.1:3000" bob@127.0.0.1 &
forward_pid=$!
wait_until 30 curl -sf "http://127.0.0.1:$forward_port/served.txt" || true
check forward-n2 "served" curl -s "http://127.0.0.1:$forward_port/served.txt"
kill "$forward_pid" 2>/dev/null || true

section "disks on a node"
check disk-set "30G workspace disk" as alice chloe ws disk default --workspace 30G
check disk-stop "stopped default" as alice chloe ws stop default
check disk-grown-n1 "30G" as alice 'df -BG --output=size /workspace | tail -1'
check disk-same-node "chloe-n1-ws-1" container_of ws-1

section "mutual TLS"
check node-api-no-cert "000" sh -c 'curl -s -o /dev/null -w "%{http_code}" --cacert "$0" "$1" || true' \
    "$(tls n1)/ca.pem" "https://127.0.0.1:${node_port[n1]}/v1/runners"
check node-api-other-node "403" curl_as n2 -o /dev/null -w '%{http_code}' "https://127.0.0.1:${node_port[n1]}/v1/runners"
question='{"key":"ws-1","question":"{\"secret\":\"CHLOE_SECRET_ANTHROPIC_API_KEY\"}"}'
check broker-other-node "403" curl_as n2 -o /dev/null -w '%{http_code}' -H 'content-type: application/json' \
    -d "$question" "https://127.0.0.1:$internal_port/internal/v1/broker"
check broker-no-cert "403" sh -c 'curl -s -o /dev/null -w "%{http_code}" --cacert "$0" -H "content-type: application/json" -d "$1" "$2"' \
    "$(tls n1)/ca.pem" "$question" "https://127.0.0.1:$internal_port/internal/v1/broker"
# 答えには本物の値が入るので、値があることだけを出す。
check broker-own-node '"value"' sh -c '"$@" | grep -o "\"value\""' _ curl -s --cacert "$(tls n1)/ca.pem" \
    --cert "$(tls n1)/cert.pem" --key "$(tls n1)/key.pem" -H 'content-type: application/json' \
    -d "$question" "https://127.0.0.1:$internal_port/internal/v1/broker"
check broker-refusal-audited "refused a secret question" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep -o "refused a secret question.*" | tail -1' "$logs/gateway.log"

section "an unreachable node"
kill "${node_pid[n2]}"; wait "${node_pid[n2]}" 2>/dev/null || true
wait_until 20 count_at_least 1 "$logs/gateway.log" "not reachable; keeping its runners" || true
check unreachable-logged "keeping its runners" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep -o "not reachable; keeping its runners" | tail -1' "$logs/gateway.log"
check not-cleared "0" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep -c "a runner stopped without the controller" || true' "$logs/gateway.log"
check other-node-fine "user" as alice 'id -un'
start_node n2
check back-again "user" as bob 'id -un'

section "orphans and node removal"
docker run -d --name chloe-n1-ws-999 ubuntu:26.04 sleep 600 >/dev/null
wait_until 20 bash -c '[ -z "$(docker ps -q --filter "name=^chloe-n1-ws-999\$")" ]' || true
check orphan-stopped-on-node "gone" sh -c '[ -z "$(docker ps -q --filter "name=^chloe-n1-ws-999\$")" ] && echo gone'
check node-rm-refused "placed on n1" sh -c '"$0" --config "$1" node rm n1 2>&1 || true' "$gateway" "$work/gateway.toml"

section "gateway restart"
kill "$gateway_pid"; wait "$gateway_pid" 2>/dev/null || true
start_gateway
wait_until 10 count_at_least 2 "$logs/gateway.log" "adopted a running runner" || true
check adopted-both "2" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep -c "adopted a running runner"' "$logs/gateway.log"
check secret-after-restart "401" anthropic bob

section "idle stop on the nodes"
kill "$alice_bg" "$bob_bg" 2>/dev/null || true
wait "$alice_bg" "$bob_bg" 2>/dev/null || true
wait_until 60 bash -c '[ -z "$(docker ps -q --filter "name=^chloe-n[12]-")" ]' || true
check stopped-both "0" sh -c 'docker ps -q --filter "name=^chloe-n[12]-" | wc -l'
check same-node-again "chloe-n2-ws-2" sh -c '"$@" >/dev/null; docker ps --format "{{.Names}}" --filter "name=^chloe-n[12]-ws-2\$"' _ \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 true

timing_summary

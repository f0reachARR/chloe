#!/usr/bin/env bash
# 段階 7 の確認 (作業中のエージェントを待つ停止と、ゲートウェイの再起動からの復旧)。
#
# - Claude Code と Codex のフックが chloe-activity を呼ぶ。
# - 接続がなくなっても、作業中の印があるあいだはランナーを止めず、印が消えたら止める。
# - ゲートウェイを再起動しても、動いているランナーを引き取り、本物の値の問い合わせに答え続ける。
# - 持ち主のないランナーは reconcile で止める。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev/lifecycle"
logs="$state/logs"
work="$(mktemp -d)"
prefix=chloe-lc-
ssh_port=2226
keys_port=18903

cleanup() {
    kill "${keys_pid:-}" "${gateway_pid:-}" 2>/dev/null || true
    docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

result() { echo "==RESULT== $1: $2"; }
check() {
    local name="$1" expected="$2"; shift 2
    local out
    out="$("$@" 2>&1)" || true
    if [[ "$out" == *"$expected"* ]]; then
        result "$name" "ok ${out//$'\n'/ | }"
    else
        result "$name" "FAIL expected '$expected', got: ${out//$'\n'/ | }"
    fi
}

docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
rm -rf "$state"
mkdir -p "$logs" "$work/keys"
ssh-keygen -q -t ed25519 -N '' -C alice -f "$work/alice"
cp "$work/alice.pub" "$work/keys/alice.keys"
python3 -m http.server --bind 127.0.0.1 --directory "$work/keys" "$keys_port" >/dev/null 2>&1 &
keys_pid=$!

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$state/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "$state/chloe.db"
master_key = "$state/master.key"
idle_timeout_secs = 10
activity_poll_secs = 3
reconcile_interval_secs = 5
allowed_users = ["alice"]

[policy]
allow = ["httpbin.org", "statsig.anthropic.com"]

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$state/workspaces"
name_prefix = "$prefix"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress -p chloe-runner
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here" >/dev/null
start_gateway() {
    "$root/target/debug/chloe-gateway" --config "$work/gateway.toml" >> "$logs/gateway.log" 2>&1 &
    gateway_pid=$!
    sleep 1
}
start_gateway

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as_alice() { ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 "$@"; }
key=ws-1
container="$prefix$key"
running() { [ -n "$(docker ps -q --filter "name=^$container\$")" ] && echo running || echo stopped; }
# ゲートウェイを通さずに VM で実行する (ゲートウェイの接続数に数えない)。
vm() { timeout 60 "$root/target/debug/chloe-runner" exec --vsock "$state/workspaces/$key/vsock.sock" -- bash -lc "$1" < /dev/null; }
gateway_log() { sed 's/\x1b\[[0-9;]*m//g' "$logs/gateway.log"; }
log_has() { gateway_log | grep -o "$1" | tail -1; }

echo "--- hooks"
echo real-secret-A | as_alice chloe secret set anthropic_api_key >/dev/null
check claude-hooks "begin claude" as_alice 'rm -f /tmp/act.log; CHLOE_ACTIVITY_LOG=/tmp/act.log timeout 120 claude -p hi < /dev/null >/dev/null 2>&1; cat /tmp/act.log'
check claude-hooks-end "end claude" as_alice 'cat /tmp/act.log'
check codex-hooks "begin codex" as_alice 'rm -f /tmp/act.log; CHLOE_ACTIVITY_LOG=/tmp/act.log timeout 120 codex exec --skip-git-repo-check hi < /dev/null >/dev/null 2>&1; cat /tmp/act.log'
# Codex はターンが失敗するとフックを呼ばないので印が残るが、印のプロセスはもう終わっている。
check codex-marker-dead "dead" as_alice 'for f in /run/chloe/activity/codex-*; do [ -d "/proc/$(cat "$f")" ] && echo alive || echo dead; done'

echo "--- waiting for a working agent"
# 接続を一本つないだまま、エージェントのふりをする "claude" という名前のプロセスと印を作り、
# それから接続を切る。(chloe-activity がフックから呼ばれたときと同じく、印にプロセス ID を書く。)
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 300' &
holder=$!
sleep 3
# (/bin/sleep は呼ばれた名前で動きを変える coreutils の一体型なので、名前を変えて写せない。
# シェルスクリプトのプロセスの名前は、スクリプトのファイル名になる。)
check fake-agent-marker "has-pid" vm 'mkdir -p /tmp/fake-agent && printf "#!/bin/sh\nwhile :; do sleep 5; done\n" > /tmp/fake-agent/claude && chmod +x /tmp/fake-agent/claude && (setsid /tmp/fake-agent/claude > /dev/null 2>&1 < /dev/null &) ; sleep 1; pgrep -x claude > /run/chloe/activity/claude-test-session; grep -qx "[0-9][0-9]*" /run/chloe/activity/claude-test-session && echo has-pid'
kill "$holder"; wait "$holder" 2>/dev/null || true
sleep 25
check busy-keeps-running "running" running
check busy-logged "an agent is working" log_has 'an agent is working' 
vm 'pkill -x claude' || true
for _ in $(seq 60); do [ "$(running)" = stopped ] && break; sleep 1; done
check idle-after-work "stopped" running

echo "--- gateway restart"
as_alice 'echo kept > /workspace/marker' >/dev/null
check started-again "running" running
kill "$gateway_pid"; wait "$gateway_pid" 2>/dev/null || true
check survives-restart "running" running
start_gateway
check adopted "adopted a running runner" log_has 'adopted a running runner' 
check list-running "running" as_alice chloe ws list
check secret-after-restart "401" vm "curl -s -o /dev/null -w '%{http_code}' -H \"x-api-key: \$ANTHROPIC_API_KEY\" -H 'anthropic-version: 2023-06-01' https://api.anthropic.com/v1/models"
check workspace-intact "kept" as_alice 'cat /workspace/marker'
for _ in $(seq 60); do [ "$(running)" = stopped ] && break; sleep 1; done
check adopted-idle-stop "stopped" running

echo "--- orphans"
docker run -d --name "${prefix}ws-999" ubuntu:26.04 sleep 600 >/dev/null
for _ in $(seq 20); do [ -z "$(docker ps -q --filter "name=^${prefix}ws-999\$")" ] && break; sleep 1; done
check orphan-stopped "gone" sh -c '[ -z "$(docker ps -q --filter "name=^$0ws-999\$")" ] && echo gone' "$prefix"

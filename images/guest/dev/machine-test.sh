#!/usr/bin/env bash
# 段階 16 の確認 (ワークスペースごとの VM の大きさと停止までの時間)。
#
# - ws create の --cpus と --memory で決めた大きさで VM が起動する。
# - ws machine で変えた大きさは、動いている VM には効かず、次の起動から効く。
# - ws idle never にすると、接続がなくなっても止めない。default に戻すと、今から数えて止める。
# - ws idle で決めた時間で止める。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev/machine"
logs="$state/logs"
work="$(mktemp -d)"
prefix=chloe-mc-
ssh_port=2228
keys_port=18941

cleanup() {
    kill "${keys_pid:-}" "${gateway_pid:-}" 2>/dev/null || true
    docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"
# ゲートウェイの停止までの時間。確認と確認のあいだにランナーが止まらないだけの長さは残す。
idle=5

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
idle_timeout_secs = $idle
activity_poll_secs = 1
allowed_users = ["alice"]

[machine]
vcpus = 2
memory_mib = 2048

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$state/workspaces"
name_prefix = "$prefix"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress -p chloe-runner
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null
"$root/target/debug/chloe-gateway" --config "$work/gateway.toml" >> "$logs/gateway.log" 2>&1 &
gateway_pid=$!
wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/$ssh_port"

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as_alice() { ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 "$@"; }
# ワークスペースにつなぐ。引数: ワークスペースの名前、コマンド
in_ws() { local ws="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/alice" "alice+$ws@127.0.0.1" "$@"; }
# ワークスペース名 → ランナーの識別子 (作った順)。
key_of() { case "$1" in big) echo ws-1 ;; small) echo ws-2 ;; esac; }
running() {
    local container="$prefix$(key_of "$1")"
    [ -n "$(docker ps -q --filter "name=^$container\$")" ] && echo running || echo stopped
}
is_stopped() { [ "$(running "$1")" = stopped ]; }
# VM の vCPU の数とメモリ (MiB)。カーネルが使う分だけ MemTotal は割り当てより小さく出る。
machine_of() { in_ws "$1" 'echo "cpus=$(nproc) mem=$(( $(awk "/MemTotal/ {print \$2}" /proc/meminfo) / 1024 ))"'; }
# 期待する vCPU の数と、メモリ (MiB) が範囲に入っているか。
machine_is() {
    local out cpus mem
    out="$(machine_of "$1")"
    echo "$out"
    cpus="${out#cpus=}"; cpus="${cpus%% *}"
    mem="${out##*mem=}"
    [ "$cpus" = "$2" ] && [ "$mem" -ge "$3" ] && [ "$mem" -le "$4" ] && echo matches
}

section "create with a size"
check create-big "created big" as_alice chloe ws create big --cpus 3 --memory 3G --idle never
check create-small "created small" as_alice chloe ws create small
check show-machine "cpus	3" as_alice chloe ws machine big
check show-default-machine "memory	2G" as_alice chloe ws machine small
check list-columns "3cpu	3G	never" as_alice chloe ws list
check machine-big "matches" machine_is big 3 2700 3072
check machine-default "matches" machine_is small 2 1700 2048
check reject-cpus "between 1 and 32" as_alice chloe ws machine big --cpus 33
check reject-memory "at least 512 MiB" as_alice chloe ws machine big --memory 256M

section "resize from the next start"
check resize "4 vCPUs and 2G of memory from the next start" as_alice chloe ws machine big --cpus 4 --memory 2G
check running-keeps-size "matches" machine_is big 3 2700 3072
check stop "stopped big" as_alice chloe ws stop big
check resized "matches" machine_is big 4 1700 2048

section "never stops"
# 接続がなくなってから、ゲートウェイの停止までの時間の倍だけ待つ。
sleep $((idle * 2 + 2))
check never-keeps-running "running" running big
check default-stops-meanwhile "stopped" running small
check back-to-default "will stop 5s after the last connection closes" as_alice chloe ws idle big default
wait_until $((idle + 15)) is_stopped big || true
check default-stops "stopped" running big

section "own idle timeout"
check set-idle "will stop 20s after the last connection closes" as_alice chloe ws idle small 20s
check show-idle "20s" as_alice chloe ws idle small
in_ws small true
sleep $((idle + 3))
check own-idle-keeps-running "running" running small
wait_until 30 is_stopped small || true
check own-idle-stops "stopped" running small
check reject-idle "invalid idle timeout" as_alice chloe ws idle small 0m

timing_summary

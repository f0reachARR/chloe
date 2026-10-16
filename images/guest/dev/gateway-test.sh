#!/usr/bin/env bash
# 段階 5 の確認。chloe-gateway に OpenSSH のクライアントでつなぎ、VM のシェルとコマンドまで届くかを確かめる。
#
# GitHub の代わりに、テスト用の公開鍵を返す HTTP サーバーをローカルに立てる。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

work="$dev_scripts/gateway"
mkdir -p "$work/keys"
ssh-keygen -q -t ed25519 -N '' -C tester -f "$work/tester"
ssh-keygen -q -t ed25519 -N '' -C stranger -f "$work/stranger"
cp "$work/tester.pub" "$work/keys/tester.keys"
cp "$work/stranger.pub" "$work/keys/outsider.keys"

keys_port=18900
ssh_port=2222
python3 -m http.server --bind 127.0.0.1 --directory "$work/keys" "$keys_port" >/dev/null 2>&1 &
keys_pid=$!

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$dev_state/gateway/host_key"
github_url = "http://127.0.0.1:$keys_port"

[workspaces.default]
vsock = "$dev_state/vsock.sock"
members = ["tester", "outsider"]

[workspaces.other]
vsock = "$dev_state/vsock.sock"
members = ["outsider"]
EOF

cargo build --quiet --manifest-path "$dev_root/Cargo.toml" -p chloe-gateway
"$dev_root/target/debug/chloe-gateway" --config "$work/gateway.toml" > "$dev_logs/gateway.log" 2>&1 &
gateway_pid=$!
trap 'kill $keys_pid $gateway_pid 2>/dev/null; rm -rf "$dev_scripts"; docker kill "$CHLOE_DEV_NAME" >/dev/null 2>&1 || true' EXIT

start_vm

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as_tester() { ssh "${ssh_opts[@]}" -i "$work/tester" "$@"; }

host_check exec "user /workspace" as_tester tester@127.0.0.1 'echo $(id -un) $PWD'
host_check exit-code "status=7" sh -c '"$@" "exit 7"; echo "status=$?"' _ \
    ssh "${ssh_opts[@]}" -i "$work/tester" tester@127.0.0.1
host_check stdin "HELLO" sh -c 'echo hello | "$@" "tr a-z A-Z"' _ \
    ssh "${ssh_opts[@]}" -i "$work/tester" tester@127.0.0.1
host_check stderr "to-stderr" sh -c '"$@" "echo to-stderr >&2" 2>&1 >/dev/null' _ \
    ssh "${ssh_opts[@]}" -i "$work/tester" tester@127.0.0.1
host_check pty "/dev/pts/" as_tester -tt tester@127.0.0.1 tty
host_check shell "SHELL-OK user" sh -c 'printf "echo SHELL-OK \$(whoami)\nexit\n" | script -qec "$*" /dev/null | grep -ao "SHELL-OK user"' _ \
    ssh "${ssh_opts[*]}" -i "$work/tester" -tt tester@127.0.0.1
host_check workspace-by-name "user" as_tester tester+default@127.0.0.1 'id -un'

# 登録されていない鍵、メンバーでないワークスペース、存在しないワークスペースは拒否する。
host_check wrong-key "Permission denied" ssh "${ssh_opts[@]}" -i "$work/stranger" tester@127.0.0.1 true
host_check not-member "Permission denied" as_tester tester+other@127.0.0.1 true
host_check no-workspace "Permission denied" as_tester tester+nope@127.0.0.1 true
host_check bad-login "Permission denied" as_tester '../x@127.0.0.1' true

stop_vm

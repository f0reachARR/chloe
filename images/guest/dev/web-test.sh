#!/usr/bin/env bash
# Web の画面の E2E。本物のゲートウェイとワークスペースの VM を相手に、Playwright の Chromium で
# 画面を動かす (web/e2e)。スマートフォンの幅 (明るい配色) と広い画面 (暗い配色) で確かめる。
#
# 画面をビルドしてゲートウェイの web_root に置き、`chloe web login` の URL でログインする。
# Playwright のブラウザがなければ、先に `cd web && pnpm exec playwright install chromium` を実行する。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev/web"
logs="$state/logs"
work="$(mktemp -d)"
prefix=chloe-web-
ssh_port=2224
keys_port=18902
api_port=18921

cleanup() {
    kill "${keys_pid:-}" "${gateway_pid:-}" 2>/dev/null || true
    docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"

docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
rm -rf "$state"
mkdir -p "$logs" "$work/keys"
ssh-keygen -q -t ed25519 -N '' -C alice -f "$work/alice"
cp "$work/alice.pub" "$work/keys/alice.keys"
python3 -m http.server --bind 127.0.0.1 --directory "$work/keys" "$keys_port" >/dev/null 2>&1 &
keys_pid=$!

(cd "$root/web" && pnpm install --frozen-lockfile --silent && pnpm build >/dev/null)

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$state/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "$state/chloe.db"
master_key = "$state/master.key"
allowed_users = ["alice"]

[api]
listen = "127.0.0.1:$api_port"
public_url = "http://127.0.0.1:$api_port"
web_root = "$root/web/dist"

[memberships]
"org:acme" = ["alice"]

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$state/workspaces"
name_prefix = "$prefix"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null
"$root/target/debug/chloe-gateway" --config "$work/gateway.toml" > "$logs/gateway.log" 2>&1 &
gateway_pid=$!
wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/$ssh_port"

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/alice" "$user@127.0.0.1" "$@"; }

section "setup"
check workspace "created acme/proj" as alice chloe ws create acme/proj
# 先に VM を起動しておく (画面のテストで起動を待たないように)。
check vm-up "UP" as alice+acme/proj 'echo UP'
login="$(as alice chloe web login | grep -o 'http://[^ ]*' | tail -1)"
check login-link "/login/link?code=" echo "$login"

section "browser"
code=0
(cd "$root/web" && CHLOE_E2E_URL="http://127.0.0.1:$api_port" CHLOE_E2E_LOGIN="$login" \
    CHLOE_E2E_OUT="$state/playwright" CHLOE_E2E_SCREENSHOTS="${CHLOE_E2E_SCREENSHOTS:-}" pnpm exec playwright test) || code=$?
# 監査ログに、Web の端末のシェルの始まりと終わりが残る。
web_shells() { [ "$(strip_colors "$logs/gateway.log" | grep 'session started' | grep -c 'via="web"')" -gt 0 ] && echo yes; }
check audit-web-shell "yes" web_shells

timing_summary
exit "$code"

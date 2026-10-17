#!/usr/bin/env bash
# 段階 6 の終了条件の確認。
#
# chloe-gateway がワークスペースごとにランナーを起動し、二人の利用者 (alice と bob) が
# それぞれの個人ワークスペースを同時に使えることを確かめる。
# GitHub の代わりに、テスト用の公開鍵を返す HTTP サーバーをローカルに立てる。
# 本物の API キーは使わない。シークレットには偽の値を登録する。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
mvp="$root/build/dev/mvp"
logs="$mvp/logs"
work="$(mktemp -d)"
prefix=chloe-mvp-
ssh_port=2223
keys_port=18901

cleanup() {
    kill "${keys_pid:-}" "${gateway_pid:-}" 2>/dev/null || true
    docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"

docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
rm -rf "$mvp"
mkdir -p "$logs" "$work/keys"
for user in alice bob stranger carol; do
    ssh-keygen -q -t ed25519 -N '' -C "$user" -f "$work/$user"
done
cp "$work/alice.pub" "$work/keys/alice.keys"
cp "$work/bob.pub" "$work/keys/bob.keys"
cp "$work/carol.pub" "$work/keys/carol.keys"
python3 -m http.server --bind 127.0.0.1 --directory "$work/keys" "$keys_port" >/dev/null 2>&1 &
keys_pid=$!

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$mvp/host_key"
github_url = "http://127.0.0.1:$keys_port"
database = "$mvp/chloe.db"
master_key = "$mvp/master.key"
idle_timeout_secs = 5
allowed_users = ["alice", "bob"]
# GitHub App がなくても、設定の admins はサーバーの運用者として設定とシークレットを変えられる。
admins = ["alice"]

[memberships]
"org:acme" = ["alice"]

[policy]
allow = ["httpbin.org", "statsig.anthropic.com"]
passthrough = ["registry-1.docker.io", "auth.docker.io", "*.docker.com"]

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$mvp/workspaces"
name_prefix = "$prefix"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here" >/dev/null
"$root/target/debug/chloe-gateway" --config "$work/gateway.toml" > "$logs/gateway.log" 2>&1 &
gateway_pid=$!
wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/$ssh_port"

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/${user%%+*}" "$user@127.0.0.1" "$@"; }
egress_log() { cat "$mvp/workspaces/$1/logs/requests.jsonl" 2>/dev/null; }

section "secrets and management commands"
check alice-secret "saved anthropic_api_key" sh -c 'echo real-secret-A | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set anthropic_api_key
check bob-secret "saved anthropic_api_key" sh -c 'echo real-secret-B | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 chloe secret set anthropic_api_key
check secret-list "anthropic_api_key" as alice chloe secret list
check secret-not-logged "clean" sh -c '! grep -q real-secret "$0" && echo clean' "$logs/gateway.log"

section "two users at the same time"
# 関数を & で動かすと $! はサブシェルになり、kill が ssh に届かないので、ssh を直接起動する。
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 300' &
alice_bg=$!
ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 'sleep 300' &
bob_bg=$!
check alice-env "sk-ant-api03-chloe" as alice 'printenv ANTHROPIC_API_KEY'
check bob-env "sk-ant-api03-chloe" as bob 'printenv ANTHROPIC_API_KEY'
alice_ph="$(as alice 'printenv ANTHROPIC_API_KEY')"
bob_ph="$(as bob 'printenv ANTHROPIC_API_KEY')"
check distinct-placeholders "differ" sh -c '[ "$0" != "$1" ] && echo differ' "$alice_ph" "$bob_ph"
check both-running "2" sh -c 'docker ps -q --filter "name=^$0" | wc -l' "$prefix"
check no-real-secret-in-vm "absent" as bob 'env | grep -q real-secret && echo present || echo absent'

# 本物の値はランナーに渡さない (コンテナの環境変数にもない)。
runner_secret_env() {
    docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$prefix$1" | grep -c '^CHLOE_SECRET_' || true
}
# ランナーの識別子は、ゲストに渡した環境変数のファイルにプレースホルダーがあるかで探す。
runner_key() { basename "$(dirname "$(dirname "$(grep -l "$1" "$mvp"/workspaces/*/config/guest.env)")")"; }
alice_key="$(runner_key "$alice_ph")"
bob_key="$(runner_key "$bob_ph")"
check alice-runner-no-secret "0" runner_secret_env "$alice_key"
check bob-runner-no-secret "0" runner_secret_env "$bob_key"

# 自分のプレースホルダーは差し替えられ、他人のプレースホルダーは差し替えられない。
anthropic() { as "$1" "curl -s -o /dev/null -w '%{http_code}' -H 'x-api-key: $2' -H 'anthropic-version: 2023-06-01' https://api.anthropic.com/v1/models"; }
check alice-own "401" anthropic alice "$alice_ph"
check bob-uses-alice-placeholder "401" anthropic bob "$alice_ph"
sleep 1
last_anthropic_request() { egress_log "$1" | grep '"host":"api.anthropic.com"' | tail -1; }
check alice-injected '"injected":["anthropic_api_key-0"]' last_anthropic_request "$alice_key"
check bob-not-injected '"injected":[]' last_anthropic_request "$bob_key"
check secret-served "secret served" sh -c 'grep "secret served" "$0" | sed "s/\x1b\[[0-9;]*m//g" | grep -o "secret served.*anthropic_api_key" | tail -1' "$logs/gateway.log"

section "updating a running workspace"
check live-update "used from the next request" sh -c 'echo real-secret-A2 | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set anthropic_api_key
check egress-updated "1" grep -c "policy updated" "$mvp/workspaces/$alice_key/logs/egress.log"
check bob-untouched "0" sh -c 'grep -c "policy updated" "$0" || true' "$mvp/workspaces/$bob_key/logs/egress.log"
check alice-still-injected "401" anthropic alice "$alice_ph"
sleep 1
check alice-injected-after-update '"injected":["anthropic_api_key-0"]' last_anthropic_request "$alice_key"
# 動いているランナーでも、新しい種類のプレースホルダーは次のシェルから入り、消すと抜ける。
started_at() { docker inspect -f '{{.State.StartedAt}}' "$prefix$alice_key"; }
alice_started="$(started_at)"
check new-kind-absent "absent" as alice 'printenv OPENAI_API_KEY || echo absent'
echo real-secret-O | as alice chloe secret set openai_api_key >/dev/null
check new-kind-in-new-shell "sk-proj-chloe" as alice 'printenv OPENAI_API_KEY'
check new-kind-same-runner "$alice_started" started_at
# 新しい種類の宛先も、動いたまま名前を引けて差し替えられる (VM の DNS の許可リストも入れ替わる)。
check new-kind-reachable "401" as alice "curl -s -o /dev/null -w '%{http_code}' -H \"Authorization: Bearer \$OPENAI_API_KEY\" https://api.openai.com/v1/models"
as alice chloe secret rm openai_api_key >/dev/null
check removed-kind-gone "absent" as alice 'printenv OPENAI_API_KEY || echo absent'

section "workspaces"
check ws-list "default" as alice chloe ws list
check ws-create-org "created acme/proj" as alice chloe ws create acme/proj --root persistent
check ws-create-not-member "not a member" as bob chloe ws create acme/proj2
check ws-org-member "user" as alice+acme/proj 'id -un'
check ws-org-not-member "Permission denied" as bob+acme/proj true
check ws-org-list "acme/proj	persistent" as alice chloe ws list
check admin-policy "saved the settings of acme" sh -c 'printf "allow = [\"example.org\"]\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme
check not-admin-policy "only the administrators" sh -c 'printf "allow = []\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 chloe policy set acme
check no-passthrough-in-policy "invalid settings" sh -c 'printf "passthrough = [\"x.example\"]\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme
check not-admin-org-secret "cannot change the secrets of acme" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 chloe secret set --owner acme openai_api_key

section "authentication"
check wrong-key "Permission denied" ssh "${ssh_opts[@]}" -i "$work/stranger" alice@127.0.0.1 true
check not-allowed-user "Permission denied" as carol true
check bad-login "Permission denied" ssh "${ssh_opts[@]}" -i "$work/alice" '../x@127.0.0.1' true
check exit-code "status=7" sh -c '"$@" "exit 7"; echo "status=$?"' _ ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1
check pty "/dev/pts/" as alice -tt tty
check shell "SHELL-OK user" sh -c 'printf "echo SHELL-OK \$(whoami)\nexit\n" | script -qec "$*" /dev/null | grep -ao "SHELL-OK user"' _ \
    ssh "${ssh_opts[*]}" -i "$work/alice" -tt alice@127.0.0.1

section "idle stop and persistence"
as alice 'echo kept > /workspace/marker && mkdir -p ~/.claude && echo kept > ~/.claude/marker'
kill "$alice_bg" "$bob_bg" 2>/dev/null || true
wait "$alice_bg" "$bob_bg" 2>/dev/null || true
stopped=no
for _ in $(seq 120); do
    if [ -z "$(docker ps -q --filter "name=^$prefix$alice_key\$")" ]; then stopped=yes; break; fi
    sleep 1
done
check idle-stop "yes" echo "$stopped"
check workspace-kept "kept" as alice 'cat /workspace/marker'
check home-kept "kept" as alice 'cat ~/.claude/marker'

section "control plane down"
# ゲートウェイ (問い合わせ先) を止めると、本物の値が要る要求は 503 で失敗し、要らない要求は通る。
# VM には chloe-runner で vsock から直接入る。
cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-runner
kill "$gateway_pid"; wait "$gateway_pid" 2>/dev/null || true
vm() { "$root/target/debug/chloe-runner" exec --vsock "$mvp/workspaces/$alice_key/vsock.sock" -- bash -lc "$1"; }
check down-secret-503 "503" vm "curl -s -o /dev/null -w '%{http_code}' -H \"x-api-key: \$ANTHROPIC_API_KEY\" https://api.anthropic.com/v1/models"
check down-plain-ok "200" vm "curl -s -o /dev/null -w '%{http_code}' https://httpbin.org/get"

timing_summary

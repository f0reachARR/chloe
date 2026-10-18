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
api_port=18920

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

[api]
listen = "127.0.0.1:$api_port"

[memberships]
"org:acme" = ["alice"]

[[policy.rules]]
hosts = ["httpbin.org", "statsig.anthropic.com"]
action = "allow"

[[policy.rules]]
hosts = ["registry-1.docker.io", "auth.docker.io", "*.docker.com"]
action = "passthrough"

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$mvp/workspaces"
name_prefix = "$prefix"
EOF

cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null
"$root/target/debug/chloe-gateway" --config "$work/gateway.toml" > "$logs/gateway.log" 2>&1 &
gateway_pid=$!
wait_until 10 bash -c "exec 3<>/dev/tcp/127.0.0.1/$ssh_port"

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/${user%%+*}" "$user@127.0.0.1" "$@"; }
egress_log() { cat "$mvp/workspaces/$1/logs/requests.jsonl" 2>/dev/null; }

section "secrets and management commands"
check alice-secret "saved anthropic_api_key" sh -c 'printf "%s\n" "$0" | "$@"' "$(example_secret anthropic_api_key real-secret-A)" \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set anthropic_api_key --spec
check bob-secret "saved anthropic_api_key" sh -c 'printf "%s\n" "$0" | "$@"' "$(example_secret anthropic_api_key real-secret-B)" \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 chloe secret set anthropic_api_key --spec
check secret-list "anthropic_api_key" as alice chloe secret list
check secret-not-logged "clean" sh -c '! grep -q real-secret "$0" && echo clean' "$logs/gateway.log"

section "two users at the same time"
# 関数を & で動かすと $! はサブシェルになり、kill が ssh に届かないので、ssh を直接起動する。
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 300' &
alice_bg=$!
ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 'sleep 300' &
bob_bg=$!
check alice-env "sk-ant-api03-chloe" as alice 'printenv ANTHROPIC_API_KEY'
# ssh <ホスト> 'コマンド' もログインシェルで動き、mise で入れたツールの shims が PATH に入る。
check login-shell-path ".local/share/mise/shims" as alice 'printenv PATH'
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
example_secret openai_api_key real-secret-O | as alice chloe secret set openai_api_key --spec >/dev/null
check new-kind-in-new-shell "sk-proj-chloe" as alice 'printenv OPENAI_API_KEY'
check new-kind-same-runner "$alice_started" started_at
# 新しい種類の宛先も、動いたまま名前を引けて差し替えられる (VM の DNS の許可リストも入れ替わる)。
check new-kind-reachable "401" as alice "curl -s -o /dev/null -w '%{http_code}' -H \"Authorization: Bearer \$OPENAI_API_KEY\" https://api.openai.com/v1/models"
as alice chloe secret rm openai_api_key >/dev/null
check removed-kind-gone "absent" as alice 'printenv OPENAI_API_KEY || echo absent'

section "secrets with a definition"
# 組み込みでない名前は定義を付けて登録する。httpbin.org が受け取ったヘッダーを返すので、差し替えた値を確かめられる。
cat > "$work/echo.toml" <<'EOF'
env = ["ECHO_TOKEN"]
placeholder = "echo_{id}_x"
value = "real-custom-secret"
[[inject]]
hosts = ["httpbin.org"]
header = "authorization"
EOF
# 署名し直したあとの Authorization も、httpbin.org の返すヘッダーで確かめる。
cat > "$work/aws.toml" <<'EOF'
[fields.access_key_id]
env = ["AWS_ACCESS_KEY_ID"]
placeholder = "AKIA{id}"
value = "AKIDREALEXAMPLE"
[fields.secret_access_key]
env = ["AWS_SECRET_ACCESS_KEY"]
value = "real-aws-secret"
[[sign]]
scheme = "aws-sigv4"
hosts = ["httpbin.org"]
EOF
set_from() { local file="$1"; shift; as alice chloe secret set "$@" < "$file"; }
check custom-needs-spec "pass one with --spec" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set echo_token
check custom-set "saved echo_token" set_from "$work/echo.toml" echo_token --spec
check custom-show 'placeholder = "echo_{id}_x"' as alice chloe secret show echo_token
check builtin-takes-no-spec "takes only a value" set_from "$work/echo.toml" github_token --spec
check former-builtin-needs-spec "pass one with --spec" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set claude_oauth_token
check custom-env "echo_chloe" as alice 'printenv ECHO_TOKEN'
echo_headers() { as alice "curl -s -H \"Authorization: Bearer \$ECHO_TOKEN\" https://httpbin.org/headers"; }
check custom-injected "Bearer real-custom-secret" echo_headers
check custom-unbound "placeholder-to-unbound-host" as alice "curl -sS -H \"x-leak: \$ECHO_TOKEN\" https://api.anthropic.com/v1/models"
check custom-value-only "saved echo_token" sh -c 'echo real-custom-secret-2 | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set echo_token
check custom-value-updated "Bearer real-custom-secret-2" echo_headers
check aws-set "saved aws" set_from "$work/aws.toml" aws --spec
check aws-env "AKIAchloe" as alice 'printenv AWS_ACCESS_KEY_ID'
check aws-resigned "Credential=AKIDREALEXAMPLE/" as alice \
    "curl -s --aws-sigv4 aws:amz:us-east-1:execute-api --user \"\$AWS_ACCESS_KEY_ID:\$AWS_SECRET_ACCESS_KEY\" https://httpbin.org/headers"
sleep 1
last_httpbin_request() { egress_log "$alice_key" | grep '"host":"httpbin.org"' | tail -1; }
check aws-sign-logged '"injected":["aws-sign-0"]' last_httpbin_request
check custom-not-logged "clean" sh -c '! grep -qE "real-custom-secret|real-aws-secret|AKIDREALEXAMPLE" "$0" && echo clean' "$logs/gateway.log"
as alice chloe secret rm echo_token >/dev/null
as alice chloe secret rm aws >/dev/null
check custom-removed "absent" as alice 'printenv ECHO_TOKEN || echo absent'

section "secrets in files"
# 環境変数を読まないツール (apt) には、プレースホルダーを入れたファイルを置く。httpbin.org の
# /basic-auth/<利用者>/<パスワード> は、Basic 認証が合えば 200 を返すので、差し替えを確かめられる。
cat > "$work/files.toml" <<'EOF'
value = "real-file-secret"
[[inject]]
hosts = ["httpbin.org"]
header = "authorization"
format = "basic-password"
[[files]]
path = "/etc/apt/auth.conf.d/chloe-test.conf"
content = "machine httpbin.org login deploy password {value}\n"
[[files]]
path = "/home/user/.netrc"
owner = "user"
mode = "0600"
content = "machine httpbin.org login deploy password {value}\n"
EOF
check files-ws "created files" as alice chloe ws create files
as alice+files true
# 動いているワークスペースには、登録したときに guest-agent が置く。
check files-set "saved file_token" set_from "$work/files.toml" file_token --spec
check files-apt-conf "password chloe" as alice+files 'cat /etc/apt/auth.conf.d/chloe-test.conf'
check files-no-real "absent" as alice+files 'sudo grep -rq real-file-secret /etc/apt/auth.conf.d ~/.netrc && echo present || echo absent'
check files-netrc-owner "600 user" as alice+files 'stat -c "%a %U" ~/.netrc'
check files-apt "apt-ok" as alice+files \
    'sudo /usr/lib/apt/apt-helper download-file https://httpbin.org/basic-auth/deploy/real-file-secret /tmp/apt-out >/dev/null && echo apt-ok'
check files-netrc "200" as alice+files \
    "curl -s -o /dev/null -w '%{http_code}' --netrc https://httpbin.org/basic-auth/deploy/real-file-secret"
# 起動し直すと、設定用のドライブから置く。
check files-stop "stopped files" as alice chloe ws stop files
check files-after-boot "password chloe" as alice+files 'cat /etc/apt/auth.conf.d/chloe-test.conf'
check files-not-logged "clean" sh -c '! grep -q real-file-secret "$0" && echo clean' "$logs/gateway.log"
# 消すと、置いたファイルも消える。
as alice chloe secret rm file_token >/dev/null
check files-removed "2 absent" as alice+files 'echo "$(ls /etc/apt/auth.conf.d/chloe-test.conf ~/.netrc 2>&1 | grep -c "No such file") absent"'
as alice chloe ws rm files --disks >/dev/null

section "workspaces"
check ws-list "default" as alice chloe ws list
check ws-create-org "created acme/proj" as alice chloe ws create acme/proj --root persistent
check ws-create-not-member "not a member" as bob chloe ws create acme/proj2
check ws-org-member "user" as alice+acme/proj 'id -un'
check ws-org-not-member "Permission denied" as bob+acme/proj true
check ws-org-list "acme/proj	persistent" as alice chloe ws list
check ws-disk-grow "30G workspace disk and a 40G Docker disk from the next start" as alice chloe ws disk acme/proj --workspace 30G
check ws-disk-no-shrink "cannot be shrunk" as alice chloe ws disk acme/proj --workspace 25G
check ws-disk-limit "between 1 and 400 GiB" as alice chloe ws disk acme/proj --docker 401G
check ws-disk-stop "stopped acme/proj" as alice chloe ws stop acme/proj
check ws-disk-grown "30G" as alice+acme/proj 'df -BG --output=size /workspace | tail -1'
check admin-policy "saved the settings of acme" sh -c 'printf "[[rules]]\nhosts = [\"example.org\"]\naction = \"allow\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme
check not-admin-policy "only the administrators" sh -c 'printf "rules = []\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/bob" bob@127.0.0.1 chloe policy set acme
check no-passthrough-in-policy "passthrough rules can only be set" sh -c 'printf "[[rules]]\nhosts = [\"x.example\"]\naction = \"passthrough\"\n" | "$@"' _ \
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
    ssh "${ssh_opts[*]}" -i "$work/alice" -tt alice+default@127.0.0.1
# ワークスペースを書かずに端末でシェルを開くと、default のほかに acme/proj にも入れるので、
# 選ぶ画面が出る。キーは fifo から送り、画面を待つ。
mkfifo "$work/pick-in"
exec 8<>"$work/pick-in"
script -qfec "stty cols 100 rows 20; ssh -tt ${ssh_opts[*]} -i $work/alice alice@127.0.0.1" /dev/null \
    < "$work/pick-in" > "$work/pick-out" 2>&1 &
pick_shows() { wait_until 60 grep -qa "$1" "$work/pick-out" && echo "$1"; }
check picker-opens "choose a workspace" pick_shows "choose a workspace"
check picker-lists "acme/proj" pick_shows "acme/proj"
printf '\033[B\r' >&8
# 選ぶと元の画面に戻してから、選んだワークスペースのシェルを開く。
check picker-leaves "1049l" pick_shows "$(printf '\033\\[?1049l')"
printf 'echo "PICKED=$CHLOE_WORKSPACE"; exit\n' >&8
check picker-shell "PICKED=acme/proj" pick_shows "PICKED=acme/proj"
exec 8>&-

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

section "management API"
api() {
    local token="$1" method="$2" path="$3"; shift 3
    curl -s -w ' %{http_code}' -X "$method" -H "Authorization: Bearer $token" \
        -H 'content-type: application/json' "http://127.0.0.1:$api_port$path" "$@"
}
alice_token="$(as alice chloe token create ci --expires 1d | head -1)"
bob_token="$(as bob chloe token create bob-ci | head -1)"
check api-token-shown "chloe_" sh -c 'echo "$0" | grep -o "^chloe_"' "$alice_token"
check api-no-token "401" curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$api_port/v1/me"
check api-wrong-token "401" api chloe_wrong GET /v1/me
check api-me '"login":"alice"' api "$alice_token" GET /v1/me
check api-list '"name":"default"' api "$alice_token" GET /v1/workspaces
check api-create '"name":"acme/from-api","root":"ephemeral","running":false,"workspace_disk_gib":20,"docker_disk_gib":40} 201' \
    api "$alice_token" POST /v1/workspaces -d '{"name":"acme/from-api"}'
check api-create-visible-by-ssh "acme/from-api" as alice chloe ws list
check api-create-not-member "not a member of org:acme" api "$bob_token" POST /v1/workspaces -d '{"name":"acme/bob"}'
check api-create-duplicate "409" api "$alice_token" POST /v1/workspaces -d '{"name":"acme/from-api"}'
check api-patch-root '"root":"persistent"' api "$alice_token" PATCH /v1/workspaces/acme/from-api -d '{"root":"persistent"}'
check api-show-other "404" api "$bob_token" GET /v1/workspaces/acme/from-api
check api-secret-set "204" api "$alice_token" PUT /v1/secrets/openai_api_key \
    -d '{"value":"real-api-secret","env":["OPENAI_API_KEY"],"placeholder":"sk-proj-{id}","inject":[{"hosts":["api.openai.com"],"header":"authorization"}]}'
check api-secret-show '"placeholder":"sk-proj-{id}"' api "$alice_token" GET /v1/secrets/openai_api_key
check api-secret-list '"names":["anthropic_api_key","openai_api_key"]' api "$alice_token" GET /v1/secrets
check api-secret-ssh "openai_api_key" as alice chloe secret list
check api-org-secret-denied "403" api "$bob_token" PUT '/v1/secrets/openai_api_key?owner=acme' -d '{"value":"x"}'
check api-policy-set '"updated_runners":' api "$alice_token" PUT /v1/policies/acme -d '{"rules":[{"hosts":["api.example"],"action":"allow"}]}'
check api-policy-show '{"rules":[{"hosts":["api.example"],"action":"allow"}]} 200' api "$alice_token" GET /v1/policies/acme
check api-policy-ssh "api.example" as alice chloe policy show acme
check api-policy-denied "only the administrators" api "$bob_token" PUT /v1/policies/acme -d '{"rules":[]}'
check api-policy-invalid "overlaps" api "$alice_token" PUT /v1/policies/acme -d '{"rules":[{"hosts":["hub.docker.com"],"action":"deny"}]}'
check api-delete "204" api "$alice_token" DELETE /v1/workspaces/acme/from-api
check api-deleted "404" api "$alice_token" GET /v1/workspaces/acme/from-api
check api-token-list '"name":"ci"' api "$alice_token" GET /v1/tokens
alice_token_id="$(api "$alice_token" GET /v1/me | sed 's/.*"token":\([0-9]*\).*/\1/')"
check api-bob-cannot-revoke "404" api "$bob_token" DELETE "/v1/tokens/$alice_token_id"
check api-revoke "204" api "$alice_token" DELETE "/v1/tokens/$alice_token_id"
check api-revoked "401" api "$alice_token" GET /v1/me
check api-audited 'via="api"' sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep "management command" | grep -o "via=\"api\"" | tail -1' "$logs/gateway.log"
check api-secret-not-logged "clean" sh -c '! grep -q "real-api-secret" "$0" && echo clean' "$logs/gateway.log"
check api-token-not-logged "clean" sh -c '! grep -q "$1" "$0" && echo clean' "$logs/gateway.log" "$alice_token"

section "control plane down"
# ゲートウェイ (問い合わせ先) を止めると、本物の値が要る要求は 503 で失敗し、要らない要求は通る。
# VM には chloe-runner で vsock から直接入る。
cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-runner
kill "$gateway_pid"; wait "$gateway_pid" 2>/dev/null || true
vm() { "$root/target/debug/chloe-runner" exec --vsock "$mvp/workspaces/$alice_key/vsock.sock" -- bash -lc "$1"; }
check down-secret-503 "503" vm "curl -s -o /dev/null -w '%{http_code}' -H \"x-api-key: \$ANTHROPIC_API_KEY\" https://api.anthropic.com/v1/models"
check down-plain-ok "200" vm "curl -s -o /dev/null -w '%{http_code}' https://httpbin.org/get"

timing_summary

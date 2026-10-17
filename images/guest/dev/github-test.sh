#!/usr/bin/env bash
# 段階 6f と 8 の確認。GitHub のふりをするサーバーに GitHub App の API を答えさせ、
# 所属の確認、chloe github login (Device Flow)、GitHub のトークンの差し替えを確かめる。
#
# アプリは acme と beta の二つの Organization にインストールしてある。
# 利用者は alice (acme の管理者で、team dev のメンバー)、bob (どちらのメンバーでもないが
# allowed_users にいる)、carol (どちらでもない)、dave (beta のメンバー)、
# erin (acme のメンバーで、team dev の maintainer)。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
state="$root/build/dev/github"
logs="$state/logs"
work="$(mktemp -d)"
prefix=chloe-gh-
ssh_port=2224
stub_port=18902

cleanup() {
    kill "${stub_pid:-}" "${gateway_pid:-}" 2>/dev/null || true
    docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

# shellcheck source=gateway-lib.sh
source "$here/gateway-lib.sh"

docker ps -aq --filter "name=^$prefix" | xargs -r docker rm -f >/dev/null
rm -rf "$state"
mkdir -p "$logs" "$work/keys"
for user in alice bob carol dave erin; do
    ssh-keygen -q -t ed25519 -N '' -C "$user" -f "$work/$user"
    cp "$work/$user.pub" "$work/keys/$user.keys"
done

cat > "$work/stub.py" <<'EOF'
import json, sys, threading
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

keys_dir, port, token_log = sys.argv[1], int(sys.argv[2]), sys.argv[3]
polls = {"n": 0}
lock = threading.Lock()
# インストールごとのトークンと、そのトークンで答える所属 (パス → 役割)。
tokens = {"1": "ghs_acme", "2": "ghs_beta"}
memberships = {
    "ghs_acme": {
        "/orgs/acme/memberships/alice": "admin",
        "/orgs/acme/teams/dev/memberships/alice": "member",
        "/orgs/acme/memberships/erin": "member",
        "/orgs/acme/teams/dev/memberships/erin": "maintainer",
    },
    "ghs_beta": {
        "/orgs/beta/memberships/dave": "member",
    },
}

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        auth = self.headers.get("authorization", "")
        path = self.path
        if path.endswith(".keys"):
            try:
                data = open(f"{keys_dir}/{path.strip('/')}", "rb").read()
            except FileNotFoundError:
                return self.reply(404, {})
            self.send_response(200)
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            return self.wfile.write(data)
        if path.startswith("/app/installations?") and auth.startswith("Bearer ey"):
            return self.reply(200, [
                {"id": 1, "account": {"login": "acme", "type": "Organization"}},
                {"id": 2, "account": {"login": "beta", "type": "Organization"}},
            ])
        token = auth.removeprefix("Bearer ")
        if token in memberships:
            role = memberships[token].get(path)
            if role:
                return self.reply(200, {"state": "active", "role": role})
            return self.reply(404, {})
        if path == "/user" and auth == "Bearer ghu_stub_alice":
            return self.reply(200, {"login": "alice"})
        self.reply(404, {"message": "Not Found"})

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0))).decode()
        parts = self.path.split("/")
        if self.path.startswith("/app/installations/") and parts[-1] == "access_tokens":
            # 範囲を指定した発行の本文を残す (確認用)。
            if body:
                with lock, open(token_log, "a") as f:
                    f.write(f"{parts[3]} {body}\n")
            return self.reply(201, {"token": tokens[parts[3]], "expires_at": "2099-01-01T00:00:00Z"})
        if self.path == "/login/device/code":
            return self.reply(200, {"device_code": "dc", "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device", "expires_in": 60, "interval": 1})
        if self.path == "/login/oauth/access_token":
            with lock:
                polls["n"] += 1
                n = polls["n"]
            if n == 1:
                return self.reply(200, {"error": "authorization_pending"})
            return self.reply(200, {"access_token": "ghu_stub_alice", "expires_in": 28800,
                "refresh_token": "ghr_stub", "refresh_token_expires_in": 15897600})
        self.reply(404, {})

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
EOF
python3 -I "$work/stub.py" "$work/keys" "$stub_port" "$work/token-requests.log" &
stub_pid=$!

cat > "$work/github-app.toml" <<EOF
app_id = 1
client_id = "Iv-stub"
private_key = "file:$root/crates/chloe-github/tests/data/test-app-key.pem"
api_url = "http://127.0.0.1:$stub_port"
web_url = "http://127.0.0.1:$stub_port"
EOF

cat > "$work/gateway.toml" <<EOF
listen = "127.0.0.1:$ssh_port"
host_key = "$state/host_key"
github_url = "http://127.0.0.1:$stub_port"
database = "$state/chloe.db"
master_key = "$state/master.key"
idle_timeout_secs = 5
github_app = "$work/github-app.toml"
allowed_users = ["bob"]

[policy]
allow = ["httpbin.org"]

[runner]
guest_dir = "$root/build/guest"
egress_bin = "$root/target/debug/chloe-egress"
state_root = "$state/workspaces"
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
egress_log() { cat "$state/workspaces/$1/logs/requests.jsonl" 2>/dev/null; }
runner_key() { basename "$(dirname "$(dirname "$(grep -l "$1" "$state"/workspaces/*/config/guest.env)")")"; }
last_github_request() { egress_log "$1" | grep '"host":"api.github.com"' | tail -1; }

section "who can log in"
check org-member "alice" as alice 'echo $(id -un) alice'
check allowed-user "ok" as bob 'echo ok'
check outsider "Permission denied" as carol true
check other-org-member "ok" as dave 'echo ok'

section "chloe github login"
# ログインの前から動いているランナーを、接続をつないだまま保つ。
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 120' &
alice_holder=$!
wait_until 60 as alice true
check gh-before-login "absent" as alice 'printenv GH_TOKEN || echo absent'
check status-before "not signed in" as alice chloe github status
check login "Signed in" as alice chloe github login
check status-after "signed in" as alice chloe github status
check token-not-logged "clean" sh -c '! grep -q ghu_stub_alice "$0" && echo clean' "$logs/gateway.log"

section "personal workspace gets the user token"
# ログインする前から動いているランナーでも、新しいシェルには GH_TOKEN が入る (起動し直さない)。
alice_ph="$(as alice 'printenv GH_TOKEN')"
check gh-placeholder "ghu_chloe" echo "$alice_ph"
# ログインの前に起動したランナーには、設定用ドライブにこのプレースホルダーがない。
# 方針を動いたまま入れ替えたランナー (egress.log に記録がある) が、alice のランナーである。
alice_key="$(basename "$(dirname "$(dirname "$(grep -l "policy updated" "$state"/workspaces/*/logs/egress.log)")")")"
check gh-not-restarted "ws-" echo "$alice_key"
kill "$alice_holder" 2>/dev/null || true
check git-helper "password=ghu_chloe" as alice 'printf "protocol=https\nhost=github.com\n\n" | git credential fill'
check api-user "401" as alice "curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: token $alice_ph' https://api.github.com/user"
sleep 1
check api-injected '"injected":["github_token-1"]' last_github_request "$alice_key"

section "organization and team workspaces"
check create-org "created acme/proj" as alice chloe ws create acme/proj
check create-team "created acme/dev/tools" as alice chloe ws create acme/dev/tools
check not-member-create "not a member" as bob chloe ws create acme/other
check not-member-login "Permission denied" as bob+acme/proj true
check list "acme/dev/tools" as alice chloe ws list
org_ph="$(as alice+acme/proj 'printenv GH_TOKEN')"
check org-placeholder "ghu_chloe" echo "$org_ph"
check org-api "401" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: token $org_ph' https://api.github.com/user"
sleep 1
check org-injected '"injected":["github_token-1"]' last_github_request "$(runner_key "$org_ph")"
check org-runner-no-token "0" sh -c '
    docker inspect -f "{{range .Config.Env}}{{println .}}{{end}}" "$0$1" | grep -c "^CHLOE_SECRET_" || true' \
    "$prefix" "$(runner_key "$org_ph")"
check org-token-served "secret served" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep "secret served" | grep "acme/proj" | grep -o "secret served.*github_token" | tail -1' "$logs/gateway.log"
check personal-not-in-org "absent" as alice+acme/proj 'env | grep -q ghu_stub && echo present || echo absent'

section "a second organization"
check create-beta "created beta/app" as dave chloe ws create beta/app
check beta-not-for-acme "not a member" as dave chloe ws create acme/x
check acme-not-for-beta "Permission denied" as alice+beta/app true
check beta-list "beta/app" as dave chloe ws list
beta_ph="$(as dave+beta/app 'printenv GH_TOKEN')"
check beta-placeholder "ghu_chloe" echo "$beta_ph"
check beta-api "401" as dave+beta/app "curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: token $beta_ph' https://api.github.com/user"
check beta-token-served "secret served" sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep "secret served" | grep "beta/app" | grep -o "secret served.*github_token" | tail -1' "$logs/gateway.log"

section "organization settings"
check policy-not-admin "only the administrators" sh -c 'printf "allow = [\"example.org\"]\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/dave" dave@127.0.0.1 chloe policy set beta
check policy-maintainer-not-admin "only the administrators" sh -c 'printf "root = \"persistent\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe policy set acme/dev
check org-blocked-before "000" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check policy-set "saved the settings of acme" sh -c 'printf "allow = [\"example.org\"]\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme
check policy-show "example.org" as erin chloe policy show acme
check org-allowed-after "200" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check team-inherits "200" as alice+acme/dev/tools "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check other-org-unaffected "000" as dave+beta/app "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check team-root-default "saved the settings of acme/dev" sh -c 'printf "root = \"persistent\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme/dev
as alice chloe ws create acme/dev/new >/dev/null
check team-root-applied "acme/dev/new	persistent" as alice chloe ws list
check policy-invalid "invalid settings" sh -c 'printf "allow = 1\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme

section "installation token scope"
check scope-set "saved the settings of acme/dev" sh -c 'printf "root = \"persistent\"\n[github]\nrepositories = [\"tools\"]\npermissions = { contents = \"read\" }\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme/dev
check scope-shown "repositories" as alice chloe policy show acme/dev
team_ph="$(as alice+acme/dev/tools 'printenv GH_TOKEN')"
as alice+acme/dev/tools "curl -s -o /dev/null -H 'Authorization: token $team_ph' https://api.github.com/user" >/dev/null
check scope-requested '1 {"repositories":["tools"],"permissions":{"contents":"read"}}' sh -c 'cat "$0" 2>/dev/null || true' "$work/token-requests.log"
as alice+acme/proj "curl -s -o /dev/null -H 'Authorization: token $org_ph' https://api.github.com/user" >/dev/null
check scope-not-for-org "1" sh -c 'wc -l < "$0"' "$work/token-requests.log"

section "organization and team secrets"
check maintainer-team-secret "saved anthropic_api_key for acme/dev" sh -c 'echo real-team-key | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe secret set --owner acme/dev anthropic_api_key
check maintainer-not-org-secret "cannot change the secrets of acme" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe secret set --owner acme anthropic_api_key
check other-org-secret "cannot change the secrets of beta" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret rm --owner beta openai_api_key
check admin-org-secret "saved openai_api_key for acme" sh -c 'echo real-org-key | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set --owner acme openai_api_key
check team-secret-list "anthropic_api_key" as erin chloe secret list --owner acme/dev
check team-gets-team-secret "sk-ant-api03-chloe" as alice+acme/dev/tools 'printenv ANTHROPIC_API_KEY'
check team-gets-org-secret "sk-proj-chloe" as alice+acme/dev/tools 'printenv OPENAI_API_KEY'
check org-not-team-secret "absent" as alice+acme/proj 'printenv ANTHROPIC_API_KEY || echo absent'
check secrets-not-logged "clean" sh -c '! grep -q "real-team-key\|real-org-key" "$0" && echo clean' "$logs/gateway.log"

timing_summary

#!/usr/bin/env bash
# 段階 6f の確認。GitHub のふりをするサーバーに GitHub App の API を答えさせ、
# 所属の確認、chloe github login (Device Flow)、GitHub のトークンの差し替えを確かめる。
#
# 利用者は alice (acme のメンバーで、team dev のメンバー)、bob (acme のメンバーではないが
# allowed_users にいる)、carol (どちらでもない)。
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
for user in alice bob carol; do
    ssh-keygen -q -t ed25519 -N '' -C "$user" -f "$work/$user"
    cp "$work/$user.pub" "$work/keys/$user.keys"
done

cat > "$work/stub.py" <<'EOF'
import json, sys, threading
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

keys_dir, port = sys.argv[1], int(sys.argv[2])
polls = {"n": 0}
lock = threading.Lock()

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
        installation = auth == "Bearer ghs_stub"
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
        if path == "/orgs/acme/installation" and auth.startswith("Bearer ey"):
            return self.reply(200, {"id": 1})
        if path == "/orgs/acme/members/alice" and installation:
            return self.reply(204)
        if path.startswith("/orgs/acme/members/") and installation:
            return self.reply(404, {})
        if path == "/orgs/acme/teams/dev/memberships/alice" and installation:
            return self.reply(200, {"state": "active"})
        if path.startswith("/orgs/acme/teams/") and installation:
            return self.reply(404, {})
        if path == "/user" and auth == "Bearer ghu_stub_alice":
            return self.reply(200, {"login": "alice"})
        self.reply(404, {"message": "Not Found"})

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0))).decode()
        if self.path == "/app/installations/1/access_tokens":
            return self.reply(201, {"token": "ghs_stub", "expires_at": "2099-01-01T00:00:00Z"})
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
python3 -I "$work/stub.py" "$work/keys" "$stub_port" &
stub_pid=$!

cat > "$work/github-app.toml" <<EOF
app_id = 1
client_id = "Iv-stub"
organization = "acme"
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
idle_timeout_secs = 15
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
sleep 1

ssh_opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o LogLevel=ERROR)
as() { local user="$1"; shift; ssh "${ssh_opts[@]}" -i "$work/${user%%+*}" "$user@127.0.0.1" "$@"; }
egress_log() { cat "$state/workspaces/$1/logs/requests.jsonl" 2>/dev/null; }
runner_key() { basename "$(dirname "$(dirname "$(grep -l "$1" "$state"/workspaces/*/config/guest.env)")")"; }
last_github_request() { egress_log "$1" | grep '"host":"api.github.com"' | tail -1; }

echo "--- who can log in"
check org-member "alice" as alice 'echo $(id -un) alice'
check allowed-user "ok" as bob 'echo ok'
check outsider "Permission denied" as carol true

echo "--- chloe github login"
check status-before "not signed in" as alice chloe github status
check login "Signed in" as alice chloe github login
check status-after "signed in" as alice chloe github status
check token-not-logged "clean" sh -c '! grep -q ghu_stub_alice "$0" && echo clean' "$logs/gateway.log"

echo "--- personal workspace gets the user token"
# ログインする前に起動したランナーは止めてから使う。
as alice chloe ws stop default >/dev/null
alice_ph="$(as alice 'printenv GH_TOKEN')"
check gh-placeholder "ghu_chloe" echo "$alice_ph"
check git-helper "password=ghu_chloe" as alice 'printf "protocol=https\nhost=github.com\n\n" | git credential fill'
check api-user "401" as alice "curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: token $alice_ph' https://api.github.com/user"
sleep 1
check api-injected '"injected":["github_token-1"]' last_github_request "$(runner_key "$alice_ph")"

echo "--- organization and team workspaces"
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
check org-runner-token "installation" sh -c '
    docker inspect -f "{{range .Config.Env}}{{println .}}{{end}}" "$0$1" | grep -q "^CHLOE_SECRET_GITHUB_TOKEN=ghs_stub$" && echo installation' \
    "$prefix" "$(runner_key "$org_ph")"
check personal-not-in-org "absent" as alice+acme/proj 'env | grep -q ghu_stub && echo present || echo absent'

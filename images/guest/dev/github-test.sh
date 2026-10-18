#!/usr/bin/env bash
# 段階 6f と 8 の確認。GitHub のふりをするサーバーに GitHub App の API を答えさせ、
# 所属の確認、SSH の Device Flow と予約したユーザー名、chloe github login、
# GitHub のトークンの差し替えを確かめる。
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
# GitHub に登録していない鍵。予約したユーザー名で、Device Flow を通して結び付ける。
ssh-keygen -q -t ed25519 -N '' -C laptop -f "$work/laptop"
# Device Flow のコードを承認する GitHub のアカウント (スタブが読む)。
echo alice > "$work/approver"
# keyboard-interactive の問いに Enter だけを返す。
printf '#!/bin/sh\necho\n' > "$work/askpass"
chmod +x "$work/askpass"

cat > "$work/stub.py" <<'EOF'
import json, sys, threading
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

keys_dir, port, token_log, approver = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
# Device Flow のコードの数と、一度は「まだ承認されていない」と答えたコード。
codes = {"n": 0, "polled": set()}
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
        if path == "/user" and auth.startswith("Bearer ghu_stub_"):
            return self.reply(200, {"login": auth.removeprefix("Bearer ghu_stub_")})
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
            with lock:
                codes["n"] += 1
                n = codes["n"]
            return self.reply(200, {"device_code": f"dc{n}", "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device", "expires_in": 60, "interval": 1})
        if self.path == "/login/oauth/access_token":
            code = next((p.split("=", 1)[1] for p in body.split("&") if p.startswith("device_code=")), body)
            with lock:
                first = code not in codes["polled"]
                codes["polled"].add(code)
            if first:
                return self.reply(200, {"error": "authorization_pending"})
            # トークンは、そのとき承認するアカウントのものにする。
            login = open(approver).read().strip()
            return self.reply(200, {"access_token": f"ghu_stub_{login}", "expires_in": 28800,
                "refresh_token": "ghr_stub", "refresh_token_expires_in": 15897600})
        self.reply(404, {})

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
EOF
python3 -I "$work/stub.py" "$work/keys" "$stub_port" "$work/token-requests.log" "$work/approver" &
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
allow_personal_rules = true

[[policy.rules]]
hosts = ["httpbin.org"]
action = "allow"

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
# sign_in 鍵 ユーザー名 承認するアカウント コマンド...: Device Flow を通して接続する。
# 鍵を none にすると、公開鍵を使わない。askpass には問いしか渡らないので、失敗の理由
# (端末なら keyboard-interactive の説明に出る) は監査ログで確かめる。
sign_in() {
    local key="$1" user="$2"
    echo "$3" > "$work/approver"
    shift 3
    local opts=(-p "$ssh_port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
        -o IdentitiesOnly=yes -o IdentityAgent=none -o LogLevel=ERROR)
    if [ "$key" = none ]; then
        opts+=(-o PubkeyAuthentication=no)
    else
        opts+=(-i "$work/$key")
    fi
    SSH_ASKPASS="$work/askpass" SSH_ASKPASS_REQUIRE=force DISPLAY=:0 \
        ssh "${opts[@]}" "$user@127.0.0.1" "$@" </dev/null
}
egress_log() { cat "$state/workspaces/$1/logs/requests.jsonl" 2>/dev/null; }
runner_key() { basename "$(dirname "$(dirname "$(grep -l "$1" "$state"/workspaces/*/config/guest.env)")")"; }
last_github_request() { egress_log "$1" | grep '"host":"api.github.com"' | tail -1; }

section "who can log in"
# GitHub に登録した鍵でも、Device Flow を済ませるまでは入れない。
check key-needs-device-flow "Permission denied" as alice true
check wrong-account "Permission denied" sign_in alice alice bob chloe whoami
check wrong-account-reason "signed in to GitHub as bob, not alice" grep -o "signed in to GitHub as bob, not alice" "$logs/gateway.log"
# 失敗しても同じ接続では Device Flow をやり直さない (クライアントが keyboard-interactive を試し直しても)。
check wrong-account-once "count=1" sh -c 'echo "count=$(grep -c "signed in to GitHub as bob, not alice" "$0")"' "$logs/gateway.log"
check org-member "alice" sign_in alice alice alice chloe whoami
check key-after-device-flow "alice" as alice chloe whoami
check allowed-user "bob" sign_in bob bob bob chloe whoami
check outsider "Permission denied" sign_in carol carol carol true
check other-org-member "dave" sign_in dave dave dave chloe whoami
check maintainer "erin" sign_in erin erin erin chloe whoami

section "the reserved user name"
# 予約したユーザー名と GitHub に存在しない名前では、Device Flow で利用者が決まり、鍵を結び付ける。
check reserved-unknown-key "Permission denied" sh -c 'ssh "$@" chloe@127.0.0.1 true' _ "${ssh_opts[@]}" -i "$work/laptop"
check reserved-sign-in "alice" sign_in laptop chloe alice chloe whoami
check reserved-key "alice" sh -c 'ssh "$@" chloe@127.0.0.1 chloe whoami' _ "${ssh_opts[@]}" -i "$work/laptop"
check unknown-name-key "alice" sh -c 'ssh "$@" nosuchuser@127.0.0.1 chloe whoami' _ "${ssh_opts[@]}" -i "$work/laptop"
check reserved-key-listed "SHA256:" sh -c 'ssh "$@" chloe@127.0.0.1 chloe key list' _ "${ssh_opts[@]}" -i "$work/laptop"
check reserved-without-key "dave" sign_in none chloe dave chloe whoami
check reserved-outsider "Permission denied" sign_in none chloe carol true
check reserved-outsider-reason "carol cannot use this server" grep -o "carol cannot use this server" "$logs/gateway.log"
check login-not-for-key "Permission denied" sh -c 'ssh "$@" bob@127.0.0.1 true' _ "${ssh_opts[@]}" -i "$work/laptop"

section "chloe github login"
# ログアウトの前から動いているランナーを、接続をつないだまま保つ。
ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 'sleep 120' &
alice_holder=$!
wait_until 60 as alice true
check logout "signed out" as alice chloe github logout
check key-after-logout "Permission denied" as alice true
# 接続のときの Device Flow でも、動いているランナーにトークンを渡し直す。
check relogin "alice" sign_in alice alice alice chloe whoami
check login "Signed in" as alice chloe github login
check status-after "signed in" as alice chloe github status
check token-not-logged "clean" sh -c '! grep -q ghu_stub_alice "$0" && echo clean' "$logs/gateway.log"

section "personal workspace gets the user token"
# ログインし直す前から動いているランナーでも、新しいシェルには GH_TOKEN が入る (起動し直さない)。
alice_ph="$(as alice 'printenv GH_TOKEN')"
check gh-placeholder "ghu_chloe" echo "$alice_ph"
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
check policy-not-admin "only the administrators" sh -c 'printf "[[rules]]\nhosts = [\"example.org\"]\naction = \"allow\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/dave" dave@127.0.0.1 chloe policy set beta
check policy-maintainer-not-admin "only the administrators" sh -c 'printf "root = \"persistent\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe policy set acme/dev
check org-blocked-before "000" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check policy-set "saved the settings of acme" sh -c 'printf "[[rules]]\nhosts = [\"example.org\"]\naction = \"allow\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme
check policy-show "example.org" as erin chloe policy show acme
check org-allowed-after "200" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check team-inherits "200" as alice+acme/dev/tools "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check other-org-unaffected "000" as dave+beta/app "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.org/"
check team-root-default "saved the settings of acme/dev" sh -c 'printf "root = \"persistent\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme/dev
as alice chloe ws create acme/dev/new >/dev/null
check team-root-applied "acme/dev/new	persistent" as alice chloe ws list
check policy-invalid "invalid settings" sh -c 'printf "rules = 1\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set acme

section "personal settings"
check personal-blocked-before "000" as alice "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.com/"
check personal-policy-set "saved the settings of alice" sh -c 'printf "[[rules]]\nhosts = [\"example.com\"]\naction = \"allow\"\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set
check personal-policy-show "example.com" as alice chloe policy show
check personal-allowed-after "200" as alice "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.com/"
check personal-not-in-org-policy "000" as alice+acme/proj "curl -s -o /dev/null -w '%{http_code}' --max-time 10 https://example.com/"
check personal-no-github "cannot have github" sh -c 'printf "[github]\nrepositories = [\"x\"]\n" | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe policy set
check personal-policy-rm "removed the settings of alice" as alice chloe policy rm

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
check maintainer-team-secret "saved anthropic_api_key for acme/dev" sh -c 'printf "%s\n" "$0" | "$@"' "$(example_secret anthropic_api_key real-team-key)" \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe secret set --owner acme/dev anthropic_api_key --spec
check maintainer-not-org-secret "cannot change the secrets of acme" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/erin" erin@127.0.0.1 chloe secret set --owner acme anthropic_api_key
check other-org-secret "cannot change the secrets of beta" sh -c 'echo x | "$@"' _ \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret rm --owner beta openai_api_key
check admin-org-secret "saved openai_api_key for acme" sh -c 'printf "%s\n" "$0" | "$@"' "$(example_secret openai_api_key real-org-key)" \
    ssh "${ssh_opts[@]}" -i "$work/alice" alice@127.0.0.1 chloe secret set --owner acme openai_api_key --spec
check team-secret-list "anthropic_api_key" as erin chloe secret list --owner acme/dev
check team-gets-team-secret "sk-ant-api03-chloe" as alice+acme/dev/tools 'printenv ANTHROPIC_API_KEY'
check team-gets-org-secret "sk-proj-chloe" as alice+acme/dev/tools 'printenv OPENAI_API_KEY'
check org-not-team-secret "absent" as alice+acme/proj 'printenv ANTHROPIC_API_KEY || echo absent'
check secrets-not-logged "clean" sh -c '! grep -q "real-team-key\|real-org-key" "$0" && echo clean' "$logs/gateway.log"

section "management TUI"
# 端末を割り当てた ssh で chloe だけを実行すると、TUI が開く。キーは fifo から送り、画面を待つ。
# script の端末は大きさが 0 なので、先に決める。
mkfifo "$work/tui-in"
exec 7<>"$work/tui-in"
script -qfec "stty cols 120 rows 40; ssh -tt ${ssh_opts[*]} -i $work/alice alice@127.0.0.1 chloe" /dev/null \
    < "$work/tui-in" > "$work/tui-out" 2>&1 &
tui_shows() { wait_until 20 grep -qa "$1" "$work/tui-out" && echo "$1"; }
tui_keys() { printf '%b' "$1" >&7; }
check tui-opens "acme/proj" tui_shows "acme/proj"
check tui-alt-screen "alternate screen" sh -c 'grep -qa "$(printf "\033\\[?1049h")" "$0" && echo alternate screen' "$work/tui-out"
tui_keys 'n'
check tui-form "New workspace" tui_shows "New workspace"
tui_keys 'tui-ws\x13'
check tui-created "created tui-ws" tui_shows "created tui-ws"
check tui-created-listed "tui-ws" as alice chloe ws list
tui_keys '2o'
check tui-org-secrets "openai_api_key" tui_shows "openai_api_key"
tui_keys '3\r'
# 一覧の先頭の acme の設定を開く。
check tui-org-settings "example.org" tui_shows "example.org"
# Esc と q を一度に送ると Alt+q になるので、どこからでも閉じる Ctrl+C で閉じる。
tui_keys '\x03'
check tui-exits "1049l" tui_shows "$(printf '\033\\[?1049l')"
exec 7>&-
cp "$work/tui-out" "$logs/tui-out"
check tui-audit 'via="tui"' sh -c 'sed "s/\x1b\[[0-9;]*m//g" "$0" | grep "management command" | grep -o "via=\"tui\"" | head -1' "$logs/gateway.log"

timing_summary

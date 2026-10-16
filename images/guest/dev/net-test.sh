#!/usr/bin/env bash
# 段階 2 と 4 の確認。ランナーを proxy モードで起動し、chloe-egress の方針が効いているかを確かめる。
#
# 本物の API キーは使わない。プロキシは偽の秘密情報を埋め込み、
# 差し替えの結果は httpbin.org の応答 (受け取ったヘッダーを返す) で確かめる。
# Claude Code と Codex は、TLS の検証を通って API に届き、認証エラーで止まることを確かめる。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

cat > "$dev_scripts/net.sh" <<'EOF'
#!/bin/bash
source /mnt/lib.sh
PH_TEST=chloe-ph-test-4f1d2c9a7b
PH_ANTHROPIC=sk-ant-api03-chloe-ph-8e2a6f0c31d94b7a
PH_OPENAI=sk-proj-chloe-ph-5b7d3e1f9a2c4068
code() { curl -sS -o /dev/null -w '%{http_code}' --max-time 20 "$@"; }

# CA の公開と取り込み
check ca-publish sh -c 'curl -fsS http://ca.chloe.internal/ca.pem -o /usr/local/share/ca-certificates/chloe.crt && update-ca-certificates 2>&1 | tail -1'

# 許可と遮断
check allowed sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' https://httpbin.org/get)\" = 200 ] && echo 200"
# 許可していない名前は DNS で解決できない。DNS を迂回しても、プロキシが 403 で遮断する。
check_fails dns-not-allowed getent hosts example.com
check dns-allowed getent hosts httpbin.org
check not-allowed sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' --resolve example.com:443:1.1.1.1 https://example.com/)\" = 403 ] && echo 403"
check http2 curl -sS -o /dev/null -w '%{http_version}' --http2 https://httpbin.org/get
check plain-http sh -c "curl -fsS http://httpbin.org/get | grep -o '\"url\": \"[^\"]*\"'"

# 差し替え: 登録した宛先ではプレースホルダーが本物になり、プレースホルダーは届かない
check inject sh -c "curl -fsS -H 'Authorization: Bearer $PH_TEST' https://httpbin.org/headers | grep -o 'Bearer [a-z-]*'"
# 差し替え: 登録していない宛先へのプレースホルダーは遮断する (ヘッダー、URL、本文)
check unbound-header sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' -H 'X-Token: $PH_TEST' https://example.org/)\" = 403 ] && echo 403"
check unbound-query sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' 'https://example.org/?t=$PH_TEST')\" = 403 ] && echo 403"
check unbound-body sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' -d 't=$PH_TEST' https://example.org/)\" = 403 ] && echo 403"
# 差し替え: 登録した宛先でも、規則にないヘッダーのプレースホルダーはそのまま (本物は入らない)
check other-header sh -c "curl -fsS -H 'X-Token: $PH_TEST' https://httpbin.org/headers | grep -o 'chloe-ph-test-[0-9a-f]*'"

# 宛先、メソッド、パスの規則 (policy.dev.json の rules)
status() { curl -sS -o /dev/null -w '%{http_code}' --max-time 20 "$@"; }
check path-allowed sh -c "$(declare -f status); [ \"\$(status https://httpbin.org/anything/allowed/x)\" = 200 ] && echo 200"
check path-denied sh -c "$(declare -f status); [ \"\$(status https://httpbin.org/anything/blocked)\" = 403 ] && echo 403"
check path-method-denied sh -c "$(declare -f status); [ \"\$(status -X DELETE https://httpbin.org/anything/allowed/x)\" = 403 ] && echo 403"
check path-dotdot-denied sh -c "$(declare -f status); [ \"\$(status --path-as-is https://httpbin.org/anything/allowed/../blocked)\" = 403 ] && echo 403"
check path-encoded-denied sh -c "$(declare -f status); [ \"\$(status 'https://httpbin.org/anything/%61llowed/../blocked')\" = 403 ] && echo 403"
check path-other-unaffected sh -c "$(declare -f status); [ \"\$(status https://httpbin.org/get)\" = 200 ] && echo 200"

# SNI と Host の食い違い (ドメインフロンティング)
check sni-mismatch sh -c "[ \"\$(curl -sS -o /dev/null -w '%{http_code}' -H 'Host: example.com' https://httpbin.org/get)\" = 403 ] && echo 403"

# 80 番と 443 番以外への通信、プロキシを経由しない通信は届かない
check_fails other-port curl -sS --max-time 5 http://portquiz.net:8080/
# 接続先は SNI から決めるので、ゲストが別の IP アドレスへ接続しても SNI のホストに届く
check ignores-original-ip curl -fsS -o /dev/null -w '%{http_code}' --resolve httpbin.org:443:1.1.1.1 https://httpbin.org/get

# 中継のみの宛先: 復号しないので、chloe の CA がなくても本物の証明書で通る
check passthrough-cert sh -c "curl -sS -o /dev/null --max-time 20 -w '%{http_code} %{ssl_verify_result}' --cacert /etc/ssl/certs/ISRG_Root_X1.pem https://registry-1.docker.io/v2/ || true"
check docker-pull sh -c 'docker pull -q curlimages/curl'
# CA を入れていないコンテナからの復号対象の宛先は証明書の検証で失敗し、CA を渡せば通る
check_fails container-no-ca docker run --rm curlimages/curl -sS --max-time 20 https://httpbin.org/get
check container-with-ca docker run --rm --entrypoint sh curlimages/curl -c 'curl -fsS http://ca.chloe.internal/ca.pem -o /tmp/ca.pem && curl -fsS -o /dev/null -w "%{http_code}" --cacert /tmp/ca.pem https://httpbin.org/get'

# Claude Code と Codex: TLS を通って API に届き、偽の秘密情報で認証エラーになる
check claude sh -c "ANTHROPIC_API_KEY=$PH_ANTHROPIC timeout 90 claude -p hi < /dev/null 2>&1 | tail -3; true"
check codex sh -c "CODEX_API_KEY=$PH_OPENAI timeout 90 codex exec --skip-git-repo-check hi < /dev/null 2>&1 | tail -5; true"

result done ok
EOF
make_test_drive

export CHLOE_NET_MODE=proxy
rm -f "$dev_logs/requests.jsonl"
boot net net.sh
echo "--- proxy log: $dev_logs/requests.jsonl"

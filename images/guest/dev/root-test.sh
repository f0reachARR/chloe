#!/usr/bin/env bash
# 段階 6 のゲスト側の確認。
#
# - ランナーが渡した環境変数が、guest-agent の起動するプロセスとログインシェルに入る。
# - ルートを保持するモードでは apt で入れたパッケージやルートに置いたファイルが残り、
#   使い捨てのモードでは消える。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# 前の実行で作ったルートの層が残っていると、イメージの版の確認が合わないので、
# ワークスペースのディスクを作り直す (ランナーが起動時に作る)。
rm -f "$dev_state/workspace.ext4"
mkdir -p "$dev_state/config"
echo "CHLOE_TEST_PLACEHOLDER=sk-test-chloe-placeholder" > "$dev_state/config/guest.env"
export CHLOE_GUEST_ENV=/state/config/guest.env

cat > "$dev_scripts/root.sh" <<'EOF'
#!/bin/bash
source /mnt/lib.sh
check exec-env sh -c 'runuser -u user -- printenv CHLOE_TEST_PLACEHOLDER'
check login-env sh -c 'runuser -u user -- bash -lc "printenv CHLOE_TEST_PLACEHOLDER"'
check ca-env sh -c 'runuser -u user -- printenv SSL_CERT_FILE'
case "$1" in
persistent-first)
    check root-layer findmnt -no OPTIONS /
    check apt-install sh -c 'DEBIAN_FRONTEND=noninteractive apt-get -qq update >/dev/null && apt-get -qq install -y sl >/dev/null && echo installed'
    check marker sh -c 'echo kept > /usr/local/bin/chloe-marker && echo written'
    ;;
persistent-second)
    check apt-kept sh -c 'test -x /usr/games/sl && echo kept'
    check marker-kept cat /usr/local/bin/chloe-marker
    check not-outdated sh -c 'test ! -e /etc/chloe/root-outdated && echo current'
    ;;
ephemeral)
    check_fails apt-gone test -x /usr/games/sl
    check_fails marker-gone cat /usr/local/bin/chloe-marker
    ;;
esac
result done ok
EOF
make_test_drive

echo "--- persistent (first boot)"
CHLOE_ROOT_MODE=persistent boot root-persistent-1 root.sh persistent-first
echo "--- persistent (second boot)"
CHLOE_ROOT_MODE=persistent boot root-persistent-2 root.sh persistent-second
echo "--- ephemeral"
CHLOE_ROOT_MODE=ephemeral boot root-ephemeral root.sh ephemeral

#!/bin/sh
# 開発用のランナーのコンテナの入口。コンテナのネットワーク namespace に VM の通信の出口を用意し、
# 状態のディレクトリ (/state) の持ち主の利用者で、プロキシと DNS と Firecracker を起動する。
# systemd で動かすランナー (chloe-runner@.service) と同じスクリプトを使う。
set -eu

owner="$(stat -c %u:%g /state)"
/opt/chloe/chloe-runner-net "${owner%%:*}"
# 方針を渡さずに起動したとき (run.sh) は、開発用の方針を使う。
export CHLOE_POLICY="${CHLOE_POLICY:-/opt/chloe/policy.dev.json}"
exec setpriv --reuid="${owner%%:*}" --regid="${owner##*:}" --groups="$(stat -c %g /dev/kvm)" \
    /opt/chloe/chloe-runner-start /state

#!/usr/bin/env bash
# VM を使う確認をまとめて実行し、結果と所要時間をまとめて出す。
#
#   test-all.sh [名前...]   (名前は smoke net root disk mvp github lifecycle node。省略するとすべて)
#
# どの確認も VM の名前、状態のディレクトリ、ポートが別なので、すべて並べて実行する。
# smoke、net、root、disk には、CHLOE_DEV_NAME と CHLOE_DEV_STATE で別の VM と状態を使わせる。
# 各スクリプトの出力は build/dev/test-logs/<名前>.log に置く。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$root/build/dev/test-logs"
mkdir -p "$out"

names=("$@")
[ ${#names[@]} -gt 0 ] || names=(smoke net root disk mvp github lifecycle node)
selected() { local n; for n in "${names[@]}"; do [ "$n" = "$1" ] && return 0; done; return 1; }

# 並べて実行するスクリプトが同時にビルドしないよう、先にビルドしておく。
cargo build --quiet --manifest-path "$root/Cargo.toml" -p chloe-gateway -p chloe-egress -p chloe-runner -p chloe-node
docker build -q -t chloe-dev-runner -f "$here/runner.Dockerfile" "$here/.." >/dev/null

run() {
    local name="$1" start code
    start=$(date +%s)
    code=0
    CHLOE_DEV_NAME="chloe-dev-$name" CHLOE_DEV_STATE="$root/build/dev/$name" \
        "$here/$name-test.sh" > "$out/$name.log" 2>&1 || code=$?
    echo "$name $code $(( $(date +%s) - start ))" > "$out/$name.status"
}

start=$(date +%s)
pids=()
for name in smoke net root disk mvp github lifecycle node; do
    if selected "$name"; then
        run "$name" &
        pids+=($!)
    fi
done
wait "${pids[@]}"

status=0
printf '%-10s %5s %5s %6s\n' test ok fail secs
for name in "${names[@]}"; do
    read -r _ code secs < "$out/$name.status"
    ok=$(grep -c '==RESULT==.*: ok' "$out/$name.log" || true)
    fail=$(grep -c '==RESULT==.*: FAIL' "$out/$name.log" || true)
    printf '%-10s %5s %5s %6s%s\n' "$name" "$ok" "$fail" "$secs" "$([ "$code" = 0 ] || echo "  (exit $code)")"
    if [ "$fail" != 0 ] || [ "$code" != 0 ]; then
        status=1
        grep '==RESULT==.*: FAIL' "$out/$name.log" | cut -c1-200 | sed 's/^/    /' || true
    fi
done
echo "total $(( $(date +%s) - start ))s"
exit "$status"

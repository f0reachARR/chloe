# ゲストのイメージのビルドスクリプトの共通部分。source して使う。
#
#   steps 6                 全体の手順の数を決める
#   step "カーネルを取ってくる"  次の手順を始める (前の手順の所要時間を出す)
#   finish                  最後の手順の所要時間と、全体の所要時間を出す
#
# 出力は標準エラーに出す (成果物の場所などの標準出力と混ぜない)。

build_start=$SECONDS
step_start=$SECONDS
step_index=0
step_total=0
step_name=""

steps() { step_total="$1"; }

# 経過した秒数を "1m23s" の形にする。
human_secs() {
    local s="$1"
    if [ "$s" -ge 60 ]; then
        printf '%dm%02ds' $((s / 60)) $((s % 60))
    else
        printf '%ds' "$s"
    fi
}

end_step() {
    if [ -n "$step_name" ]; then
        printf '      done in %s\n' "$(human_secs $((SECONDS - step_start)))" >&2
    fi
}

step() {
    end_step
    step_index=$((step_index + 1))
    step_name="$1"
    step_start=$SECONDS
    printf '==> [%d/%d] %s\n' "$step_index" "$step_total" "$step_name" >&2
}

finish() {
    end_step
    step_name=""
    printf '==> finished in %s\n' "$(human_secs $((SECONDS - build_start)))" >&2
}

# 端末なら curl の進み具合の表示を、そうでなければ何も出さない。
curl_progress() {
    if [ -t 2 ]; then
        echo --progress-bar
    else
        echo --silent --show-error
    fi
}

# コマンドの出力をログに書き、パターンに一致した行の数を一定の間隔で表示する。
# 失敗したらログの末尾を出す。
#   with_count <ログ> <パターン> <名詞> コマンド...
with_count() {
    local log="$1" pattern="$2" noun="$3"; shift 3
    local status=0
    "$@" > "$log" 2>&1 &
    local pid=$! last=-1
    while kill -0 "$pid" 2>/dev/null; do
        local count
        count="$(grep -cE "$pattern" "$log" 2>/dev/null || true)"
        if [ "$count" != "$last" ]; then
            if [ -t 2 ]; then
                printf '\r      %s %s (%s)' "$count" "$noun" "$(human_secs $((SECONDS - step_start)))" >&2
            else
                printf '      %s %s (%s)\n' "$count" "$noun" "$(human_secs $((SECONDS - step_start)))" >&2
            fi
            last="$count"
        fi
        sleep "$([ -t 2 ] && echo 1 || echo 30)"
    done
    wait "$pid" || status=$?
    [ -t 2 ] && printf '\n' >&2
    if [ "$status" -ne 0 ]; then
        echo "failed (exit $status); the last lines of $log:" >&2
        tail -n 40 "$log" >&2
        return "$status"
    fi
}

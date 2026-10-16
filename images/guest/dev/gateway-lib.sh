# ゲートウェイを通す確認スクリプト (mvp、github、lifecycle) の共通部分。source して使う。
#
# 結果は "==RESULT== 名前: ok|FAIL 出力 (+秒)" の行で出す。秒は、直前の確認からの経過時間。
# 節 (section) ごとの所要時間を、終わりにまとめて出す。

t_start="$(date +%s.%N)"
t_last="$t_start"
t_section=""
t_section_start="$t_start"
t_summary=()

elapsed() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.1f", b - a }'; }

result() { echo "==RESULT== $1: $2"; }

# check 名前 期待する文字列 コマンド...: 出力に期待する文字列が含まれれば ok。
check() {
    local name="$1" expected="$2"; shift 2
    local out now
    out="$("$@" 2>&1)" || true
    now="$(date +%s.%N)"
    local took
    took="$(elapsed "$t_last" "$now")"
    t_last="$now"
    if [[ "$out" == *"$expected"* ]]; then
        result "$name" "ok ${out//$'\n'/ | } (+${took}s)"
    else
        result "$name" "FAIL expected '$expected', got: ${out//$'\n'/ | } (+${took}s)"
    fi
}

# wait_until 秒 コマンド...: コマンドが成功するまで待つ。時間内に成功しなければ失敗を返す。
wait_until() {
    local limit="$1"; shift
    local deadline
    deadline="$(awk -v n="$(date +%s.%N)" -v l="$limit" 'BEGIN { printf "%.3f", n + l }')"
    until "$@" >/dev/null 2>&1; do
        if awk -v n="$(date +%s.%N)" -v d="$deadline" 'BEGIN { exit !(n > d) }'; then
            return 1
        fi
        sleep 0.2
    done
}

# section 名前: 節を始める。前の節の所要時間を記録する。
section() {
    local now
    now="$(date +%s.%N)"
    if [ -n "$t_section" ]; then
        t_summary+=("$(elapsed "$t_section_start" "$now")s $t_section")
    fi
    t_section="$1"
    t_section_start="$now"
    echo "--- $1"
}

# 節ごとの所要時間を出す。スクリプトの終わりに呼ぶ。
timing_summary() {
    section ""
    echo "--- timing (total $(elapsed "$t_start" "$(date +%s.%N)")s)"
    printf '  %s\n' "${t_summary[@]}"
}

# ゲートウェイのログ (色を除く)。
strip_colors() { sed 's/\x1b\[[0-9;]*m//g' "$@"; }

# count_at_least 数 ファイル パターン: ファイルにパターンの行が数以上あれば成功。
count_at_least() {
    local n="$1" file="$2" pattern="$3"
    [ "$(strip_colors "$file" 2>/dev/null | grep -c -- "$pattern")" -ge "$n" ]
}

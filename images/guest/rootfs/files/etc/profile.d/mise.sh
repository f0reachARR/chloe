# mise で入れたツールを使えるようにする。ツールは利用者のホームディレクトリ (~/.local/share/mise) に
# 入り、ワークスペースのディスクに残る。ssh <ホスト> 'コマンド' もログインシェルで動くので、ここで読む。
# 対話のシェルでは、ディレクトリごとの設定 (mise.toml) も効かせる。
if [ -n "${BASH_VERSION:-}" ] && command -v mise > /dev/null; then
    case $- in
    *i*) eval "$(mise activate bash)" ;;
    *) eval "$(mise activate bash --shims)" ;;
    esac
fi

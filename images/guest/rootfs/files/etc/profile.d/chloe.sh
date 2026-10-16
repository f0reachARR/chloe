# ランナーが渡した環境変数 (プレースホルダーなど) をログインシェルに入れる。
# guest-agent が起動するプロセスにはすでに入っているが、VM の中でログインシェルを
# 起動し直したときのために、ここでも読む。
if [ -r /run/chloe/env ]; then
    set -a
    . /run/chloe/env
    set +a
fi

# OS の信頼ストアを読まないツールのために、chloe の CA を含む信頼ストアを指す。
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export REQUESTS_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
export NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt
export CODEX_CA_CERTIFICATE=/etc/ssl/certs/ca-certificates.crt

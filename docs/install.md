# インストール

chloe を一台のホストで動かす手順と、ランナーを複数のホストに置く手順を書く。

ランナー（ワークスペースの VM とプロキシ）は、今は開発用の実装だけがある。
ワークスペースごとに特権コンテナ（`chloe-dev-runner`）を起動し、その中で Firecracker とプロキシを動かす。
jailer による Firecracker の隔離はまだないので、ホストは chloe 専用にし、信頼できる利用者に限って使う。

## ホストの要件

- **CPU と OS**：x86_64 の Linux。KVM が使えること（`/dev/kvm` がある）。物理サーバーか、入れ子の仮想化を許した VM。
- **Docker**：ランナーは特権コンテナで動くので、`--privileged` と `/dev/kvm`、`/dev/net/tun` のデバイスを渡せること。ゲートウェイ（一台のとき）か `chloe-node`（複数のホストのとき）を動かす利用者が、`docker` を使えること。
- **ディスク**：ゲストのイメージに約 3GB。ワークスペースごとに、ワークスペースのディスク（20GB）と Docker のディスク（40GB）を疎なファイルで作る。使った分だけ実際に消費する。
- **ネットワーク**：ビルドのときと、VM が許可リストの宛先へつなぐときに、外へ出られること。

ワークスペースの VM は 4 vCPU、メモリ 4GiB で起動する（今は設定で変えられない）。

## ビルドに要るもの

- **Rust**：[rustup](https://rustup.rs/) を入れる。リポジトリの `rust-toolchain.toml` が、stable と、ゲストのバイナリに使う `x86_64-unknown-linux-musl` のターゲットを選ぶ。
- **BoringSSL のビルドに要るもの**：プロキシ（`chloe-egress`）は BoringSSL をソースからビルドする。C と C++ のコンパイラ、cmake、libclang、git が要る。Debian と Ubuntu では次のとおり。

  ```sh
  sudo apt-get install build-essential cmake clang libclang-dev git
  ```

- **Docker**：カーネルとルートファイルシステムは Docker のコンテナの中でビルドする。

`chloe-egress` はホストでビルドし、ランナーのコンテナ（Ubuntu 26.04）に渡して動かす。
ホストの glibc がコンテナより新しいと動かないので、Ubuntu 26.04 か、それより古い glibc のホストでビルドする。

## ビルド

リポジトリのルートで実行する。
成果物は `build/` と `target/` に出る。

```sh
# ゲストのカーネル (Linux 6.18 系) と、ルートファイルシステム (Ubuntu 26.04、Claude Code、Codex、Docker)。
images/guest/kernel/build.sh
images/guest/rootfs/build.sh

# ランナーのコンテナのイメージ (Firecracker と dnsmasq を含む)。
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest/dev

# ゲートウェイ、プロキシ、ホストの常駐プロセス。
cargo build --release -p chloe-gateway -p chloe-egress -p chloe-node
```

ルートファイルシステムには、ビルドしたときの最新の Claude Code（stable）と、`rootfs/Dockerfile` で版を決めた Codex が入る。
更新するときはビルドし直す（「更新」を参照）。

## 一台で動かす

### 設定

設定の例 `images/guest/dev/gateway.example.toml` を写して編集する。
項目の一覧は [設定](configuration.md) にある。

```sh
mkdir -p /etc/chloe
cp images/guest/dev/gateway.example.toml /etc/chloe/gateway.toml
```

少なくとも次を書き換える。

- `listen`：SSH の待ち受け。利用者に公開するなら `0.0.0.0:22` などにする。
- `host_key`、`database`、`master_key`、`audit_log`：状態の置き場所（なければ作る）。
- `allowed_users` と `admins`：ログインを許す GitHub のログイン名と、サーバーの運用者。
- `[runner]` の `guest_dir`、`egress_bin`、`state_root`：ビルドした `build/guest`、`target/release/chloe-egress`、ワークスペースの状態の置き場所。相対パスは起動したディレクトリから解決する。
- `[policy]`：すべてのワークスペースに共通の許可リスト。

### 起動

```sh
target/release/chloe-gateway --config /etc/chloe/gateway.toml
```

ホスト鍵、データベース、マスター鍵は、初めて起動したときに作る。
利用者は、GitHub に登録した公開鍵で入る。

```sh
ssh <GitHub のログイン名>@<ホスト>
```

最初の接続でワークスペースの VM を起動する（数秒かかる）。
使い方は [使い方](usage.md) にある。

### GitHub App

GitHub App を設定すると、Organization のメンバーがログインでき、公開鍵を登録していない利用者は Device Flow でログインでき、VM の中で GitHub を使える。
登録の手順は [GitHub App の準備](github-app.md) にある。
アプリの設定ファイルのパスを、ゲートウェイの設定の `github_app` に書く。

### systemd の unit の例

確かめていない例である。
`docker` を使える利用者で動かす。

```ini
[Unit]
Description=chloe gateway
After=network-online.target docker.service
Wants=network-online.target

[Service]
User=chloe
WorkingDirectory=/opt/chloe
ExecStart=/opt/chloe/bin/chloe-gateway --config /etc/chloe/gateway.toml
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

ゲートウェイを再起動しても、動いているワークスペースは止まらない。
起動したゲートウェイが引き取り、本物の値の問い合わせに答え続ける。

## 状態の置き場所とバックアップ

| 置き場所 | 中身 | バックアップ |
| --- | --- | --- |
| `database` | ワークスペース、シークレット（暗号化したもの）、設定、トークンのハッシュ、ホストの一覧 | 要る。`master_key` と組で |
| `master_key` | シークレットを暗号化する鍵 | 要る。失うと、保存したシークレットを読めなくなる |
| `host_key` | SSH のホスト鍵 | 要る。変わると、利用者の `known_hosts` と合わなくなる |
| `[cluster]` の `pki_dir` | CA の証明書と鍵 | 要る。失うと、すべてのホストに参加し直させる |
| `audit_log` | 監査ログ（JSON Lines） | 運用の方針による |
| `[runner]` の `state_root`（複数のホストでは各ホストの `state_root`） | ワークスペースごとのディスク、プロキシの CA、HTTP のログ | ワークスペースの中身を残したいなら要る |

`database` は SQLite で、WAL のモードで使う。
動かしたまま写すときは `sqlite3 chloe.db ".backup backup.db"` を使う。

`chloe ws rm` でワークスペースを消しても、`state_root` のディスクは残る（消すのは手で行う）。

## 複数のホストに置く

ゲートウェイ（コントロールプレーン）を一か所に置き、各ホストで `chloe-node` を動かす。
コントロールプレーンは CA を持ち、ホストの証明書を発行する。
コントロールプレーンとホストのあいだは相互 TLS でつなぐ。

### ポート

| 向き | ポート（例） | 用途 |
| --- | --- | --- |
| 利用者 → コントロールプレーン | 22 | SSH |
| ホスト → コントロールプレーン | 8444 | 内部 API（参加、本物の値の問い合わせの中継） |
| コントロールプレーン → ホスト | 8445 | `chloe-node` の API（ランナーの操作、端末の中継） |

### コントロールプレーン

ゲートウェイの設定から `[runner]` を消し、`[cluster]` を書く。

```toml
[cluster]
listen = "0.0.0.0:8444"
# ホストがつなぐ名前か IP アドレス。内部 API の証明書に入れる。
server_names = ["cp.example.com"]
pki_dir = "/var/lib/chloe/pki"
```

起動すると、CA がなければ作る。
ホストを登録し、参加用のトークンを作る。
URL は、コントロールプレーンがそのホストの API を呼ぶときのもので、ホストの証明書にこのホスト名が入る。

```sh
chloe-gateway --config /etc/chloe/gateway.toml node add n1 --url https://n1.example.com:8445
```

表示されたトークンは一度だけ使え、1 時間で失効する（`join_ttl_secs` で変えられる）。
トークンには CA の証明書の指紋が入っていて、ホストは最初の接続でコントロールプレーンを確かめる。

### ホスト

各ホストに、ビルドしたものを置く。
今は配る仕組みがないので、手で写す。

- `build/guest/`（カーネルとルートファイルシステム）
- `chloe-egress` と `chloe-node` のバイナリ
- `chloe-dev-runner` のイメージ（ホストでビルドするか、`docker save` と `docker load` で写す）

参加して、証明書を受け取る。

```sh
chloe-node join --server https://cp.example.com:8444 --token <トークン> --dir /var/lib/chloe-node/tls
```

設定の例 `images/guest/dev/node.example.toml` を写して編集し、起動する。
`chloe-node` は `docker` を使える利用者で動かす。

```sh
chloe-node run --config /etc/chloe/node.toml
```

`chloe-gateway node list` で、参加したホストを確かめられる。

### 置き場所

ワークスペースは、最初に起動したホストに結びつく（ディスクがそのホストにあるので）。
新しいワークスペースは、置いたワークスペースがもっとも少ないホストに置く。
ホストをまたいでワークスペースを移す手段は、まだない。

ホストがつながらないあいだ、そのホストのワークスペースには入れないが、ほかのホストのワークスペースは使える。
コントロールプレーンは、つながらないホストのランナーを止まったとはみなさず、つながったら元どおり扱う。

ホストの登録を消すには、そのホストに置いたワークスペースを先に消す。

```sh
chloe-gateway --config /etc/chloe/gateway.toml node rm n1
```

## 管理 API を有効にする

ゲートウェイの設定に `[api]` を書く。
証明書と鍵を書けば HTTPS で、なければ HTTP で待ち受ける。
HTTP で待ち受けるときは、ローカルからだけ使うか、TLS を終端するリバースプロキシの後ろに置く。

```toml
[api]
listen = "127.0.0.1:8080"
# tls_cert = "/etc/chloe/api.crt"
# tls_key = "/etc/chloe/api.key"
```

使い方は [管理 API](api.md) にある。

## 更新

- **バイナリ**：ゲートウェイを止めて入れ替え、起動する。データベースのマイグレーションは起動のときに適用する。動いているワークスペースは止まらない。
- **ゲストのイメージ**：`images/guest/rootfs/build.sh` でビルドし直すと、次に起動するワークスペースから新しいイメージを使う。動いているワークスペースは、`chloe ws stop` で止めるまで古いイメージのまま動く。ルートを保持するワークスペース（`persistent`）は、残した変更を新しいイメージの上に重ねて起動し、層を作ったときとイメージの版が違うことを VM の中の `/etc/chloe/root-outdated` に書く（版が違うときに層をどう扱うかは、まだ決めていない）。
- **ランナーのコンテナのイメージ**：`chloe-dev-runner` をビルドし直したら、動いているワークスペースを一度止める。古いイメージで動いているランナーは、設定の変更を受けたときに VM の DNS の許可リストを作り直せない。

## 困ったとき

- **ログ**：ゲートウェイと `chloe-node` は標準エラーにログを出す（`RUST_LOG` で詳しさを変えられる）。ワークスペースごとのプロキシのログ（`egress.log`）、HTTP のログ（`requests.jsonl`）、DNS のログ（`dnsmasq.log`）は `state_root/<ws-番号>/logs/` にある。
- **VM が起動しない**：`/dev/kvm` を使えるか、`docker run --privileged` ができるかを確かめる。ランナーのコンテナの出力（`docker logs chloe-ws-<番号>`）に、Firecracker とゲストのシリアルコンソールの出力が出る。
- **VM から外につながらない**：許可リストにない宛先は、名前の解決からできない。`requests.jsonl` と `dnsmasq.log` で、遮断した宛先がわかる。

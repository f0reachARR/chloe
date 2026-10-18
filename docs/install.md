# インストール

chloe を一台のホストで動かす手順と、ランナーを複数のホストに置く手順を書く。

ランナー（ワークスペースの VM とプロキシ）は、今は開発用の実装だけがある。
ワークスペースごとに特権コンテナ（`chloe-dev-runner`）を起動し、その中で Firecracker とプロキシを動かす。
jailer による Firecracker の隔離はまだないので、ホストは chloe 専用にし、信頼できる利用者に限って使う。

## ホストの要件

- **CPU と OS**：x86_64 の Linux。KVM が使えること（`/dev/kvm` がある）。物理サーバーか、入れ子の仮想化を許した VM。
- **Docker**：ランナーは特権コンテナで動くので、`--privileged` と `/dev/kvm`、`/dev/net/tun` のデバイスを渡せること。ゲートウェイ（一台のとき）か `chloe-node`（複数のホストのとき）を動かす利用者が、`docker` を使えること。
- **ディスク**：ゲストのイメージに約 3GB。ワークスペースごとに、ワークスペースのディスク（既定 20GiB）と Docker のディスク（既定 40GiB）を疎なファイルで作る。使った分だけ実際に消費する。大きさの既定と上限は設定の `[disks]` で変えられる。
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
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest

# ゲートウェイ、プロキシ、ホストの常駐プロセス。
cargo build --release -p chloe-gateway -p chloe-egress -p chloe-node
```

どちらのスクリプトも、手順ごとに番号と所要時間を表示する。
カーネルのコンパイルは数分かかり、そのあいだはコンパイルしたファイルの数を表示する（全体のログは `build/guest/kernel/build.log`）。
カーネルのソースとビルドの途中のファイルは `build/cache/` に残り（約 2GB）、二回目からは差分だけをビルドする。

ルートファイルシステムには、ビルドしたときの最新の Claude Code（stable）と、`rootfs/Dockerfile` で版を決めた Codex が入る。
更新するときはビルドし直す（「更新」を参照）。

## 配置

ビルドしたものは、Debian と Ubuntu の慣習に合わせて次の場所に置く。
設定の例（`deploy/gateway.toml`、`deploy/node.toml`）と systemd の unit（`deploy/systemd/`）は、この配置で書いてある。

| 置き場所 | 中身 |
| --- | --- |
| `/usr/local/bin/` | `chloe-gateway`、`chloe-node` |
| `/usr/local/lib/chloe/` | `chloe-egress`（ランナーのコンテナに渡すもので、ホストでは実行しない）と、`guest/`（ゲストのカーネルとルートファイルシステム） |
| `/etc/chloe/` | 設定（`gateway.toml`、`node.toml`、GitHub App の設定と秘密鍵、管理 API の証明書と鍵） |
| `/var/lib/chloe/` | ゲートウェイの状態（ホスト鍵、データベース、マスター鍵、CA、ワークスペースの状態） |
| `/var/lib/chloe-node/` | `chloe-node` の状態（証明書と鍵、ワークスペースの状態） |
| `/var/log/chloe/` | 監査ログ |

ゲストのルートファイルシステムは約 2.5GB ある。
`/usr` の容量が足りなければ、`/var/lib/chloe/guest` などに置き、設定の `guest_dir` を書き換える。

### 動かす利用者

ゲートウェイと `chloe-node` は、ランナーのコンテナを起動するために `docker` を使う。
unit の例は、`docker` のグループに入れた専用の利用者 `chloe` で動かす。
`docker` のグループに入った利用者は root と同じことができるので、利用者を分けてもホストを守る効果はほとんどなく、root で動かしてもよい。
root で動かすときは、unit の `User=`、`Group=`、`AmbientCapabilities=` を消す。

```sh
sudo useradd --system --no-create-home --home-dir /var/lib/chloe --shell /usr/sbin/nologin --groups docker chloe
```

`/dev/kvm` と `/dev/net/tun` を開くのはランナーのコンテナなので、`chloe` を `kvm` のグループに入れる必要はない。
ランナーのコンテナは root で動くが、`state_root` に作るファイルの持ち主を `state_root` の下のディレクトリの持ち主に合わせるので、ワークスペースの状態は `chloe` の利用者で消せる。

### 置く

リポジトリのルートで実行する。

```sh
sudo install -m 755 target/release/chloe-gateway target/release/chloe-node /usr/local/bin/
sudo install -D -m 755 target/release/chloe-egress /usr/local/lib/chloe/chloe-egress
sudo install -D -m 644 build/guest/kernel/vmlinux /usr/local/lib/chloe/guest/kernel/vmlinux
sudo install -D -m 644 build/guest/rootfs.ext4 /usr/local/lib/chloe/guest/rootfs.ext4
sudo install -d -m 755 /etc/chloe
sudo install -m 644 deploy/systemd/chloe-gateway.service deploy/systemd/chloe-node.service /etc/systemd/system/
sudo systemctl daemon-reload
```

ゲストのイメージは `cp` ではなく `install` で置く（理由は「更新」にある）。

## 一台で動かす

### 設定

設定の例 `deploy/gateway.toml` を写して編集する。
項目の一覧は [設定](configuration.md) にある。

```sh
sudo install -m 644 deploy/gateway.toml /etc/chloe/gateway.toml
```

少なくとも次を書き換える。

- `listen`：SSH の待ち受け。例は `0.0.0.0:22` で、ホストの sshd と重なる。sshd を別のポートに移すか、ゲートウェイを別のポートで待ち受ける。
- `allowed_users` と `admins`：ログインを許す GitHub のログイン名と、サーバーの運用者。
- `[[policy.rules]]`：すべてのワークスペースに共通の、外への通信の規則。どの規則にも一致しない通信は遮断する。

状態の置き場所（`host_key`、`database`、`master_key`、`audit_log`、`[runner]` の `state_root`）は、unit の `StateDirectory=` と `LogsDirectory=` が作る `/var/lib/chloe` と `/var/log/chloe` の下にしてある。
別の場所に変えるときは、`chloe` の利用者が書けるディレクトリにする。

### 起動

```sh
sudo systemctl enable --now chloe-gateway
journalctl -u chloe-gateway -f
```

ホスト鍵、データベース、マスター鍵は、初めて起動したときに作る。
GitHub App を設定していれば、利用者は `chloe` のユーザー名で入り、初回は Device Flow で GitHub のアカウントを確かめる。
GitHub App がなければ、GitHub に登録した公開鍵で入る。

```sh
ssh chloe@<ホスト>                    # GitHub App があるとき
ssh <GitHub のログイン名>@<ホスト>     # GitHub App がないとき
```

最初の接続でワークスペースの VM を起動する（数秒かかる）。
使い方は [使い方](usage.md) にある。

ゲートウェイを再起動しても、動いているワークスペースは止まらない。
起動したゲートウェイが引き取り、本物の値の問い合わせに答え続ける。

unit の例は、まだ実際のホストで確かめていない。

### 運用のコマンド

`node add` など、`chloe-gateway` のサブコマンドを手で実行するときは、`chloe` の利用者で実行する。
root で実行すると、SQLite がデータベースの横に作るファイル（`chloe.db-wal` など）が root のものになり、ゲートウェイが開けなくなることがある。

```sh
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node list
```

### GitHub App

GitHub App を設定すると、Organization のメンバーがログインでき、ログインには Device Flow が必須になり、VM の中で GitHub を使える。
登録の手順は [GitHub App の準備](github-app.md) にある。
アプリの設定と秘密鍵を `/etc/chloe/` に置き、ゲートウェイの設定の `github_app` にそのパスを書く。

## 状態の置き場所とバックアップ

例の配置での場所を括弧に書く。

| 置き場所 | 中身 | バックアップ |
| --- | --- | --- |
| `database`（`/var/lib/chloe/chloe.db`） | ワークスペース、シークレット（暗号化したもの）、設定、トークンのハッシュ、ホストの一覧 | 要る。`master_key` と組で |
| `master_key`（`/var/lib/chloe/master.key`） | シークレットを暗号化する鍵 | 要る。失うと、保存したシークレットを読めなくなる |
| `host_key`（`/var/lib/chloe/host_key`） | SSH のホスト鍵 | 要る。変わると、利用者の `known_hosts` と合わなくなる |
| `[cluster]` の `pki_dir`（`/var/lib/chloe/pki`） | CA の証明書と鍵 | 要る。失うと、すべてのホストに参加し直させる |
| `audit_log`（`/var/log/chloe/audit.jsonl`） | 監査ログ（JSON Lines） | 運用の方針による |
| `[runner]` の `state_root`（`/var/lib/chloe/workspaces`、複数のホストでは各ホストの `/var/lib/chloe-node/workspaces`） | ワークスペースごとのディスク、プロキシの CA、HTTP のログ | ワークスペースの中身を残したいなら要る |

`database` は SQLite で、WAL のモードで使う。
動かしたまま写すときは、`sqlite3` の `.backup` を `chloe` の利用者で使う。

```sh
sudo -u chloe sqlite3 /var/lib/chloe/chloe.db ".backup /var/lib/chloe/chloe.db.bak"
```

`chloe ws rm` でワークスペースを消しても、`state_root` のディスクは残る。
一緒に消すには `chloe ws rm NAME --disks` とする。

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
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node add n1 --url https://n1.example.com:8445
```

表示されたトークンは一度だけ使え、1 時間で失効する（`join_ttl_secs` で変えられる）。
トークンには CA の証明書の指紋が入っていて、ホストは最初の接続でコントロールプレーンを確かめる。

### ホスト

各ホストに、ビルドしたものを「配置」のとおりに置き、利用者 `chloe` を作る（`chloe-gateway` と `chloe-gateway.service` は要らない）。
`useradd` の `--home-dir` は `/var/lib/chloe-node` にする。
今は配る仕組みがないので、手で写す。
`chloe-dev-runner` のイメージも要る（ホストでビルドするか、`docker save` と `docker load` で写す）。

参加して、証明書を受け取る。
`/var/lib` には `chloe` の利用者が書けないので、状態のディレクトリを先に作っておく（unit の `StateDirectory=` が作るのは起動のときである）。

```sh
sudo install -d -o chloe -g chloe -m 700 /var/lib/chloe-node
sudo -u chloe chloe-node join --server https://cp.example.com:8444 --token <トークン> --dir /var/lib/chloe-node/tls
```

設定の例 `deploy/node.toml` を写し、`control_plane` を書き換えて起動する。

```sh
sudo install -m 644 deploy/node.toml /etc/chloe/node.toml
sudo systemctl enable --now chloe-node
```

コントロールプレーンで `sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node list` を実行すると、参加したホストを確かめられる。

### 置き場所

ワークスペースは、最初に起動したホストに結びつく（ディスクがそのホストにあるので）。
新しいワークスペースは、置いたワークスペースがもっとも少ないホストに置く。
ホストをまたいでワークスペースを移す手段は、まだない。

ホストがつながらないあいだ、そのホストのワークスペースには入れないが、ほかのホストのワークスペースは使える。
コントロールプレーンは、つながらないホストのランナーを止まったとはみなさず、つながったら元どおり扱う。

ホストの登録を消すには、そのホストに置いたワークスペースを先に消す。

```sh
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node rm n1
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

鍵は `chloe` の利用者が読めるようにする（`sudo install -m 640 -g chloe api.key /etc/chloe/api.key`）。

使い方は [管理 API](api.md) にある。

## 更新

- **バイナリ**：「配置」と同じ `install` で置き直し、`sudo systemctl restart chloe-gateway`（ホストでは `chloe-node`）で起動し直す。データベースのマイグレーションは起動のときに適用する。動いているワークスペースは止まらない。
- **ゲストのイメージ**：`images/guest/rootfs/build.sh` でビルドし直し、「配置」と同じ `install` で置き直すと、次に起動するワークスペースから新しいイメージを使う。`install` は古いファイルを消してから新しいファイルを作るので、動いている VM は開いている古いファイルを読み続けられる。`cp` は同じファイルの中身を上書きするので、動いている VM のルートファイルシステムを壊す。動いているワークスペースは、`chloe ws stop` で止めるまで古いイメージのまま動く。ルートを保持するワークスペース（`persistent`）は、残した変更を新しいイメージの上に重ねて起動し、層を作ったときとイメージの版が違うことを VM の中の `/etc/chloe/root-outdated` に書く（版が違うときに層をどう扱うかは、まだ決めていない）。
- **ランナーのコンテナのイメージ**：`chloe-dev-runner` をビルドし直したら、動いているワークスペースを一度止める。古いイメージで動いているランナーは、設定の変更を受けたときに VM の DNS の許可リストを作り直せない。
ディスクを広げるのもランナーのコンテナなので、`chloe ws disk` で大きさを変えられるようになる前のイメージのままでは、大きさを変えても次の起動でディスクが広がらない。
複数のホストに置くときは、どのホストでもイメージをビルドし直し、`chloe-node` も更新する。

## 困ったとき

- **ログ**：ゲートウェイと `chloe-node` は標準エラーにログを出す（`journalctl -u chloe-gateway` で読める。`RUST_LOG` で詳しさを変えられる）。ワークスペースごとのプロキシのログ（`egress.log`）、HTTP のログ（`requests.jsonl`）、DNS のログ（`dnsmasq.log`）は `state_root/<ws-番号>/logs/`（例の配置では `/var/lib/chloe/workspaces/<ws-番号>/logs/`）にある。
- **VM が起動しない**：`/dev/kvm` を使えるか、`docker run --privileged` ができるかを確かめる。ランナーのコンテナの出力（`docker logs chloe-ws-<番号>`）に、Firecracker とゲストのシリアルコンソールの出力が出る。
- **VM から外につながらない**：許可リストにない宛先は、名前の解決からできない。`requests.jsonl` と `dnsmasq.log` で、遮断した宛先がわかる。

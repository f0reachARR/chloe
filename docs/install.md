# インストール

chloe を一台のホストで動かす手順と、ランナーを複数のホストに置く手順を書く。
Ubuntu には deb で入れる。
deb を使わずに手で置く手順と、ランナーを Docker のコンテナで動かす手順は、後ろの節にある。

ランナー（ワークスペースの VM とプロキシ）は、systemd の unit で動かす。
root で動くのはワークスペースごとのネットワークの準備だけで、プロキシ、DNS、Firecracker は専用の利用者 `chloe` で動く。
jailer による Firecracker の隔離はまだないので、ホストは chloe 専用にし、信頼できる利用者に限って使う。

## ホストの要件

- **CPU と OS**：x86_64 の Ubuntu 24.04 か 26.04。KVM が使えること（`/dev/kvm` がある）。物理サーバーか、入れ子の仮想化を許した VM。
- **ディスク**：ゲストのイメージに約 3GB。ワークスペースごとに、ワークスペースのディスク（既定 20GiB）と Docker のディスク（既定 40GiB）を疎なファイルで作る。使った分だけ実際に消費する。大きさの既定と上限は設定の `[disks]` で変えられる。
- **ネットワーク**：VM が許可リストの宛先へつなぐときに、外へ出られること。ランナーとホストをつなぐ veth に `10.250.0.0/16` を使う（ホストのネットワークと重なるときは変える。「ランナーとホストのネットワーク」を参照）。

ワークスペースの VM は 4 vCPU、メモリ 4GiB で起動する（今は設定で変えられない）。

## deb で入れる

### deb を作る

deb は、ビルドするマシンで作る。
Docker が要る（ビルドは Ubuntu 24.04 のコンテナの中で行い、できたものは 24.04 と 26.04 で動く）。
リポジトリのルートで実行する。

```sh
# ゲストのカーネル (Linux 6.18 系) と、ルートファイルシステム (Ubuntu 26.04、Docker、mise)。
images/guest/kernel/build.sh
images/guest/rootfs/build.sh

# chloe-common、chloe-gateway、chloe-node、chloe-guest を build/deb/ に作る。
packaging/deb/build.sh --guest
```

`--guest` を付けなければ、`chloe-guest` を作らない。
ゲストのイメージを作るスクリプトは、手順ごとに番号と所要時間を表示する。
カーネルのコンパイルは数分かかり、ソースとビルドの途中のファイルは `build/cache/` に残る（約 2GB）。
二回目からは差分だけをビルドする。

ルートファイルシステムには、`images/guest/rootfs/Dockerfile` で版を決めた mise が入る。
Claude Code や Codex などのツールは入れない（利用者が VM の中で mise で入れる。[使い方](usage.md#ツールを入れる)）。

deb の版は `0.1.0~git<コミットの日付>.<ハッシュ>` になる（コミットしていない変更があれば `+dirty` が付く）。
control の Maintainer は、環境変数 `DEB_MAINTAINER` で変えられる。

| パッケージ | 中身 |
| --- | --- |
| `chloe-common` | `chloe-egress`、Firecracker、ランナーのスクリプトと unit、polkit の規則、`/etc/chloe/runner.env`。利用者 `chloe` と、状態のディレクトリ（`/var/lib/chloe`）を作る |
| `chloe-gateway` | `chloe-gateway`、その unit、`/etc/chloe/gateway.toml`、シークレットの定義の例（`/usr/share/doc/chloe-gateway/examples/secrets/`） |
| `chloe-node` | `chloe-node`、その unit、`/etc/chloe/node.toml`（複数のホストに置くときのランナーのホスト） |
| `chloe-guest` | ゲストのカーネルとルートファイルシステム（`/usr/lib/chloe/guest`） |

### 入れる

作った deb をホストに写し、`apt` で入れる（依存するパッケージも入る）。

```sh
sudo apt install ./chloe-common_*.deb ./chloe-gateway_*.deb ./chloe-guest_*.deb
```

入れただけでは、ゲートウェイは起動しない。
設定の例のままでは、22 番で待ち受けて sshd とぶつかり、ログインを許す利用者も仮の名前だからである。
次の「一台で動かす」の手順で設定してから起動する。

置かれるものは次のとおり。

| 置き場所 | 中身 |
| --- | --- |
| `/usr/bin/` | `chloe-gateway`、`chloe-node` |
| `/usr/libexec/chloe/` | `chloe-egress`、Firecracker、ランナーのスクリプト |
| `/usr/lib/chloe/guest/` | ゲストのカーネルとルートファイルシステム |
| `/usr/lib/systemd/system/` | `chloe-gateway.service`、`chloe-node.service`、ランナーの unit（`chloe-host-net.service`、`chloe-netns@.service`、`chloe-runner@.service`） |
| `/etc/chloe/` | 設定（`gateway.toml`、`node.toml`、`runner.env`）。GitHub App の設定と秘密鍵、管理 API の証明書と鍵もここに置く |
| `/var/lib/chloe/` | ゲートウェイの状態（ホスト鍵、データベース、マスター鍵、CA）と、ワークスペースの状態（`workspaces/`） |
| `/var/lib/chloe-node/` | `chloe-node` の証明書と鍵 |
| `/var/log/chloe/` | 監査ログ |

`/etc/chloe/` の設定は conffile なので、編集したものは更新で上書きされない。

## 一台で動かす

### 設定

`/etc/chloe/gateway.toml` を編集する。
項目の一覧は [設定](configuration.md) にある。
少なくとも次を書き換える。

- `listen`：SSH の待ち受け。例は `0.0.0.0:22` で、ホストの sshd と重なる。sshd を別のポートに移すか、ゲートウェイを別のポートで待ち受ける。
- `allowed_users` と `admins`：ログインを許す GitHub のログイン名と、サーバーの運用者。
- `[[policy.rules]]`：すべてのワークスペースに共通の、外への通信の規則。どの規則にも一致しない通信は遮断する。

状態の置き場所（`host_key`、`database`、`master_key`、`audit_log`）は、unit の `StateDirectory=` と `LogsDirectory=` が作る `/var/lib/chloe` と `/var/log/chloe` の下にしてある。
別の場所に変えるときは、`chloe` の利用者が書けるディレクトリにする。
ランナーの置き場所とアドレスは `/etc/chloe/runner.env` に書く（[設定](configuration.md#runner)）。

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

## ランナーとホストのネットワーク

ゲートウェイは、ワークスペースのランナーを `systemctl start chloe-runner@ws-<番号>` で起動する。
polkit の規則は、`chloe` の利用者にこの unit の起動、停止、読み直しだけを許す。
`chloe-runner@` は、ワークスペースごとのネットワーク namespace を作る `chloe-netns@`（root）と、ホスト側の規則を置く `chloe-host-net`（root）を引き込む。

動いているランナーは `systemctl list-units 'chloe-runner@*'` で、ゲストのシリアルコンソールの出力は `journalctl -u chloe-runner@ws-<番号>` で見られる。
プロキシと DNS のログは `/var/lib/chloe/workspaces/<ws-番号>/logs/` にある。

ランナーの namespace は、veth（ホスト側の名前は `cw<番号>`）でホストとつなぎ、ホストで masquerade して外へ出す。
外へ出るのは namespace の中のプロキシと DNS だけで、VM の通信は namespace の中でプロキシと DNS に向けられる。
veth のアドレスは `/etc/chloe/runner.env` の `CHLOE_NET_POOL`（既定 `10.250.0.0/16`）から、ランナー一つに /30 ずつ割り当てる。
`chloe-host-net` は、nftables の表 `inet chloe-host` に次の規則を置き、`net.ipv4.ip_forward` を 1 にする。

- namespace からホストへの通信を落とす（コントロールプレーンの内部 API、sshd などを守る）。
- namespace からプライベートアドレス（RFC1918 など）への転送を落とす。プロキシは宛先のアドレスを確かめないので、利用者の規則で内部のアドレスに解決される名前を許しても、ここで止まる。ただし DNS（53 番）は通す。上流の DNS サーバーが LAN の中にあることが多いためである。
- namespace 同士の転送を落とす。

DNS の上流は、systemd-resolved を使っていれば `/run/systemd/resolve/resolv.conf` に書かれたサーバーを使う（namespace の中からは `127.0.0.53` に届かないため）。
systemd-resolved を使っていなければ `/etc/resolv.conf` を使う。

次の点に気をつける。

- `nftables.service` を使っているホストでは、その設定（`/etc/nftables.conf`）の `flush ruleset` で `inet chloe-host` も消える。`nftables.service` を読み直したら、`sudo systemctl reload chloe-host-net` で置き直す（restart にすると、動いているランナーも止まる）。
- ほかの仕組み（Docker、ufw など）が転送を落とす規則を置いていると、`cw*` からの転送も落とされることがある。nftables では、同じ hook の規則はどの表でも受け入れられたときだけ通る。

## 状態の置き場所とバックアップ

例の配置での場所を括弧に書く。

| 置き場所 | 中身 | バックアップ |
| --- | --- | --- |
| `database`（`/var/lib/chloe/chloe.db`） | ワークスペース、シークレット（暗号化したもの）、設定、トークンのハッシュ、ホストの一覧 | 要る。`master_key` と組で |
| `master_key`（`/var/lib/chloe/master.key`） | シークレットを暗号化する鍵 | 要る。失うと、保存したシークレットを読めなくなる |
| `host_key`（`/var/lib/chloe/host_key`） | SSH のホスト鍵 | 要る。変わると、利用者の `known_hosts` と合わなくなる |
| `[cluster]` の `pki_dir`（`/var/lib/chloe/pki`） | CA の証明書と鍵 | 要る。失うと、すべてのホストに参加し直させる |
| `audit_log`（`/var/log/chloe/audit.jsonl`） | 監査ログ（JSON Lines） | 運用の方針による |
| `runner.env` の `CHLOE_STATE_ROOT`（`/var/lib/chloe/workspaces`。複数のホストでは各ホストに置く） | ワークスペースごとのディスク、プロキシの CA、HTTP のログ | ワークスペースの中身を残したいなら要る |

`database` は SQLite で、WAL のモードで使う。
動かしたまま写すときは、`sqlite3` の `.backup` を `chloe` の利用者で使う。

```sh
sudo -u chloe sqlite3 /var/lib/chloe/chloe.db ".backup /var/lib/chloe/chloe.db.bak"
```

`chloe ws rm` でワークスペースを消しても、ワークスペースのディスクは残る。
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

コントロールプレーンには `chloe-common` と `chloe-gateway` を入れる（ランナーを置かないので `chloe-guest` は要らない）。
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

各ホストに `chloe-common`、`chloe-node`、`chloe-guest` を入れる。

```sh
sudo apt install ./chloe-common_*.deb ./chloe-node_*.deb ./chloe-guest_*.deb
```

参加して、証明書を受け取る。
`/var/lib` には `chloe` の利用者が書けないので、状態のディレクトリを先に作っておく（unit の `StateDirectory=` が作るのは起動のときである）。

```sh
sudo install -d -o chloe -g chloe -m 700 /var/lib/chloe-node
sudo -u chloe chloe-node join --server https://cp.example.com:8444 --token <トークン> --dir /var/lib/chloe-node/tls
```

`/etc/chloe/node.toml` の `control_plane` を書き換えて起動する。

```sh
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

新しく作った deb を、入れたときと同じく `apt install` で入れる。

- **ゲートウェイと `chloe-node`**：動いていれば、起動し直す。データベースのマイグレーションは起動のときに適用する。動いているワークスペースは止まらない。
- **ランナー（`chloe-common`）**：動いているランナーは止めない。新しいスクリプトと `chloe-egress` は、次に起動するランナーから使う。ホスト側の規則は置き直す。
- **ゲストのイメージ（`chloe-guest`）**：次に起動するワークスペースから新しいイメージを使う。dpkg は新しいファイルを別の名前で書いてから置き換えるので、動いている VM は開いている古いファイルを読み続けられる。動いているワークスペースは、`chloe ws stop` で止めるまで古いイメージのまま動く。ルートを保持するワークスペース（`persistent`）は、残した変更を新しいイメージの上に重ねて起動し、層を作ったときとイメージの版が違うことを VM の中の `/etc/chloe/root-outdated` に書く（版が違うときに層をどう扱うかは、まだ決めていない）。

複数のホストに置くときは、どのホストでも `chloe-node`、`chloe-common`、`chloe-guest` を更新する。

## 消す

```sh
sudo apt remove chloe-gateway chloe-node chloe-common chloe-guest
```

消すと、ゲートウェイと `chloe-node` を止めて無効にし、動いているランナーも止める。
`apt purge` にすると、`/etc/chloe/` の設定も消す。
どちらでも、状態（`/var/lib/chloe`、`/var/lib/chloe-node`、`/var/log/chloe`）と利用者 `chloe` は残す。
マスター鍵を失うと、保存したシークレットを読めなくなるからである。
要らなければ手で消す。

## deb を使わずに置く

Ubuntu 以外のディストリビューションや、開発中のものを試すときは、ビルドしたものを手で置く。
`deploy/` の設定の例と unit は、この節の配置（`/usr/local`）で書いてある。

### ビルドに要るもの

- **Rust**：[rustup](https://rustup.rs/) を入れる。リポジトリの `rust-toolchain.toml` が、stable と、ゲストのバイナリに使う `x86_64-unknown-linux-musl` のターゲットを選ぶ。
- **BoringSSL のビルドに要るもの**：プロキシ（`chloe-egress`）は BoringSSL をソースからビルドする。C と C++ のコンパイラ、cmake、libclang、git が要る。Debian と Ubuntu では次のとおり。

  ```sh
  sudo apt-get install build-essential cmake clang libclang-dev git
  ```

- **Docker**：カーネルとルートファイルシステムは Docker のコンテナの中でビルドする。

### ビルドする

```sh
images/guest/kernel/build.sh
images/guest/rootfs/build.sh
cargo build --release -p chloe-gateway -p chloe-egress -p chloe-node
```

### ホストに要るもの

```sh
sudo apt-get install dnsmasq-base e2fsprogs iproute2 jq nftables polkitd
```

Firecracker は Ubuntu のアーカイブにないので、[リリース](https://github.com/firecracker-microvm/firecracker/releases)のバイナリを置く。
版はランナーのコンテナ（`images/guest/dev/runner.Dockerfile`）と合わせる。

```sh
v=v1.17.0
curl -fsSLO "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz"
curl -fsSLO "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz.sha256.txt"
sha256sum -c "firecracker-$v-x86_64.tgz.sha256.txt"
tar -xzf "firecracker-$v-x86_64.tgz"
sudo install -m 755 "release-$v-x86_64/firecracker-$v-x86_64" /usr/local/bin/firecracker
```

### 置く

リポジトリのルートで実行する。

```sh
sudo useradd --system --no-create-home --home-dir /var/lib/chloe --shell /usr/sbin/nologin --groups kvm chloe
sudo install -m 755 target/release/chloe-gateway target/release/chloe-node /usr/local/bin/
sudo install -D -m 755 target/release/chloe-egress /usr/local/lib/chloe/chloe-egress
sudo install -D -m 755 -t /usr/local/lib/chloe/runner images/guest/runner/*
sudo install -D -m 644 build/guest/kernel/vmlinux /usr/local/lib/chloe/guest/kernel/vmlinux
sudo install -D -m 644 build/guest/rootfs.ext4 /usr/local/lib/chloe/guest/rootfs.ext4
sudo install -m 644 deploy/systemd/*.service /etc/systemd/system/
sudo install -m 644 deploy/polkit/50-chloe.rules /etc/polkit-1/rules.d/
sudo install -m 644 deploy/tmpfiles/chloe.conf /etc/tmpfiles.d/
sudo install -d -m 755 /etc/chloe
sudo install -m 644 deploy/runner.env deploy/gateway.toml /etc/chloe/   # chloe-node のホストでは node.toml
sudo systemd-tmpfiles --create /etc/tmpfiles.d/chloe.conf
sudo systemctl daemon-reload
```

利用者 `chloe` は `kvm` のグループに入れる（Firecracker が `/dev/kvm` を開くため）。
ゲストのイメージは `cp` ではなく `install` で置く。
`install` は古いファイルを消してから新しいファイルを作るので、動いている VM は開いている古いファイルを読み続けられる。
`cp` は同じファイルの中身を上書きするので、動いている VM のルートファイルシステムを壊す。

あとは「一台で動かす」か「複数のホストに置く」の手順で設定して起動する。
更新するときは、同じ `install` で置き直し、`sudo systemctl restart chloe-gateway`（ホストでは `chloe-node`）で起動し直す。

## ランナーを Docker で動かす

設定の `[runner]` から `kind` を消し、`guest_dir`、`egress_bin`、`state_root` を書くと、ランナーを Docker の特権コンテナ（`chloe-dev-runner`）で動かす（[設定](configuration.md#runner)）。
開発機での確認はこの動かし方で行う。
ランナーのコンテナは `--privileged` で動き、ゲートウェイ（ホストでは `chloe-node`）を動かす利用者は `docker` のグループに入る。
`docker` のグループに入った利用者は root と同じことができる。

```sh
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest
sudo usermod -aG docker chloe
```

`chloe-dev-runner` をビルドし直したら、動いているワークスペースを一度止める。
古いイメージで動いているランナーは、設定の変更を受けたときに VM の DNS の許可リストを作り直せない。

Docker から systemd の動かし方に移るときは、すべてのワークスペースを止めてから設定を書き換える（`docker ps --filter name=chloe-` で、ランナーのコンテナが残っていないことを確かめる）。
止めずに切り替えると、新しい動かし方は動いている Docker のコンテナを自分のランナーとして見つけられない。
ワークスペースの状態のディレクトリの形は同じなので、ディスクとプロキシの CA はそのまま引き継げる。
状態のディレクトリに root のファイルが残っていれば、`sudo chown -R chloe:chloe /var/lib/chloe/workspaces` で持ち主を直す。

## 困ったとき

- **ログ**：ゲートウェイと `chloe-node` は標準エラーにログを出す（`journalctl -u chloe-gateway` で読める。`RUST_LOG` で詳しさを変えられる）。ワークスペースごとのプロキシのログ（`egress.log`）、HTTP のログ（`requests.jsonl`）、DNS のログ（`dnsmasq.log`）は `/var/lib/chloe/workspaces/<ws-番号>/logs/` にある。
- **VM が起動しない**：`journalctl -u chloe-runner@ws-<番号> -u chloe-netns@ws-<番号>` に、ネットワークの準備、Firecracker、ゲストのシリアルコンソールの出力が出る。`/dev/kvm` を `kvm` のグループで開けるかを確かめる。ランナーを Docker で動かしているときは、`docker logs chloe-ws-<番号>` に出る。
- **ゲートウェイがランナーを起動できない**：`systemctl start chloe-runner@...` が `Interactive authentication required` で失敗するなら、polkit の規則（`/usr/share/polkit-1/rules.d/50-chloe.rules`）が読まれているか、polkit が動いているか（`systemctl status polkit`）を確かめる。
- **VM から外につながらない**：許可リストにない宛先は、名前の解決からできない。`requests.jsonl` と `dnsmasq.log` で、遮断した宛先がわかる。許可した宛先にもつながらないなら、`sudo nft list table inet chloe-host` でホスト側の規則が置かれているか（`nftables.service` に消されていないか）を確かめる。

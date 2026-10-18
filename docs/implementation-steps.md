# chloe 実装手順

[調査と計画](research-and-plan.md) の設計を、どの順で作るかを定める。
前提が崩れると設計ごと変わる部分（VM の中で Docker が動くか、透過プロキシで差し替えが成り立つか）を先に確かめ、Rust のコードは全体の土台になる「VM を起動してシェルを取る」部分から書き始める。

## 決めた前提

- **ゲスト OS**：Ubuntu 26.04 LTS。
- **ゲストの init**：systemd。dockerd と containerd の管理が素直になる。起動は数百ミリ秒遅くなるが、スナップショットからの復元ではこの差はほぼ効かない。
- **rootfs の作り方**：Dockerfile でビルドしたコンテナのファイルシステムを ext4 イメージに書き出す。パッケージの追加は Dockerfile の変更で済む。
- **VMM**：Firecracker v1.17.0。
- **ゲストカーネル**：Linux 6.18 系。Firecracker の kernel policy では 6.18 のサポートが 2028-06-01 まで続く。Firecracker の検証用設定（`resources/guest_configs/microvm-kernel-ci-x86_64-6.18.config`）を土台に、Docker に必要な設定を重ねる。
- **開発と検証の環境**：KVM が使える手元の Linux マシン。VM を使う統合テストはこのマシンかセルフホストの CI ランナーで回す。

## 段階の一覧

| # | 段階 | 中身 | 終了条件 |
| --- | --- | --- | --- |
| 0 | リポジトリの土台 | Cargo ワークスペース、ツールチェーンの固定、fmt と clippy | 空のクレート群がビルドできる |
| 1 | ゲストイメージ | カーネル、rootfs、Firecracker の手動起動 | VM 内で `docker run hello-world`、`claude`、`codex` が動く |
| 2 | ネットワークの検証 | netns、tap、nftables、mitmproxy の transparent モード、CA の公開、差し替え | 計画書の PoC 確認項目に結果が出る |
| 3 | guest-agent と runner | vsock 越しの PTY と exec、VM に端末をつなぐ CLI | `chloe-runner shell` で VM のシェルに入れる |
| 4 | egress | rama で mitmproxy を置き換える | 段階 2 と同じ確認が rama で通る |
| 5 | gateway | russh、GitHub の公開鍵、PTY 中継、Device Flow | `ssh alice@localhost` で VM に入れる |
| 6 | controller と secrets | DB、ワークスペース管理、シークレットの保管先 | 二人の利用者が別々のシークレットで使える |
| 7 | 運用機能 | エージェントの作業を待つ停止、スナップショット、ポート転送、監査ログ | 作業中のエージェントが止められず、切断と再接続で状態が戻る |
| 8 | 設定のデータベース化と複数の Organization | 許可リスト、規則、トークンの範囲、シークレットを Organization、team、ワークスペースごとにデータベースで持ち、管理者が変えられる | 二つの Organization がそれぞれの設定で使える |
| 9 | 複数台の物理サーバー | コントロールプレーンとランナーのホストを分け、HTTPS (相互 TLS) で通信する | 二台のホストのランナーを一つのゲートウェイから使える |
| 10 | 接続と管理の使い勝手 | ワークスペースのホスト名、Device Flow を必須にしたログインと共通のユーザー名、管理の TUI | `ssh chloe@host` で入れて、`ssh -t chloe@host chloe` で設定を変えられる |
| 11 | Docker を使わないランナー | ランナーを systemd の unit で動かし、root で動かす部分をネットワークの準備に絞る | 段階 6 のテストが、Docker のない root の使えるホストで通る |
| 12 | deb のパッケージ | ゲートウェイ、`chloe-node`、ランナーの部品を Ubuntu の deb にする | Ubuntu 24.04 と 26.04 に `apt install` で入れ、ランナーが動く |
| 13 | ゲストのツールを mise に | ゲストのイメージから Claude Code と Codex を外し、mise を入れる | VM の中で mise でツールを入れて使える |
| 14 | Kubernetes | runner の Pod 化、PVC、NetworkPolicy | 段階 6 のテストが Kubernetes 上で通る |

段階 1 と 2 は互いに依存しないので並行して進められる。
段階 3 から 5 は、それぞれ前の段階の上に積む。

## 確認の進め方

確認は二層に分ける。

- **Rust のテスト**（`cargo test`）：判断の中身を確かめる。controller の停止の予約、作業中のエージェントを待つ停止とその上限、reconcile、問い合わせ用ソケットの答えは、偽のランナーと偽の guest-agent を相手に数秒で確かめる。偽の guest-agent は、本物と同じく `Started` を先に返し、途中で接続を閉じられたらプロセスを止めたことにする。
- **VM を使う確認**（`images/guest/dev/*-test.sh`）：部品がつながっていることを確かめる。本物のゲートウェイ、ランナー、VM を動かし、機能ごとに代表の項目だけを確かめる。判断の細かな場合分けは Rust のテストに任せる。

VM を使う確認は `images/guest/dev/test-all.sh` でまとめて実行する。
どの確認も VM の名前、状態のディレクトリ、ポートが別なので、並べて実行する（段階 7 を終えた時点で 6 つ、171 項目が 57 秒。今の一覧は [開発](development.md) にある）。
各項目には直前の項目からの秒数が付き、ゲートウェイを通す確認は節ごとの所要時間を最後に出すので、遅くなった箇所はそこから探す。

VM を使う確認では、次のように待ち時間を短く保つ。

- 決まった秒数の `sleep` ではなく、条件（ログの行、ポート、ランナーの状態）が成り立つのを `wait_until` で待つ。
- 停止までの時間（`idle_timeout_secs`）と問い合わせの間隔を、確認と確認のあいだにランナーが止まらない程度に短くする。「止まらないこと」を確かめるときだけ、その時間に数秒を足して待つ。
- 外の API を呼ぶ本物の `claude` と `codex` は、確かめたいこと（フックが呼ばれること）が起きたら止める。Codex は 401 のあと再試行を続けるので、終わるのを待たない。

## 段階 0：リポジトリの土台

- Cargo ワークスペースを作り、計画書のクレート構成（`chloe-gateway`、`chloe-controller`、`chloe-runner`、`chloe-egress`、`chloe-secrets`、`chloe-policy`、`chloe-audit`、`chloe-guest-agent`）を空のまま置く。
- `rust-toolchain.toml` で stable を指定し、ワークスペースの `rust-version` を rama の MSRV に合わせて 1.96 にする。
- 共通の依存（`tokio`、`tracing`、`serde`、`thiserror`、`anyhow`）はワークスペースの依存として一か所で版を決める。
- `cargo fmt --check` と `cargo clippy -- -D warnings` が通る状態を保つ。CI は段階 1 の後、VM なしで回せる範囲から入れる。

## 段階 1：ゲストイメージ

`images/guest/` にビルドの手順を置く。
成果物は `build/guest/` に出し、リポジトリには含めない。

### カーネル

1. Firecracker v1.17.0 の `microvm-kernel-ci-x86_64-6.18.config` を取得する。
2. Docker に必要な設定を断片（`images/guest/kernel/docker.config`）として書く。cgroup v2 の各コントローラー、namespace、overlayfs、veth、ブリッジ、netfilter（nf_tables と iptables 互換）、seccomp、BPF などが対象になる。
3. カーネルの `scripts/kconfig/merge_config.sh` で二つを重ね、`make olddefconfig` で確定する。
4. ビルドはコンテナの中で行い、`vmlinux` を取り出す。
5. moby の `contrib/check-config.sh` を確定した設定に掛け、Docker に必要な項目が揃っているかを確かめる。

ゲストではモジュールを読み込まずに済むよう、必要なものはすべて組み込み（`=y`）にする。

### rootfs

1. `images/guest/rootfs/Dockerfile` で `ubuntu:26.04` から作る。入れるものは systemd、Docker の公式パッケージ（docker-ce、containerd、buildx、compose）、git、curl、tmux、Claude Code、Codex。
2. 利用者 `user`（`docker` グループ所属）を作り、ホームディレクトリは後でワークスペースイメージを重ねる場所にする。
3. コンテナを `docker export` し、`mkfs.ext4 -d` でイメージファイルに書き出す。ホストでループマウントせずに済み、root 権限も要らない。
4. ワークスペース用と Docker 用の空の ext4 イメージも同じ方法で作る（疎ファイル）。

### 手動起動

1. Firecracker v1.17.0 の静的バイナリを取得する。
2. カーネル、rootfs（読み取り専用）、ワークスペース、Docker の三つのドライブ、vsock、tap を JSON の設定ファイルに書き、`firecracker --no-api --config-file` で起動する。
3. この段階ではシリアルコンソールでログインして確かめる。

終了条件は、VM の中で次のことができることである。

- `docker run hello-world` と `docker build` が動く（この段階ではネットワークを NAT で素通しにしてよい）。
- `claude --version` と `codex --version` が動く。
- 再起動してもワークスペースと Docker のイメージに書いた内容が残る。

### 段階 1 の結果（2026-10-16）

終了条件はすべて満たした。
手順は次のとおりである。

```sh
images/guest/kernel/build.sh      # build/guest/kernel/vmlinux
images/guest/rootfs/build.sh      # build/guest/rootfs.ext4
images/guest/dev/run.sh           # VM を起動してシリアルコンソールにつなぐ
images/guest/dev/smoke-test.sh    # 終了条件の自動確認 (2 回起動する)
```

当初の手順から変えた点と、分かったことを記録する。

- **開発用の起動はコンテナの中で行う**：開発機では sudo にパスワードが要り、AppArmor が非特権のユーザー namespace を禁止しているので、tap を作れない。そこで Docker の特権コンテナの中に tap0 と nftables の NAT を作り、その中で Firecracker を動かす。構成は Kubernetes の特権 Pod で動かすランナーと同じ形になる。
- **containerd のデータもディスクイメージに置く**：Docker 29 は containerd のイメージストアを使い、そのデータを `/var/lib/containerd` に置く。ここが overlayfs のルート上にあると overlay を重ねられずにコンテナが起動しないので、Docker 用のイメージを `/var/lib/docker` と `/var/lib/containerd` の両方に bind する。
- **Ubuntu 26.04 では `pivot_root` が util-linux-extra に移っている**。
- **シリアルコンソールに長い行を流すと文字が落ちることがある**ので、確認用のスクリプトは読み取り専用のドライブとして渡す。
- **カーネル**：moby の check-config.sh の必須項目はすべて満たす。足りないのは任意の項目（AppArmor、SCTP、暗号化オーバーレイネットワーク、btrfs など）だけである。

## 段階 2：ネットワークの検証

段階 1 の VM を使い、計画書の PoC 確認項目を順に確かめる。
手順と結果は `docs/poc/` に記録する。

1. ネットワーク namespace を作り、その中に tap と Firecracker を置く。
2. nftables で、tap から外への 80 番と 443 番をプロキシに転送し、それ以外を遮断する。
3. mitmproxy を transparent モードで動かし、CA 証明書をゲストの信頼ストアに入れる。`http://ca.chloe.internal/ca.pem` での公開もアドオンで書く。
4. ホストに束縛したプレースホルダーの差し替えをアドオンで書き、Claude Code と Codex を API キーとサブスクリプションの両方で動かす。
5. 中継のみの宛先（Docker Hub など）を `ignore_hosts` で設定し、CA を入れていないコンテナからのイメージ取得を確かめる。
6. 許可外のホストへプレースホルダーを送り、遮断されることを確かめる。

結果は [段階 2 の検証結果](poc/stage2-network.md) に記録した。
本物の認証情報がないので、サブスクリプションのログインは未確認のまま残っている。

## 段階 3：guest-agent と runner

### 作ったもの

- **`chloe-agent-proto`**：ホストと guest-agent のあいだのプロトコル。一つの接続で一つのプロセスを扱い、長さつきのフレームに postcard で符号化したメッセージを載せる。
- **`chloe-guest-agent`**：ゲストで systemd が常駐させる。vsock の 7000 番で待ち受け、ホスト（CID 2）からの接続だけを受ける。要求ごとに、端末つき（pty-process）かパイプでプロセスを起動する。root 以外の利用者には `runuser` で切り替え、既定の利用者は `user`、既定の作業ディレクトリは `/workspace` にした。musl で静的にリンクし、rootfs のビルドで組み込む。
- **`chloe-runner`**：`shell` と `exec` を持つ CLI。Firecracker の vsock の Unix ソケットに `CONNECT 7000` を書いて guest-agent につなぐ。端末つきのときは raw モードにし、端末の大きさの変化（SIGWINCH）を送る。

VM の起動そのものは、まだ `images/guest/dev/run.sh` のコンテナが行う。
Firecracker を `chloe-runner` から直接起動する部分は、ランナーの管理を作る段階 6 で書く。
そのため、当初の終了条件の `chloe-runner run` は `run.sh` と `chloe-runner shell` の組み合わせに置き換えた。

```sh
CHLOE_DEV_DETACH=1 images/guest/dev/run.sh
target/debug/chloe-runner shell --vsock build/dev/vsock.sock
```

### 結果（2026-10-16）

- ログインシェル、端末の割り当てと大きさ、標準入力、標準エラー、終了コード、利用者の切り替え、作業ディレクトリ、環境変数を `smoke-test.sh` で確かめた。
- 確認スクリプトはシリアルコンソールではなく `chloe-runner exec` で実行するようにした。段階 1 の確認は 54 秒から 20 秒になった。
- シリアルコンソールの root の自動ログインは外した。

guest-agent の認証は、vsock の接続元が CID 2 であることだけに頼っている。
ゲストのカーネルは vsock のループバック（`CONFIG_VSOCKETS_LOOPBACK`）を持たないので、ゲスト内のプロセスが guest-agent に接続することはできない。
ホスト側の Unix ソケットに接続できる者は誰でもゲストの root になれるので、ソケットはランナーの外に出さない（開発用の `run.sh` だけは、ホストの利用者がつなげるよう 0666 にしている）。

## 段階 4：egress

### TLS の実装の選定

rama 0.4.0 の BoringSSL 版の SNI MITM の例は、この開発機で問題なくビルドできた（cmake と clang があり、初回のビルドは 1 分半ほど）。
BoringSSL の経路には、渡した CA で SNI ごとの証明書をその場で発行する仕組み（`ServerCertIssuerKind::ProvidedCa`）があるので、BoringSSL を使う。
rama にはもう一つ、先に上流へ接続してその証明書をまねる方式（`TlsMitmRelayService`）もある。
ただしこの方式は判定の前に上流へ接続してしまい、「許可していない宛先には接続しない」方針と合わないので使わない。

### 作ったもの

- **`chloe-policy`**：段階 2 のアドオンの方針を、HTTP ライブラリに依存しない判定として書き直した。中継のみの宛先、SNI と Host の一致、許可リスト、宛先に束縛したプレースホルダーの差し替え（置換と Basic 認証）を扱う。遮断するときは、一部だけ差し替えた状態を残さない。`net-test.sh` の各項目に対応する単体テストを持つ。
- **`chloe-egress`**：rama による透過プロキシ。nftables が 80 番を 8080 番に、443 番を 8443 番に転送する。
  - 443 番は `SniRouter` で SNI を読み、中継のみの宛先なら復号せずに SNI のホストへ中継する。それ以外はワークスペースの CA で証明書を発行して復号する。
  - 上流へは SNI（平文なら Host）のホストへ接続し、上流の証明書を検証する。
  - WebSocket は Upgrade のヘッダーを残して上流へ送り、確立後のメッセージをそのまま中継する。HTTP/2 の上では Upgrade を送れないので、Upgrade を含む要求は上流との ALPN を HTTP/1.1 に固定する。
  - CA は初回の起動で生成し、ディレクトリに置く（開発用。段階 6 でデータベースに移す）。
  - ログは段階 2 と同じ JSON Lines の形式で、本文を調べたかどうか（`body_scanned`）を加えた。
- **DNS の絞り込み**：ランナーの dnsmasq は、方針の allow と passthrough にある名前だけを上流に問い合わせ、それ以外には NXDOMAIN を返す。

開発用のランナーからは mitmproxy を取り除いた。
`chloe-egress` はホストでビルドしたバイナリをコンテナに渡している（ホストの glibc 2.39 はコンテナの 2.43 より古いので動く）。
本番のランナーのイメージでは、コンテナの中でビルドするように変える。

### 結果（2026-10-16）

`net-test.sh` の 22 項目（段階 2 の 20 項目と DNS の 2 項目）がすべて通った。
Claude Code と Codex は段階 2 と同じく、API に届いてプレースホルダーから差し替えた偽のキーで認証エラーになった。
Codex は WebSocket の握手（401）のあと HTTPS に切り替えており、どちらの経路でも差し替えが効いた。

### 分かっている制限

- **本文を調べる範囲**：Content-Length が 1 MiB 以下の本文だけを調べる。大きさの分からない本文（chunked）と 1 MiB を超える本文は、調べずに上流へ送る。gzip などで圧縮された本文も、圧縮されたまま調べる（Claude Code は API への本文を gzip で送る）。WebSocket のメッセージも調べない。どの場合も、プレースホルダーが登録していない宛先へ本文の中で送られることは防げない。差し替えはヘッダーでしか起きないので、本物の秘密情報が登録していない宛先へ漏れることはない。
- **DNS のワイルドカード**：dnsmasq の `--server=/example.com/` は example.com そのものにも一致するので、`*.example.com` の許可は DNS では少し広くなる。プロキシの判定は `*.example.com` を厳密に扱う。

## 段階 5：gateway

### 作ったもの

- **`chloe-gateway`**：russh による SSH サーバー。認証は公開鍵だけを受ける。
  - SSH ユーザー名は `login` か `login+workspace` で、`login` を GitHub のログイン名として扱う。
  - 提示された公開鍵を `{github_url}/{login}.keys`（既定は `https://github.com`）と照合し、5 分間キャッシュする。ワークスペースのメンバーでなければ GitHub に問い合わせる前に拒否する。
  - セッションチャネルごとに guest-agent へ接続し、シェル、コマンド（`bash -c`）、端末、標準入出力、終了コード、端末の大きさ、シグナルを中継する。環境変数は OpenSSH の既定と同じく `LANG` と `LC_*` だけを受ける。
  - ワークスペースと VM の vsock の対応、メンバーは、段階 6 でワークスペースの管理ができるまで設定ファイル（TOML）に書く。
- Firecracker の vsock への接続処理は `chloe-agent-proto` に移し、`chloe-runner` と共有した。

### 結果（2026-10-16）

`images/guest/dev/gateway-test.sh` で、OpenSSH のクライアントからゲートウェイと guest-agent を通って VM に届くことを確かめた（11 項目）。
GitHub の代わりに、テスト用の公開鍵を返す HTTP サーバーをローカルに立てている。

### 残したもの

- **Device Flow**：GitHub App ができたら、`keyboard-interactive` で Device Flow を行い、得たトークンを個人ワークスペースの GitHub の認証情報として保存する。
- **Organization と team の所属の確認**：GitHub App のインストールトークンで確かめる。ワークスペースの持ち主を team や Organization にする段階 6 と合わせて入れる。
- **ワークスペースの選択メニュー**：ワークスペースを指定しないときに出す。ワークスペースの管理ができてから入れる。
- **ポート転送**：段階 7 で入れる。

## 段階 6：controller と secrets

### 終了条件

`images/guest/dev/mvp-test.sh` で次を確かめる。

- 二人の利用者（alice と bob）が、それぞれの個人ワークスペースに同時に SSH で入れる。
- それぞれのワークスペースのプロキシが、それぞれの利用者の秘密情報だけを差し替える。bob の VM から alice の本物の秘密情報を得る手段はない（alice のプレースホルダーを bob の VM から送っても、差し替えられずにそのまま届く）。
- 最後の接続が切れてしばらくするとランナーが止まり、次の接続で起動し直し、ワークスペースとホームディレクトリの内容が残っている。
- `ssh alice@... chloe ws list` でワークスペースの一覧が出る。

### 進め方

段階 6 は大きいので、次の順に分けて、それぞれをテストつきでコミットする。

1. **6a `chloe-secrets`**：`SecretStore` と、SQLite に置くシークレット（シークレットごとのデータ鍵を AES-256-GCM で暗号化し、データ鍵をマスター鍵で暗号化する。追加認証データに持ち主と名前を入れる）。1Password Connect のクライアントは、本物のサーバーがなくても試せるよう、テスト用のサーバーに対して確かめる。
2. **6b `chloe-controller`**：ゲートウェイと同じプロセスで使うライブラリにする。データベース（利用者、ワークスペース、メンバー、シークレット、ランナー）、ワークスペースの作成と一覧、ワークスペースごとの方針の生成、ランナーの起動と停止を抽象した `RunnerBackend`。開発用には `chloe-dev-runner` のコンテナをワークスペースごとに起動する実装を作る。テストには偽の実装を使い、KVM なしで回す。
3. **6d ゲスト側**：
   - プレースホルダーと CA を、ランナーごとの設定用ドライブで VM に渡し、guest-agent が実行するすべてのプロセスの環境に入れる。ログインシェルは `runuser --login` をやめ、環境を保つ `runuser -u user -- bash -l` にする。
   - ルートの「使い捨て」と「保持」を、カーネルの引数（`chloe.root=persistent`）で切り替える。
4. **6c ゲートウェイの統合**：ワークスペースを設定ファイルではなくデータベースから引き、接続時にランナーを起動し、最後の接続が切れてから一定時間で止める。`chloe` で始まるコマンドはゲートウェイが受け取り、VM には渡さない（`chloe ws create|list|rm`、`chloe secret set`）。シークレットの値は標準入力から読み、VM を通らない。
5. **6e** `mvp-test.sh` を通す。
6. **6f GitHub App**：インストールトークンの発行、Organization と team の所属の確認、`keyboard-interactive` での Device Flow、ユーザートークンの保存と差し替え。利用者の GitHub App の情報が要るので最後にする。

### 決めたこと

- **プレースホルダーはワークスペースごとに固定する**：計画書では「ランナーの起動ごとに乱数」としていたが、ワークスペースの作成時に一度だけ作ってデータベースに置く。`codex login --with-api-key` は API キー（ここではプレースホルダー）を永続化したホームディレクトリに書くので、起動ごとに変えると古い値が残って認証エラーになる。プレースホルダーは宛先に束縛されているので、漏れても害がない。作り直したいときのために `chloe secret rotate` を用意する。
- **シークレットの持ち主**：シークレットは持ち主（利用者、team、Organization）と名前で識別する。ワークスペースの差し替え規則は、そのワークスペースの持ち主のシークレットから作る。個人のシークレットは個人ワークスペースにだけ入り、共有ワークスペースには入らない。
- **秘密情報のランナーへの渡し方**：開発用の実装では、ランナーのコンテナの環境変数で渡す（`docker inspect` で見える）。ランナー専用の API で渡す方式は強化の段階に回す。

### 結果（2026-10-16、6a〜6e）

`images/guest/dev/mvp-test.sh` の 30 項目がすべて通った。
本物の API キーは使わず、偽の値をシークレットに登録して確かめた。

- **6a `chloe-secrets`**：`SecretDb`（エンベロープ暗号化した SQLite のシークレット）、`MasterKey`、1Password Connect のクライアント、`op://`、`file:`、`env:` の参照の解決。マイグレーションはリポジトリの `migrations/` に一つにまとめ、`chloe-secrets` が持つ。
- **6b `chloe-controller`**：シークレットの種類の一覧（`anthropic_api_key`、`claude_oauth_token`、`openai_api_key`、`github_token`）、ワークスペースの持ち主と名前、データベース、方針とゲストの環境変数の組み立て、接続数を数えてランナーを起動と停止する `Controller`、開発用の `DockerBackend`。差し替えの規則とプレースホルダーの環境変数は、持ち主がシークレットを登録した種類だけに作る（登録していないプレースホルダーを VM に入れると、ツールがサブスクリプションのログインを試さなくなる）。
- **6d ゲスト側**：設定用ドライブ（`/dev/vdd`）で環境変数と CA 証明書を渡し、guest-agent と `/etc/profile.d/chloe.sh` が読む。ログインシェルは環境を保つ `runuser -u user -- bash --login` にした。ルートの保持は `chloe.root=persistent` で切り替え、`root-test.sh` で確かめた。
- **6c ゲートウェイ**：ワークスペースをデータベースから引き、接続時にランナーを起動する。`chloe ws ...` と `chloe secret ...` はゲートウェイが処理する。

途中で分かったこと：

- **ルートを保持すると、停止時にデータが書き出されないことがあった**：ルートの overlay がワークスペースのディスクを使い続けるので、systemd がマウントを外してもディスクが閉じられず、最後の `sync` より後の書き込みが失われていた。停止の直前に `sync` するフック（`/usr/lib/systemd/system-shutdown/chloe-sync`）を入れて直した。
- **一つのプレースホルダーを複数の宛先の規則で差し替える**：GitHub のトークンは `github.com`（Basic 認証）と `api.github.com`（Bearer）で別の規則になるので、同じプレースホルダーを持つ規則のどれかが宛先を許していれば束縛された宛先とみなすよう `chloe-policy` を直した。
- **ランナーのコンテナが root のファイルを残す**：状態のディレクトリにログや CA を root で書くと、ホストの利用者が消せなくなる。`chloe-egress` をディレクトリの持ち主の権限で動かすようにした。

### 6f の結果（2026-10-16）

- **`chloe-github`**：アプリの JWT、インストールトークン（期限の 5 分前まで使い回す）、Organization と team の所属の確認（5 分間覚えておく）、Device Flow、ユーザートークンの更新。
- **ゲートウェイ**：`github_app` を設定すると、アプリの Organization のメンバーがログインでき、Organization と team のワークスペースには GitHub 上の所属で入れる。公開鍵を登録していない利用者は keyboard-interactive で Device Flow を行う。`chloe github login|status|logout` を足した。
- **GitHub のトークンの渡し方**：個人のワークスペースには Device Flow で得たユーザートークンを（失効まで 1 時間を切っていれば更新して）、team と Organization のワークスペースにはインストールトークンを、ランナーの起動時に渡す。
- **ゲストの git**：GitHub に HTTPS でつなぎ、認証ヘルパーがプレースホルダーを返す。
- `images/guest/dev/github-test.sh` で、GitHub のふりをするサーバーに対して 20 項目を確かめた。
- 本物の GitHub（ForteFibre にインストールしたアプリ）でも次を確かめた。
  - インストールトークンの発行と Organization の所属の確認（メンバーは true、メンバーでない利用者は false）。
  - `allowed_users` を空にして、GitHub に登録した公開鍵と Organization の所属だけでログインできる。
  - `chloe github login` の Device Flow（ブラウザで承認）。ゲートウェイのログにトークンは出ない。
  - 個人のワークスペースの VM にはプレースホルダーだけがあり、`api.github.com/user` がプレースホルダー経由で利用者本人として答える。非公開リポジトリへの `git ls-remote` が、401 のあと認証ヘルパーとプロキシの差し替えで通る。`git@github.com:` のリモートも HTTPS に読み替えて通る。
  - Organization のワークスペースにはインストールトークンが渡る（`/installation/repositories` が見え、`/user` は使えない）。

6f で残したもの：

- **ランナーが動いているあいだのトークンの更新**：トークンはランナーの起動時にだけ渡す。ユーザートークンは 8 時間、インストールトークンは 1 時間で失効するので、それより長く動き続けるランナーでは GitHub の操作が認証エラーになる。ランナーのプロキシが新しいトークンを受け取れるようにする（ランナー専用の設定 API と合わせて入れる）。
- **インストールトークンの絞り込み**：Organization の管理者がワークスペースごとにリポジトリと権限を決める仕組み。今はインストールに与えたリポジトリと権限をそのまま使う。
- **keyboard-interactive の自動テスト**：OpenSSH のクライアントで入力を流し込む必要があり、まだ手で確かめていない。
- **team と Organization のシークレット**：管理コマンドは今、接続した利用者の個人のシークレットだけを扱う。
- **`chloe secret rotate`**：プレースホルダーの作り直し。データベースの操作はあり、コマンドがない。
- **ワークスペースの選択メニュー**：今は指定がなければ個人の `default` に入る。
- **`chloe ws rm` のディスク**：ワークスペースを消しても、ランナーの状態のディレクトリ（ディスクのイメージ）は残る。

### パスの規則（2026-10-16）

`chloe-policy` に、宛先、メソッド、パスで要求を許すか拒否するかを決める規則を足した。
ゲートウェイの設定の `[[policy.rules]]` に書き、すべてのワークスペースに効く（書き方は `images/guest/dev/gateway.example.toml`）。

- 上から順に評価し、最初に一致した規則の `allow` か `deny` で決める。どれにも一致しなければ許可リストだけで判定する。
- パスのグロブは、`*` が一つの階層の中、`**` が 0 個以上の階層に一致する。
- 照合の前に、パーセントエンコードを戻し（`%2F` を除く）、`//`、`.`、`..` を解決する。根より上を指すパスは `bad-path` で拒否する。大文字と小文字は区別しない。
- 拒否した要求は 403 を返し、ログに規則の名前を残す。
- 中継のみの宛先は復号しないので、規則は効かない。

`net-test.sh` で、許可、拒否、メソッドでの拒否、`..` による回り込み、規則に当たらない要求を確かめた。

残したもの：ワークスペースごとの規則（Organization の管理者が決める）、git の push の本文を読んだブランチ単位の拒否、GraphQL の中身の判定。

### この段階に入れないもの

スナップショットと実行中のプロセスへの再接続、ポート転送（段階 7）、Kubernetes の実装（段階 8）、ランナー専用の設定 API、CA の秘密鍵のデータベースへの移動、宣言からのワークスペース専用イメージ。

## 段階 6g：動いているランナーへの更新

ランナーの起動時にだけシークレットを渡していると、ユーザートークン（8 時間）やインストールトークン（1 時間）より長く動くランナーで GitHub の操作が失敗する。
`chloe secret set` の結果も、ランナーを止めるまで効かない。

- `chloe-egress` に、ランナーの状態のディレクトリに置く制御用の Unix ソケットを持たせ、方針と本物の値を動いたまま入れ替えられるようにする。
- controller は、動いているランナーの方針と値を作り直して送る操作を持つ。ゲートウェイは、一定の間隔で GitHub のトークンを更新して送り、`chloe secret set` と `rm` のあとにも送る。
- 段階 8 で設定をデータベースで変えられるようにしたとき、同じ口で方針を入れ替える。

結果（2026-10-16）：`chloe-egress` の `--control` ソケット、`RunnerBackend::update`、`Controller::refresh`、ゲートウェイの定期的な更新（`refresh_interval_secs`、既定 10 分）と `chloe secret set|rm`、`chloe github login` のあとの更新を入れた。`mvp-test.sh`（35 項目）で、動いているランナーのシークレットを変えると、そのランナーのプロキシだけが新しい方針に入れ替わることを確かめた。
VM の環境変数（プレースホルダー）は起動時に決まるので、起動時になかった種類のシークレットを使うには、ワークスペースを一度止める必要がある。

## 段階 6h：本物の値の問い合わせ

6g までは、本物の値をランナーのプロキシのメモリに持たせ続けていた（起動時は環境変数、更新は送り込み）。
これを、プロキシは差し替えの規則とプレースホルダーだけを持ち、差し替えのたびにコントロールプレーンへ本物の値を問い合わせて、使い終わったらすぐ捨てる形に変える。

- **本物の値を持ち続けるのはコントロールプレーンだけになる**：ランナーのプロセスのメモリ、コンテナの環境変数（`docker inspect`）、更新用のソケットから本物の値がなくなる。
- **取り消しと更新がすぐ効く**：コントロールプレーンで値を消したり更新したりすると、次の要求から効く。GitHub のトークンの更新も、問い合わせのときにコントロールプレーンが行う。
- **監査**：いつ、どのワークスペースが、どのシークレットを使ったかをコントロールプレーンが記録する。
- **引き換え**：コントロールプレーンが止まると、本物の値が要る要求は失敗する（503）。問い合わせの分だけ要求が遅くなる。キャッシュは持たない。

作り方：

- controller が、ランナーごとに専用の問い合わせ用ソケット（ランナーの状態のディレクトリの `broker.sock`、権限 600）で待ち受ける。ソケットがランナーごとなので、どのランナーからの問い合わせかはソケットで決まり、そのワークスペースに許したシークレットだけを返す。
- 値の出どころは controller の `SecretSource` にまとめる。データベースのシークレットはそのまま、GitHub のトークンはゲートウェイが（期限が近ければ更新して）答える。
- `chloe-policy` の判定を二段に分け、判定ではどのヘッダーをどの規則で差し替えるかだけを決め、値を受け取ってから書き換える。
- 6g の制御用ソケットは、方針（許可リストや規則）の入れ替えにだけ使う。
- 問い合わせのやり取りは、段階 9 で HTTPS（相互 TLS）に置き換えやすいよう、一行の JSON の要求と応答にする。

結果（2026-10-17）：

- `chloe-policy`：`evaluate` が差し替えの予定（`Substitution`）を返し、`apply` で本物の値を当てる。値が一つでも得られなければ何も書き換えない。
- `chloe-egress`：`--broker` のソケットに差し替えのたびに尋ね、書き換えたら値を捨てる（受け取った行も上書きして消す）。値を得られなければ 503。`--broker` がなければ開発用に環境変数から読む。制御用ソケットは方針だけを受け、本物の値を含む更新は拒否する。
- `chloe-controller`：ランナーの起動の前に `broker.sock` で待ち受け、止めたら片づける。答えるのはそのワークスペースで使える種類だけで、答えるたびに種類を記録する。`DynamicSecrets` で GitHub のトークンを足す。
- ゲートウェイ：GitHub のトークンを問い合わせのときに答える（ユーザートークンは期限が 5 分を切っていれば更新し、同時の更新は錠で一つずつにする）。6g の定期的な送り込みは外した。
- `mvp-test.sh`（38 項目）で、ランナーのコンテナの環境変数に本物の値がないこと、問い合わせが記録されること、値を変えると次の要求から効くこと、ゲートウェイを止めると本物の値が要る要求だけが 503 になることを確かめた。`github-test.sh`（22 項目）で、Organization のワークスペースの GitHub のトークンも問い合わせで答えることを確かめた。

残したもの：問い合わせの監査を、SSH の接続の監査とまとめて残す仕組み（段階 7 の監査ログ）。ゲートウェイの再起動のあいだ、動いているランナーの問い合わせ用ソケットは待ち受けが止まるので、再起動後に動いているランナーへ待ち受けを張り直す必要がある（今は再起動で controller がランナーを見失う。段階 7 で扱う）。

## 段階 7：運用機能

### エージェントの作業を待つ停止

最後の接続が切れても、VM の中で Claude Code や Codex が作業を続けているあいだは、ランナーを止めない。

- Claude Code と Codex のフック（`UserPromptSubmit`、`PostToolUse`、`Stop`、`SessionEnd` など）で、作業の始まり、途中、終わりをゲストの `/run/chloe/activity/` に記録する。フックはゲストのイメージの管理用の設定（Claude Code の managed settings、Codex の `hooks.json`）に入れ、利用者の設定に依らずに効かせる。
- guest-agent に、作業中の印があるかを答える要求を足す。印は一定時間（たとえば 30 分）更新がなければ古いものとみなし、エージェントが落ちて印が残っても止められるようにする。
- controller は、停止の前に guest-agent に問い合わせ、作業中なら停止を延ばして、一定の間隔で問い合わせ直す。延ばせる長さには上限を設ける。

### ゲートウェイの再起動からの復旧

ゲートウェイ（と同じプロセスの controller）が落ちたり更新で再起動したりしても、動いているランナーと VM の中の作業を失わないようにする。

- controller は、起動したランナーをデータベースの `runners` の表に記録する。
- ゲートウェイは起動時と一定の間隔で reconcile する。記録と実際に動いているランナー（`RunnerBackend::list`）を突き合わせ、記録のあるランナーは引き取って問い合わせ用ソケットで待ち受け直し、接続がなければ停止の予定を入れる。記録のないランナーは止め、動いていないランナーの記録は消す。

### 結果（2026-10-17）

`images/guest/dev/lifecycle-test.sh` の 17 項目がすべて通った。

- **フック**：Claude Code では始まりと終わりの両方のフックが呼ばれる。Codex はターンが失敗したとき（テストでは 401）に終わりのフックを呼ばないので、印が残る。
- **残った印**：`chloe-activity` が印にエージェント本体（祖先のプロセスのうち名前が `claude` や `codex` のもの）のプロセス ID を書き、guest-agent はそのプロセスが終わっていれば印を消す。プロセス ID がない印は、30 分の期限だけで判断する。
- **作業待ち**：接続を切っても、作業中の印があるあいだはランナーが止まらず、印のプロセスが終わると止まる。
- **再起動**：ゲートウェイを止めてもランナーは動き続け、起動し直したゲートウェイが引き取る。引き取ったあとも本物の値の問い合わせに答え、接続がなければ止める。持ち主のないランナー（記録にないコンテナ）は reconcile で止める。

設定は `idle_timeout_secs`、`activity_poll_secs`、`max_busy_wait_secs`、`reconcile_interval_secs`。

### ポート転送

`ssh -L 8080:127.0.0.1:3000 alice@chloe` で、VM の中で動かしている開発用のサーバーに手元からつなげるようにする。

- ゲートウェイは `direct-tcpip` のチャネルを受け、guest-agent に VM の中から宛先へ TCP で接続させる。guest-agent はつながったら `Connected` を返し、以後はメッセージではなく生のバイト列を中継する。つながらなければ、ゲートウェイはチャネルを断る。
- 宛先は制限しない。guest-agent の接続は VM の中から出るので、VM の中のシェルからつなげる宛先にしか届かない（外への通信はランナーのプロキシの方針に従う）。
- ポート転送をした SSH の接続があるあいだは、転送中の TCP の接続がなくてもランナーを止めない。`ssh -N -L` で開いたまま、ときどきブラウザで開く使い方では、接続のあいだにランナーが止まると VM の中のサーバーも止まってしまう。
- リモートの転送（`ssh -R`）と、Unix ソケットの転送は扱わない。

### 監査ログ

ゲートウェイの設定の `audit_log` に、監査イベントを JSON Lines で追記する（権限 600）。
`tracing` のイベントのうち、target を `audit` にしたものだけを書く。

- **ログイン**：成功（`authenticated`、`method` は `publickey` か `device_flow`）と拒否（`rejected`）。
- **セッション**：開始（`session started`、`kind` は `shell` か `exec`、端末の有無）と終了（`session ended`、終了コード）。利用者のコマンドの引数はトークンを含むことがあるので残さない。
- **ポート転送**：宛先と、つながらなかったときの理由。
- **管理コマンド**：`chloe ...` の引数。シークレットの値は標準入力で受けるので引数に入らない。
- **ランナー**：起動、停止、引き取り、持ち主のないランナーの停止、controller の知らないうちに止まったランナー。
- **シークレット**：問い合わせに答えた（`secret served`）か答えなかったかと、ワークスペースとシークレットの種類。値は書かない。GitHub のユーザートークンの更新。

遮断した通信と差し替えた通信は、各ランナーの `requests.jsonl`（`chloe-egress` の HTTP ログ）に残る。
監査の記録は、この二つを合わせたものになる。

### 結果（2026-10-17、ポート転送と監査ログ）

`lifecycle-test.sh` に、ポート転送と監査ログの項目を足した。

- **ポート転送**：VM の中の HTTP サーバーに転送でつながり、閉じたポートへの転送は断られる。転送している SSH の接続があるあいだはランナーが止まらず、切ると止まる。
- **監査ログ**：ログイン、セッションの開始と終了、管理コマンド、ランナーの起動、引き取り、持ち主のないランナーの停止、シークレットの問い合わせ、ポート転送とその失敗が残る。本物の値と、利用者のコマンドの引数は残らない。

### スナップショット

止めるときにスナップショットに取り、次の接続で戻すことを PoC で確かめた（[結果](poc/stage7-snapshot.md)）。
戻すのは 0.8 秒、コールドブートは 2.9 秒で、差は小さい。
一方で、ワークスペースごとにメモリの大きさのファイルが要り、戻したあとのゲストの時刻を直す仕組みも要る。
当面はコールドブートを続け、VM の中で動かしたままのプロセスを止めずに残したいという要望が出てから採る。

## 段階 8：設定のデータベース化と複数の Organization

複数の Organization で使うことを見込み、ゲートウェイの設定ファイルに書いている設定を、データベースから変えられるようにする。

- **設定の層**：全体（ゲートウェイの設定）、Organization、team、ワークスペースの順に重ね、下の層が上の層に足したり上書きしたりする。対象は許可リスト、中継のみの宛先、パスの規則、ルートの扱いの既定、インストールトークンのリポジトリと権限。
- **管理者**：Organization の設定は、GitHub でその Organization の管理者（`admin` の役割）である利用者が変えられる。team の設定は、team の maintainer が変えられるようにするかを決める。
- **管理コマンド**：`chloe org ...`、`chloe policy ...` のように、SSH の管理コマンドで設定を読み書きする。変えた設定は、段階 6g の口で動いているランナーにも送る。
- **複数の Organization**：一つの GitHub App を複数の Organization にインストールして使う。今の `organization` の一つだけを前提にしている部分（所属の確認、インストールトークン）を、インストールごとに扱うように広げる。
- **シークレット**：Organization と team のシークレットを、その管理者が登録できるようにする。

### 決めたこと（2026-10-17）

- **使える Organization**：GitHub App をインストールしたすべての Organization。そのどれかのメンバーであればログインできる。だれでもアプリをインストールできると、だれでも chloe を使えてしまうので、GitHub App は非公開（持ち主のアカウントだけがインストールできる）にしておく。
- **設定を変えられる人**：Organization と、その team の設定は、GitHub でその Organization の管理者（`admin` の役割）である利用者だけが変えられる。
- **シークレットを登録できる人**：Organization のシークレットは Organization の管理者が、team のシークレットは Organization の管理者と team の maintainer が登録、変更、削除できる。メンバーはワークスペースの中で使えるが、値は読めない（プレースホルダーだけが見える）。
- **インストールトークンの範囲**：既定では、アプリのインストールのすべてのリポジトリと権限のトークンを渡す。Organization の管理者は、Organization や team ごとに、リポジトリの一覧と権限（たとえば `contents: read`）に絞れる。
- **個人のワークスペース**：全体の設定だけを使う。所属する Organization の設定は重ねない（どの Organization の設定を使うかが決まらないため）。

### 進め方

1. **8a 複数の Organization**：`chloe-github` を、インストールごとにインストールトークンを持ち、Organization を指定して所属を確かめる形にする。ゲートウェイのログインとワークスペースの所属の確認を、インストールしたすべての Organization に広げる。
2. **8b 役割**：Organization の管理者と team の maintainer を GitHub に問い合わせる（`/orgs/{org}/memberships/{user}` の `role`、`/orgs/{org}/teams/{team}/memberships/{user}` の `role`）。
3. **8c 設定の層**：データベースに Organization と team の設定（許可リスト、中継のみの宛先、パスの規則、ルートの扱いの既定）を置き、全体、Organization、team の順に重ねて方針を作る。`chloe policy show|set|rm <持ち主>` で読み書きし、変えたら動いているランナーに送る。
4. **8d Organization と team のシークレット**：`chloe secret set|rm|list --owner <持ち主>` を、8b の役割で許す。
5. **8e インストールトークンの範囲**：設定の層に、リポジトリと権限を足し、インストールトークンを発行するときに指定する。範囲ごとにトークンをキャッシュする。

各段階の確認は、GitHub のふりをするサーバーに Organization を二つ置いて、`github-test.sh` に足す。

### 結果（2026-10-17）

`github-test.sh` の 56 項目を含め、VM を使う確認の 210 項目がすべて通った。
本物の GitHub でも、インストールの一覧、インストールトークンの発行、Organization での役割（`admin`）を確かめた。

- **8a 複数の Organization**：`/app/installations` から Organization ごとのインストールを調べ（5 分覚えておく）、Organization を指定してインストールトークンを発行し、所属を確かめる。App の設定の `organization` と `organizations` は任意で、書けば使う Organization を限る。アプリを Any account で公開するときは、ここで限る（[GitHub App の準備](github-app.md)）。
- **8b 役割**：所属は `/orgs/{org}/memberships/{user}` と team の memberships で確かめ、招待を承諾していない利用者はメンバーとみなさない。Organization の `admin` と team の `maintainer` を返す。
- **8c 設定の層**：データベースの `policies` に Organization と team の設定を置き、全体、Organization、team の順に重ねる。許可リストは足し合わせ、パスの規則は上の層を先に並べる（最初に一致した規則で決まるので、下の層は上の層の規則を覆せない）。中継のみの宛先は、Organization と team の設定には書けない。復号しないので規則が効かず、書けると上の層の規則を素通りする宛先を作れてしまうためである。ルートの扱いの既定とインストールトークンの範囲は、もっとも下の層の値を使う。`chloe policy show|set|rm OWNER`（TOML）で読み書きし、変えたらその持ち主（Organization なら team も）の動いているランナーに送る。設定を変えられるのは、Organization の管理者とゲートウェイの設定の `admins`（サーバーの運用者）である。
- **8d シークレット**：`chloe secret list|set|rm --owner OWNER`。team のワークスペースは、team になければ Organization のシークレットを使う（同じ種類なら team のものが優先する）。
- **8e インストールトークンの範囲**：設定の `[github]` の `repositories` と `permissions` で発行し、範囲ごとにキャッシュする。

あわせて、動いているワークスペースへの更新を次のように直した。

- **プレースホルダーの入れ直し**：シークレットの種類が変わったら、controller が guest-agent に環境変数を送り（`SetEnv`）、`/run/chloe/env` を書き換える。ランナーを起動し直さなくても、このあとに開くシェルに新しいプレースホルダーが入る。すでに開いているシェルの環境は変わらない。
- **VM の DNS の許可リスト**：dnsmasq が上流に問い合わせる名前の一覧はコンテナの起動時に決まっていたので、動いたまま許可リストに足した宛先の名前を VM が引けなかった。一覧を `--servers-file` に書き、方針を入れ替えるときに書き直して読み直させる。

残したもの：設定の変更の履歴（今は最後に変えた人と時刻だけ）、team の maintainer による team の設定の変更、設定の層へのワークスペースの層の追加。

更新するときの注意：`dns-update.sh` を入れる前のランナーのイメージで動いているランナーは、ゲートウェイを入れ替えて引き取ったあとに方針を入れ替えると、プロキシの方針は入れ替わるが DNS の許可リストを作り直せずに失敗する（`docker exec` が見つからないスクリプトで失敗する）。そのランナーは一度止めて、新しいイメージで起動し直す。

## 段階 9：複数台の物理サーバー

今はゲートウェイ、controller、ランナーが一台のホストにあり、Unix ソケットと Docker の CLI でつながっている。
物理サーバーが複数台になったときのために、次のように分ける。

- **コントロールプレーン**：ゲートウェイ、controller、シークレット、データベース。一か所（冗長化するなら数台）に置く。データベースは PostgreSQL に移す。
- **ランナーのホスト（`chloe-node`）**：各物理サーバーに置く常駐プロセス。ランナーの起動と停止、方針の入れ替え、guest-agent への接続の中継を、コントロールプレーンからの HTTPS の API で受ける。
- **通信**：コントロールプレーンと `chloe-node`、ランナーのプロキシとシークレットの問い合わせのあいだは、HTTPS（相互 TLS）にする。ランナーごとに短命のクライアント証明書かトークンを発行し、問い合わせたランナーを特定する。
- **端末の中継**：ゲートウェイから VM の guest-agent への接続は、`chloe-node` が vsock を HTTPS の上（Upgrade した接続）に中継する。
- **置き場所**：ワークスペースのディスクはホストのローカルにあるので、ワークスペースは作ったホストに結びつける。ホストの空きで新しいワークスペースの置き場所を決める。ホストをまたいで移すのは、ディスクを複製する別の作業として扱う。
- `RunnerBackend` に、`chloe-node` へ HTTPS で頼む実装を足す。Docker の実装は一台用の開発環境として残す。

あわせて、`chloe` の管理コマンドで行う読み書き（ワークスペース、シークレット、設定）を、HTTP(S) の API からも行えるようにする。

### 決めたこと（2026-10-17）

- **データベース**：SQLite のままにする。データベースを使うのはコントロールプレーンだけで、ホストを分けるのに PostgreSQL は要らない。コントロールプレーンを冗長化するときに移す。
- **証明書**：コントロールプレーンが CA を持ち、`chloe-node` の証明書を発行する。`chloe-gateway node add <名前> --url <URL>` で一度だけ使える参加用のトークンを作り、`chloe-node join` がそのトークンで鍵の署名を頼む。トークンには CA の証明書の指紋を含め、`chloe-node` は最初の接続でコントロールプレーンを確かめる。
- **本物の値の問い合わせ**：ランナーのプロキシは今と同じくホストのローカルの Unix ソケットに尋ね、`chloe-node` がそれを相互 TLS でコントロールプレーンへ中継する。コントロールプレーンは、問い合わせてきた `chloe-node`（クライアント証明書）に置いたランナーの問い合わせにだけ答える。`chloe-node` が乗っ取られても、そのホストのランナーのシークレットまでに留まる。ランナーごとの資格情報は作らない。
- **管理 API の認証**：`chloe token create [--expires 30d]` で SSH から発行するトークンを `Authorization: Bearer` で渡す。データベースにはハッシュだけを置き、失効と期限を持つ。トークンの権限は、発行した利用者の権限と同じにする。

### 進め方

1. **9a ランナーとの接続の抽象化**：controller とゲートウェイが vsock のパスと問い合わせ用ソケットのパスを直接使っている部分を、`RunnerBackend` の「guest-agent につなぐ」操作と、controller の「問い合わせに答える」操作にまとめる。Docker の実装の動きは変えない。
2. **9b 証明書**：CA、`chloe-node` とコントロールプレーンの証明書の発行、参加用のトークン、ホストの一覧（データベースの `nodes`）。
3. **9c `chloe-node`**：ホストで Docker の実装を動かし、相互 TLS の API で受ける（ランナーの起動、停止、一覧、方針の入れ替え、guest-agent への接続の Upgrade での中継）。ランナーごとの問い合わせ用ソケットを作り、コントロールプレーンへ中継する。
4. **9d コントロールプレーンの側**：`chloe-node` に頼む `RunnerBackend`、複数のホストへの置き場所の決め方（ワークスペースを最初に起動したホストに結びつけ、新しいワークスペースは動いているランナーが少ないホストに置く）、`chloe-node` からの問い合わせと参加を受ける相互 TLS の API。
5. **9e 管理 API**：管理コマンドの処理を、文字列を返す形から型のある結果を返す形に分け、SSH のコマンドと HTTP の API の両方から使う。API のトークンを発行、一覧、失効するコマンドを足す。
6. **9f 確認**：一台のマシンで `chloe-node` を二つ動かし、ワークスペースが二つのホストに置かれ、それぞれで端末、ポート転送、シークレットの差し替えが動くこと、クライアント証明書のない接続とほかのホストのランナーへの問い合わせが拒否されることを確かめる。管理 API で CRUD と権限を確かめる。

### 結果（2026-10-17）

VM を使う確認は 7 つ、275 項目がすべて通った（並べて 71 秒）。

- **9a**：`RunnerBackend` に `connect_agent`（guest-agent への接続）と `local_broker_socket`（同じホストで問い合わせを待ち受けるときだけ）を持たせ、controller とゲートウェイからパスの扱いをなくした。中継されてきた問い合わせには `Controller::answer_secret` で答える。
- **9b `chloe-mtls`**：CA、証明書の発行（署名要求の名前や用途は使わず、発行する側で決める）、役割を表す CN（`chloe-control-plane`、`chloe-node/<名前>`）、相互 TLS の待ち受け（HTTP/1.1、Upgrade つき）とクライアント、参加の前の CA の指紋での確認。同じ CA が両方の証明書を発行するので、受け取った側が相手の役割を確かめる。
- **9c `chloe-node`**：`join` で証明書を受け取り、`run` で Docker の実装を API で提供する。コントロールプレーンの証明書の相手だけを受け付ける。ランナーごとの問い合わせ用ソケットを起動の前に作り、問い合わせをコントロールプレーンへ中継する。再起動したら、動いているランナーの分を作り直す。
- **9d コントロールプレーン**：設定の `[cluster]` で、ランナーを `chloe-node` のホストに置く。`chloe-gateway node add|list|rm` でホストを登録し、一度だけ使える参加用のトークン（CA の指紋つき）を作る。ワークスペースは最初に起動したホストに結びつけ、新しいワークスペースは置いたワークスペースが少ないホストに置く。内部 API は、そのホストに置いたランナーの問い合わせにだけ答え、断ったら監査ログに残す。つながらないホストのランナーは、記録のとおり動いているものとして扱う（reconcile が片づけない）。
- **9e 管理 API**：管理コマンドの処理を `ops` にまとめ、SSH のコマンドと HTTP(S) の API（設定の `[api]`）の両方から使う。`chloe token create|list|revoke` で、期限と失効のあるトークンを作る。API の呼び出しは `via="api"` とトークンの ID をつけて監査ログに残す。

設定の例は `images/guest/dev/gateway.example.toml`（`[cluster]` と `[api]`）と `deploy/node.toml`。

残したもの：

- ホストをまたいだワークスペースの移動（ディスクの複製）と、ホストが壊れたときの置き直し。
- コントロールプレーンの冗長化と PostgreSQL。
- 証明書の更新と失効の一覧。証明書の有効期間は 1 年で、コントロールプレーンの証明書は起動のたびに発行し直し、`chloe-node` の証明書は `node add` と `join` をし直す。登録を消したホストの証明書は、内部 API でも、ほかのホストの API でも（コントロールプレーンの役割でないので）使えない。一方で、漏れたコントロールプレーンの証明書は、有効なあいだ失効させる手段がない（今は CA を作り直して、すべてのホストを参加し直させる）。
- ホストの空き（CPU、メモリ、ディスク）を見た置き場所の決め方。
- ホストがつながらないあいだにゲートウェイを再起動すると、そのホストのランナーを引き取ったあと、停止の予約で止めようとして失敗し、引き取り直すことを繰り返す（害はないが、警告がそのたびに出る）。
- 登録済みのホストに `node add` をし直しても参加済みのままに見える（証明書を作り直すときは、そのホストで `join` をし直す）。

## 段階 10：接続と管理の使い勝手

段階 9 までで、機能はそろった。
使ってみると、次の三つが不便だった。

- **どのワークスペースにいるか**：VM のホスト名がどれも `chloe` なので、プロンプトを見てもワークスペースが分からない。
- **ログイン**：GitHub に登録した公開鍵だけで入れる。そのため、GitHub のユーザートークンを持たない利用者が出る。個人のワークスペースで GitHub を使うには、別に `chloe github login` が要る。ユーザー名にも GitHub のログイン名を毎回書く必要がある。
- **管理**：シークレットと Organization の設定は、TOML を書いて標準入力から渡す。定義の形を覚えていないと書けない。

### 決めたこと（2026-10-17）

- **ホスト名**：VM のホスト名をワークスペースの名前にする。`/` と `_` は `-` に置き換え、63 文字に収める（`acme/dev/tools` は `acme-dev-tools`）。個人のワークスペースには login を付けない（`default` のまま）。PS1 は変えない。Ubuntu の `~/.bashrc` が PS1 を上書きし、しかもホームディレクトリは残るので、`/etc/profile.d` で決めても効かないからである。ホスト名なら、既定の PS1、tmux、端末のタイトルにそのまま出る。正確な名前は環境変数 `CHLOE_WORKSPACE` で渡す。
- **Device Flow を必須にする**：GitHub App を設定していれば、GitHub に登録した鍵でも、ユーザートークンがない（失効して更新もできない）利用者は Device Flow に進める。公開鍵の段階を部分的な成功にして、keyboard-interactive だけを続けさせる。トークンを取り消しまで確かめるために、接続のたびに GitHub の API を呼ぶことはしない。GitHub App がなければ、今までどおり公開鍵だけで入る。
- **共通のユーザー名**：`git@github.com` と同じく、誰でも同じユーザー名（設定の `ssh_user`、既定は `chloe`）で入れる。GitHub に存在しない名前も同じに扱う（`{github_url}/{name}.keys` が 404 を返すかで決める。通信の失敗は存在しないとはみなさない）。このユーザー名では、Device Flow で承認したアカウントを利用者にし、そのとき使った公開鍵を結び付けて覚える。次からはその鍵で入れる。GitHub には鍵から持ち主を引く API がないので、結び付けはゲートウェイが持つ。一つの鍵は一人にだけ結び付け、ほかの人の Device Flow では結び付け直さない。GitHub のログイン名で入ったときは結び付けない。GitHub から消した鍵が chloe で使えたままになるのを避けるためである。
- **Device Flow のやり直し**：Device Flow は一つの接続で一度だけ行う。失敗したら、やり直すには接続し直す。OpenSSH のクライアントは keyboard-interactive を何度か試し直すので、試し直しのたびにコードを作ると、利用者には新しいコードが次々に出るからである。
- **管理の TUI**：TUI はゲートウェイで動かし、SSH のチャネルに描く（`ssh -t <host> chloe`）。手元に入れる CLI と管理 API のトークンは要らない。操作は管理コマンドと管理 API と同じ `ops` で行う。シークレットと設定は TOML の編集ではなく、欄ごとのフォームで編集する。端末を割り当てない `chloe` は、今までどおりコマンドの一覧を出す。

### 進め方

1. **10a ホスト名**：ランナーがゲストの環境変数で名前とホスト名を渡し、ゲストの `chloe-load-config` がホスト名と `/etc/hosts` を書き換える。
2. **10b ログイン**：公開鍵の結び付け（データベースの `ssh_keys`）、ユーザー名の判定（GitHub のログイン名か、共通のユーザー名か）、トークンの有無による部分的な成功、Device Flow の結果での利用者の決定。`chloe whoami` と `chloe key list|rm` を足す。
3. **10c 管理の TUI**：ratatui のバックエンドを SSH のチャネルに向け（端末の大きさは SSH の pty-req と window-change から得る）、入力のバイト列を terminput でキーにする。フォームの部品（一行の入力、隠す入力、選択、行の一覧）を作り、シークレットと設定を TOML と同じ形に変換する。定義を変えるときは、値を空にした欄に今の値を使う。
4. **10d 確認**：VM を使う確認の `github` に、Device Flow を必須にしたログイン、共通のユーザー名、TUI の操作を足す。TUI は `script` で端末を割り当てた `ssh -tt` を動かし、キーを fifo から送って画面を待つ。

### 結果（2026-10-17）

VM を使う確認は 7 つ、320 項目がすべて通った（並べて 79 秒）。

- **10a**：使い捨てと保持のどちらのルートでも、ホスト名と `/etc/hosts` が変わることを `root` で確かめた。
- **10b**：鍵は合うがトークンがない場合、別のアカウントで承認した場合、組織外の利用者、共通のユーザー名と存在しない名前で入る場合、鍵なしで入る場合、ログアウトしてから入り直す場合を確かめた。クライアントの試し直しを有効にしたままでも、失敗した Device Flow が一つの接続で一度だけであることも確かめた。
- **10b の表示**：askpass には keyboard-interactive の説明が渡らないので、確認ではコードと失敗の理由を監査ログで確かめる。端末では説明が出ることを、`script` で割り当てた端末で一度確かめた。
- **10c**：単体テストで、フォームの移動と入力、値を画面に出さないこと、雛形とシークレットの定義の往復、設定の往復を確かめた。VM を使う確認で、TUI を開き、ワークスペースを作り、Organization のシークレットと設定を見て、閉じ、監査ログに `via="tui"` が残ることを確かめた。

残したもの：

- Device Flow の始まりを絞る仕組み。共通のユーザー名では、本人を確かめる前に Device Flow を始めるので、接続を繰り返されると GitHub App の Device Flow の上限を使い切られるおそれがある。接続元ごとの制限か、同時に待つ Device Flow の数の上限を足す。
- ある名前を GitHub に誰かが登録すると、その名前は GitHub のログイン名として扱われる。それまでその名前で入っていた人は入れなくなる（共通のユーザー名で入れば影響しない）。
- ssh-agent に鍵がいくつもあると、クライアントが最初に出した鍵が結び付く。
- `chloe whoami` と `chloe key list|rm` は、SSH と TUI からしか使えない（管理 API にはない）。
- TUI は、実際の端末での色、罫線、操作中のウィンドウの大きさの変更をまだ確かめていない。大きさの変更は単体テストだけで確かめた。

## 段階 11：Docker を使わないランナー

今のランナーは、ワークスペースごとに特権コンテナ（`chloe-dev-runner`）を起動し、その中で Firecracker とプロキシを動かす。
Docker を使ったのは開発機の事情による（段階 1 の「開発用の起動はコンテナの中で行う」）。
ホストに配置するには、次の三つが不便である。

- **権限**：ゲートウェイと `chloe-node` を動かす利用者は `docker` のグループに入るので、root と同じことができる。
- **配置**：ランナーのコンテナのイメージを、ホストごとにビルドするか写す必要がある。deb のようなパッケージにも入れられない。
- **ビルド**：`chloe-egress` をコンテナ（Ubuntu 26.04）の中で動かすので、ホストの glibc がコンテナより新しいとビルドしたものが動かない。

そこで、ランナーを systemd の unit として動かす `RunnerBackend` の実装（`SystemdBackend`）を足す。
計画書のとおり、物理サーバーではランナーをプロセス群として動かす形になる。
Docker の実装は、開発機での確認（`images/guest/dev/*-test.sh`）のために残す。

### 決めたこと（2026-10-18）

- **権限の分け方**：root で動かすのは、ネットワーク namespace、veth、nftables、tap を作る部分だけにする。egress、dnsmasq、firecracker は `chloe` の利用者で動かす。firecracker は `kvm` のグループで `/dev/kvm` を開き、tap は持ち主を `chloe` にして作っておけば開ける。ディスクの作成と拡大（`truncate`、`mkfs.ext4`、`e2fsck`、`resize2fs`）も `chloe` で行う。root の側が `chloe` の書けるディレクトリのファイルを扱うと、ディスクのイメージをホストのデバイスへのシンボリックリンクにすり替えられ、VM にホストのディスクを渡すおそれがあるからである。
- **ゲートウェイの利用者**：ゲートウェイと `chloe-node` は `chloe` のまま動かす。SSH を外に開くプロセスを root にしないためである。ランナーの unit の操作は、polkit の規則で `chloe` に許す。許すのは `chloe-runner@ws-<番号>.service` の `start`、`stop`、`reload` に限る。
- **unit の形**：パッケージに入れるテンプレートの unit を使う。パスをすべて引数で渡す transient の unit（`systemd-run`）は、polkit で許す範囲を unit の名前で絞りにくいので使わない。
  - `chloe-host-net.service`（root、oneshot、起動のとき）：ホスト側の nftables の表を置く。veth の名前のワイルドカード（`cw*`）で書き、ワークスペースごとには変えない。
  - `chloe-netns@.service`（root、oneshot、`RemainAfterExit`、`StopWhenUnneeded`）：インスタンス名（`ws-3` など）だけを受け取り、ネットワーク namespace、veth、namespace の中の nftables（今の entrypoint.sh の tap0 の規則）、tap0、namespace の中の sysctl（`net.ipv4.ip_unprivileged_port_start=0`。dnsmasq が root でなくても 53 番で待ち受けられる）を作る。止まるときに片づける。
  - `chloe-runner@.service`（`chloe`、追加のグループ `kvm`）：`chloe-netns@` を `Requires=` と `After=` で引き込み、`NetworkNamespacePath=` でその namespace に入る。ディスクの作成と拡大、設定用のドライブ、egress、dnsmasq、firecracker を受け持つ。`Restart=no` にし、止まったランナーの片づけは今と同じく reconcile に任せる。
- **設定の置き場所**：パス（`STATE_ROOT`、`GUEST_DIR`、`EGRESS_BIN`）とアドレスのプールは、root だけが書ける `/etc/chloe/runner.env` に書く。unit は `EnvironmentFile=` で、ゲートウェイと `chloe-node` は起動のときに、同じファイルを読む。パスを書く場所を一つにするためで、一つのホストに置けるランナーの設定は一組になる。ワークスペースごとの値（方針、ゲストの環境変数、ディスクの大きさ、ルートの扱い）は、今と同じく状態のディレクトリの `config/` に書く。
- **外への出口**：veth と NAT で出す。プールの /30 を、起動のときに空いている枠から割り当てる（`chloe-netns-up` が flock をかけて選ぶ）。ワークスペースの番号は使い回さず増え続けるので、番号からは決めない。ホスト側では、veth からホストへの入力と、veth からプライベートアドレス（RFC1918）への転送を落とし、外へ出るときに masquerade する。egress は宛先のアドレスを確かめないので、利用者が個人の規則で内部のアドレスに解決される名前を許すと、VM からホストの内部 API に届きうる。ホスト側の規則でこの経路を塞ぐ。
- **DNS の上流**：namespace の中から `127.0.0.53`（systemd-resolved）には届かない。`/etc/netns/<名前>/resolv.conf` は `ip netns exec` でしか使われないので、`chloe-runner@` で `BindReadOnlyPaths=/run/systemd/resolve/resolv.conf:/etc/resolv.conf` とし、本当の上流を見せる。上流が LAN の中にあるときは、プライベートアドレスを落とす規則の例外にする。
- **方針の入れ替え**：egress には今と同じく `egress.sock` で送る。VM の DNS の許可リストは、`systemctl reload` で作り直す（`ExecReload=` が一覧を書き直し、dnsmasq に SIGHUP を送る）。
- **systemd の操作**：`docker.rs` が `docker` を実行するのと同じく、`systemctl` を子プロセスとして実行する。D-Bus のクレート（zbus など）は使わない。操作は起動と停止のたびに一度ずつで速さは要らず、`systemctl start` はジョブが終わるまで待って結果を終了コードで返すので、ジョブの完了の信号を自分で待つ処理が要らない。polkit の判定は、どちらでも同じく D-Bus の呼び出し元で行われる。手で同じコマンドを打って確かめられる点も、運用で役に立つ。
- **firecracker の入手**：Ubuntu のアーカイブにないので、公式のリリースのバイナリを使う。
- **設定**：`[runner]` に `kind = "docker" | "systemd"` を足す。省いたら `docker` とし、今の設定をそのまま読めるようにする。`kind = "systemd"` のパスは `runner.env` から読む。

外への経路を namespace に持たせない形は、隔離がより強いが後回しにする。
socket の unit に `NetworkNamespacePath=` を付ければ、待ち受けのソケットだけを namespace の中に作り、egress はホストの側で動かせる。
そうすれば ip_forward も NAT も要らないが、DNS を egress が答えることと、egress が内部のアドレスを拒むことが要る。

### 進め方

1. **11a スクリプトの分割**：`images/guest/dev/entrypoint.sh` を、root の部分（`chloe-netns-up`：namespace、tap0、nftables）と、`chloe` の部分（`chloe-runner-start`：ディスク、設定用のドライブ、egress、dnsmasq、firecracker）に分ける。開発用のコンテナも、コンテナの中で前者に当たる準備をしてから後者を実行する形にし、二つの実装で処理が分かれないようにする。既存の VM を使う確認が通ることを確かめる。
2. **11b unit と権限**：三つの unit、polkit の規則、`runner.env` の例、ホスト側の nftables の表を `deploy/` に置く。
3. **11c `SystemdBackend`**：`crates/chloe-controller/src/systemd.rs` に実装し、`[runner]` の `kind`、ゲートウェイの `Backend` の選択肢、`chloe-node` の対応を足す。`chloe-node` は今 `DockerBackend` を直接持ち、問い合わせ用ソケットのパスを具体的な型で引くので、両方の実装を包む型に置き換える。各操作は次のとおり。
   - `start`：`config/` に書いてから `systemctl start chloe-runner@<key>`。
   - `stop`：`systemctl stop`。失敗した unit が一覧に残らないよう、unit に `CollectMode=inactive-or-failed` を付ける（`reset-failed` を polkit で許さずに済む）。
   - `is_running`：`systemctl is-active`。
   - `list`：`systemctl list-units --state=active 'chloe-runner@*'`。
   - `update`：`egress.sock` に送り、`systemctl reload`。
   - `connect_agent` と `local_broker_socket`：変えない。`vsock.sock`、`broker.sock`、`egress.sock` はパスで指す Unix ソケットで、namespace の影響を受けない。
4. **11d 確認**：入れ子の KVM を許した root の使える VM で、ランナーの起動と停止、ゲートウェイの再起動での引き取り、方針の入れ替え（DNS の許可リストを含む）、reconcile、VM からホストと内部のアドレスに届かないことを確かめる。開発機では今までどおり Docker の実装で確かめる。
5. **11e 文書**：インストールと設定の文書に、`kind = "systemd"` での配置と、Docker の実装から移るときの手順を書く。状態のディレクトリの形は変わらないので、ディスクと CA はそのまま引き継げる。ただし、切り替える前にすべてのワークスペースを止める。止めずに切り替えると、動いている Docker のコンテナを、新しい実装が自分のランナーとして見つけられないからである。

### 結果（2026-10-18）

VM を使う確認は 9 つ、398 項目がすべて通った（並べて 80 秒）。

- **11a**：`images/guest/runner/` に、namespace の中の準備（`chloe-runner-net`、root）、プロキシと DNS と Firecracker の起動（`chloe-runner-start`）、DNS の許可リストの作り直し（`chloe-dns-update`）を置いた。開発用のコンテナも同じスクリプトを使い、プロキシ、dnsmasq、Firecracker を状態のディレクトリの持ち主で動かす。dnsmasq は、namespace の中の `net.ipv4.ip_unprivileged_port_start=0` で、root でなくても 53 番で待ち受けられた。Firecracker は、`kvm` のグループと、持ち主を指定して作った tap0 で、root でなくても動いた。
- **11b**：ホスト側のスクリプト（`chloe-host-net`、`chloe-netns-up`、`chloe-netns-down`）、三つの unit、polkit の規則、`runner.env` の例を置いた。Firecracker が終わったあとに残ったプロキシが終了処理を待ち、unit が止まりきるまで数秒かかったので、`KillSignal=SIGKILL` にした（Docker の `docker rm -f` と同じ）。
- **11c**：`local` に Docker と systemd の実装の選択（`RunnerConfig`、`LocalBackend`）と共通の処理（方針とゲストの環境変数の書き込み、プロキシへの方針の送信、guest-agent への接続、状態の削除）をまとめ、`systemd` に `SystemdBackend` を置いた。ゲートウェイと `chloe-node` は `LocalBackend` を使う。ゲートウェイと `chloe-node` の unit は、Docker を必須にしない（`Requires=` を `Wants=` にした）。
- **11d 確認**：開発機には root の権限がないので、systemd を PID 1 にした Docker の特権コンテナ（Ubuntu 24.04、Docker なし）をホストの代わりにし、`deploy/` の unit、polkit の規則、ホストの規則をそのまま使う確認（`images/guest/dev/systemd-test.sh`）を足した。プロセスの利用者、外への通信と DNS、namespace からホストとプライベートアドレスへの遮断、方針の入れ替えでの DNS の許可リストの作り直し、二つのランナーの veth の割り当て、ゲートウェイの再起動での引き取り、polkit が許さない操作、止めたあとの namespace と veth の片づけと枠の再利用を確かめた。
- **実際のホストでの確認**：Docker、tailscale、systemd-resolved が動いている Ubuntu 24.04 のホストに手で置き、同じ項目を確かめた。DNS の上流は `/run/systemd/resolve/resolv.conf` の 1.1.1.1 になった。このホストでは Docker が FORWARD の既定を ACCEPT のままにしていたので、Docker の規則との衝突は起きなかった（Docker が FORWARD を DROP にしているホストは確かめていない）。

残したもの：

- Docker が FORWARD の既定を DROP にしているホストと、ufw を有効にしたホストでの確認。
- ホスト全体の `net.ipv4.ip_forward` を 1 にする。外への経路を namespace に持たせない形（「決めたこと」の最後）にすれば要らなくなる。
- DNS の 53 番は、プライベートアドレスにも通す。外へ出られるのは namespace の中のプロキシと dnsmasq だけで、dnsmasq は許可リストの名前だけを上流に問い合わせるが、上流が変わったときに上流だけに絞り直す仕組みはない。
- `nftables.service` を読み直すと、ホスト側の規則が消える（`chloe-host-net` を起動し直す必要がある）。
- jailer。Firecracker は `chloe` の利用者の権限で動くだけで、chroot、namespace、cgroup の制限はかけていない（seccomp は Firecracker の既定のもの）。
- deb のパッケージ。ランナーの unit とスクリプトの置き場所（今は `/usr/local/lib/chloe/runner`）は、パッケージにするときに `/usr/libexec/chloe` などへ移す。

## 段階 12：deb のパッケージ

今は、ビルドしたものを `install` で手で置く。
ホストを増やすたびに同じ手順を繰り返すことになり、更新でファイルを置き忘れやすい。
段階 11 でランナーのコンテナのイメージが要らなくなったので、ホストに置くものはすべてファイルになり、deb にまとめられる。

### 決めたこと（2026-10-18）

- **パッケージの分け方**：
  - `chloe-common`：`chloe-egress`、ランナーのスクリプト、Firecracker、ランナーの unit 三つ、polkit の規則、`/etc/chloe/runner.env`。利用者 `chloe`（`kvm` のグループ）と、状態のディレクトリ（`/var/lib/chloe`、`/var/lib/chloe/workspaces`）を作る。
  - `chloe-gateway`：`chloe-gateway`、その unit、`/etc/chloe/gateway.toml`。`chloe-common` に依存する。
  - `chloe-node`：`chloe-node`、その unit、`/etc/chloe/node.toml`。`chloe-common` に依存する。
  - `chloe-guest`：ゲストのカーネルとルートファイルシステム（約 2.3GB）。ビルドしたときの Claude Code と Codex を含むので、作るのは指定したときだけにし、公開する場所には置かない。`chloe-common` は `Suggests` で示すだけにする。
- **置き場所**：`/usr/bin`（`chloe-gateway`、`chloe-node`）、`/usr/libexec/chloe`（`chloe-egress`、ランナーのスクリプト、Firecracker）、`/usr/lib/chloe/guest`（ゲストのイメージ）。`deploy/` の unit と設定の例は、手で置くときの `/usr/local` のまま残し、パッケージを作るときにパスを書き換える。手で置く手順を壊さず、元を一つに保つためである。
- **既定の設定**：`deploy/gateway.toml` と `deploy/node.toml` の `[runner]` を `kind = "systemd"` にする。Docker で動かす書き方は、注釈と [設定](configuration.md) に残す。状態の置き場所は、ゲートウェイのホストでも `chloe-node` のホストでも `/var/lib/chloe/workspaces` にそろえ、`runner.env` を書き換えずに使えるようにする。
- **Firecracker**：Apache-2.0 で再配布できるので、公式のリリースのバイナリを `chloe-common` に入れる。`chloe-runner-start` は、同じディレクトリに `firecracker` があればそれを使い、なければ `PATH` から探す。
- **unit の扱い**：インストールしただけでは、どの unit も有効にせず、起動もしない。設定の例のままでは、ゲートウェイが 22 番で待ち受けて sshd とぶつかり、`allowed_users` も仮の名前だからである。更新のときは、動いているゲートウェイと `chloe-node` だけを起動し直す（動いているワークスペースは止まらない）。ランナーの unit は、更新では止めない。止めるのは `chloe-common` を消すときだけにする。
- **purge**：`/var/lib/chloe` は消さない。マスター鍵を失うと、保存したシークレットを読めなくなるからである。
- **ビルド**：`packaging/deb/build.sh` が Ubuntu 24.04 のコンテナの中でバイナリをビルドし、`dpkg-deb` でパッケージにする。24.04 の glibc（2.39）でビルドすれば 26.04 でも動く。依存（`Depends`）は `dpkg-shlibdeps` で求める。cargo-deb は使わない。Rust のクレートではない `chloe-guest` と、パスの書き換えを一つのスクリプトで扱えるからである。
- **版**：タグを打つまでは `0.1.0~git<日付>.<短いハッシュ>` にする。
- **配り方**：この段階では `build/deb/` にファイルを作るところまでにする。CI と apt のリポジトリは作らない。

### 進め方

1. **12a 既定を systemd に**：`deploy/` の設定の例を `kind = "systemd"` にし、状態の置き場所をそろえる。`chloe-runner-start` が同じディレクトリの Firecracker を使えるようにする。
2. **12b パッケージを作る**：`packaging/deb/` に、ビルド用のコンテナ、パッケージごとの control と maintainer script、`build.sh` を置く。lintian で検査する。
3. **12c 確かめる**：Ubuntu 24.04 と 26.04 の systemd を動かす特権コンテナに `apt install` で入れ、段階 11 と同じ確認を通す。更新（版を上げて入れ直す）で動いているワークスペースが止まらないこと、削除と purge のあとに何が残るかを確かめる。shigure にも入れて確かめる。
4. **12d 文書**：インストールの文書を、deb で入れる手順を先にして書き直す。手で置く手順は別の方法として残す。

### 結果（2026-10-18）

`packaging/deb/test.sh` の 32 項目が、Ubuntu 24.04 と 26.04 の両方で通った。

- **12a**：`deploy/` の設定の例のランナーを `kind = "systemd"` にし、状態の置き場所を `/var/lib/chloe/workspaces` にそろえた（tmpfiles.d で作る）。
- **12b**：`packaging/deb/build.sh` が Ubuntu 24.04 のコンテナでビルドし、`package.sh` が `dpkg-deb` で四つのパッケージを作る。依存は `dpkg-shlibdeps` で求め、`libc6 (>= 2.39)` などになった。利用者 `chloe` は sysusers.d で作る。maintainer script は、unit を `deb-systemd-invoke` と `deb-systemd-helper` で扱う。lintian の警告は、仮の Maintainer と man のページがないことだけを全体で抑え、Firecracker（static-pie）とゲストのカーネルについての警告は、パッケージの override でそのファイルにだけ当てた。三つのパッケージは合わせて約 15MB、`chloe-guest` は約 360MB になった。
- **12c 確認**：systemd を PID 1 にした Ubuntu の特権コンテナに `apt` で入れ、新しく入れる、ランナーを動かす、版を上げて入れ直す、消す、purge する、を確かめた（`packaging/deb/test.sh`）。途中で次の三つを直した。
  - 同じ apt の実行で polkitd を新しく入れると、D-Bus が利用者 `polkitd` を作る前の設定のままで、polkitd が名前を取れずに終わり、ランナーの unit を起動できなかった。`chloe-common` の postinst で D-Bus の設定を読み直させ、polkit を起動し直させる。
  - VM が動いたままランナーを止めると、SIGKILL で終わった unit が failed になった。`SuccessExitStatus=SIGKILL` を足した。
  - 更新で `chloe-host-net` を restart すると、`Requires=` でたどるランナーまで止まる。`ExecReload=` を足して reload だけを使い、規則の置き直しを一つのトランザクションにした。

  公式の Ubuntu のイメージはサービスの操作を断る `policy-rc.d` を入れているので、確認では消して実際のホストに合わせた。
- **実際のホストでの確認**：段階 11 で使ったホスト（Ubuntu 24.04）で、手で置いたものを片づけてから deb を作って入れ、段階 11 と同じ項目を確かめた。VM を動かしたまま `chloe-guest` を更新すると、動いている Firecracker は消えた古いファイルを開いたまま読み続け、ゲストの中でページキャッシュを捨ててから読み直しても動いた。

残したもの：

- CI で deb を作ることと、apt のリポジトリ。
- 版の付け方。タグを打つ運用を決めたら、`~git` を付けない版にする。
- man のページ。
- Docker でランナーを動かすとき（`kind = "docker"`）に要る `chloe-dev-runner` のイメージは、deb に入れていない。

## 段階 13：ゲストのツールを mise に

ゲストのイメージには、ビルドしたときの Claude Code と、版を決めた Codex を入れていた。
エージェントを更新するにはイメージを作り直す必要があり、ほかのエージェントや言語の処理系は利用者が自分で入れるしかなかった。
イメージにビルドしたときのエージェントが入るので、`chloe-guest` を公開する場所に置けない理由にもなっていた。

### 決めたこと（2026-10-18）

- **mise を入れる**：Claude Code と Codex を外し、[mise](https://mise.jdx.dev/) を入れる。リリースのバイナリを版を決めて取り、minisign の署名と SHA-256 を確かめる。ツールは利用者がホームディレクトリ（ワークスペースのディスク）に入れるので、VM を止めても残り、イメージを作り直さずに更新できる。
- **mise の更新**：mise はイメージに入れてあり、`mise self-update` では更新できない。`/etc/mise/config.toml` で新しい版の問い合わせと警告を止める。
- **PATH**：ログインシェルでは `/etc/profile.d/mise.sh` で mise を有効にする（対話のシェルでは `mise activate`、そうでなければ shims）。`ssh <ホスト> 'コマンド'` は、OpenSSH と同じく `bash -c` で動かしていたので profile を読まなかった。ゲートウェイが `bash -l -c` で動かすように変え、端末を開いたときと同じ設定で動くようにする。profile.d の chloe のスクリプトは何も出力しないので、scp のように出力を使うコマンドを壊さない（利用者が `~/.profile` で出力すると壊れるのは、OpenSSH の `~/.bashrc` と同じである）。
- **フック**：Claude Code と Codex の作業中のフックの設定（`/etc/claude-code/managed-settings.d/`、`/etc/codex/hooks.json`）は残す。利用者が mise で入れれば、そのまま効く。
- **外への通信**：mise はツールの版の一覧を `mise-versions.jdx.dev` から、ツールの多くを GitHub のリリースから取る。既定の規則に `mise-versions.jdx.dev` を足す。

### 結果（2026-10-18）

VM を使う確認は 9 つ、397 項目のうち 396 項目が通った（並べて 85 秒）。あとから `mvp` に、ゲートウェイを通した `ssh <ホスト> 'コマンド'` の PATH に mise の shims が入ることを確かめる項目を足した。
落ちた 1 項目（`node` の、ゲートウェイを起動し直したあとに二つのランナーを引き取ること）は、`node` だけで二回実行すると通った。
並べて実行したときの負荷で待ち時間（10 秒）が足りなかったとみていて、この段階の変更とは関係しない。

- ルートファイルシステムは 2.3GB から 1.6GB になった。
- `smoke` で mise が動くこと、`net` で mise がプロキシを通して（chloe の CA を信頼して）版の一覧を引き、GitHub のリリースからツールを入れられることを確かめる。
- 実際の Claude Code と Codex を動かしていた確認は外した。フックの確認は、設定に書いたコマンドを、エージェントと同じ名前のプロセスから呼び、作業中の印ができて消えることを確かめる形にした。Claude Code と Codex が TLS の検証を通ることと、フックを実際に呼ぶことは確かめなくなった。

## 段階 14 以降の進め方

段階 14 以降は、各段階に入るときに、この文書に手順を書き足してから実装する。
段階 2 の結果によって段階 4 の設計（HTTP/2 の扱い、差し替えの位置など）が変わりうるので、先に細かく決めない。

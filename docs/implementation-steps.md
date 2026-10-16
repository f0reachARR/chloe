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
| 7 | 運用機能 | スナップショット、ポート転送、監査ログ | 切断と再接続で状態が戻る |
| 8 | Kubernetes | runner の Pod 化、PVC、NetworkPolicy | 段階 6 のテストが Kubernetes 上で通る |

段階 1 と 2 は互いに依存しないので並行して進められる。
段階 3 から 5 は、それぞれ前の段階の上に積む。

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

## 段階 4 以降の進め方

段階 4 以降は、各段階に入るときに、この文書に手順を書き足してから実装する。
段階 2 の結果によって段階 4 の設計（HTTP/2 の扱い、差し替えの位置など）が変わりうるので、先に細かく決めない。

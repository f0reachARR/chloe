# chloe

chloe は、Claude Code や Codex などのコーディングエージェントを、利用者ごとの使い捨てにできる microVM で動かすための SSH サーバーである。
利用者は GitHub のアカウントで `ssh` するだけで、自分のワークスペースの VM に入れる。
API キーや GitHub のトークンは VM に入れず、VM からの通信を中継するプロキシが、宛先ごとに本物の値へ差し替える。

```console
$ ssh alice@chloe.example.com
user@chloe:/workspace$ claude
```

## 状態

開発中である。
ランナー（ワークスペースの VM とプロキシ）は、特権コンテナの中で Firecracker を動かす開発用の実装だけがある。
jailer による Firecracker の隔離、systemd の unit、Kubernetes への配置はまだない。
実装の順序と各段階の結果は [実装手順](docs/implementation-steps.md) にある。

## できること

- **GitHub での認証**：GitHub に登録した公開鍵で SSH にログインする。GitHub App を設定すると、公開鍵のない利用者は Device Flow でログインでき、Organization と team の所属で共有ワークスペースに入れる。
- **ワークスペース**：利用者ごとの個人ワークスペースと、Organization や team の共有ワークスペース。VM の中では Docker も使える。最後の接続が切れてしばらくすると VM を止め、次の接続で起動し直す。エージェントが作業中のあいだは止めない。
- **シークレットの差し替え**：VM の中にはプレースホルダーだけを置く。VM からの HTTPS は透過プロキシで復号し、宛先とヘッダーの規則に合うときだけ、プレースホルダーを本物の値に差し替える。
- **通信の制限**：許可リストにない宛先への通信と名前の解決を遮断する。宛先、メソッド、パスの規則で、API の使い方も絞れる。
- **管理**：SSH の `chloe` コマンドか HTTP(S) の API で、ワークスペース、シークレット、Organization と team の設定を扱う。
- **複数のホスト**：ランナーを複数の物理サーバーに置ける（`chloe-node`）。コントロールプレーンとホストのあいだは相互 TLS でつなぐ。
- **監査**：ログイン、セッション、ポート転送、管理の操作、シークレットの問い合わせを JSON Lines で記録する。HTTP の通信は、ワークスペースごとにメタデータだけを記録する。

## 構成

```text
             SSH                        相互 TLS (複数のホストのとき)
利用者 ───────────────> chloe-gateway ──────────────────────> chloe-node
                        (コントロールプレーン)                 (ランナーのホスト)
                        - GitHub での認証                       │
                        - ワークスペースとシークレット            │ ランナーごとに
                        - 管理コマンドと管理 API                 ▼
                        - 本物の値の問い合わせに答える   ┌──────────────────────┐
                                  ▲                     │ Firecracker の VM     │
                                  │ 本物の値の問い合わせ  │  guest-agent          │
                                  │                     │  Claude Code、Codex   │
                                  └──────────────────── │  Docker               │
                                                        ├──────────────────────┤
                                                        │ chloe-egress          │──> 許可した宛先
                                                        │ (透過プロキシ、差し替え) │
                                                        └──────────────────────┘
```

一台で動かすときは、ゲートウェイが同じホストの Docker でランナーを起動する。

## はじめかた

KVM が使える x86_64 の Linux で、リポジトリのルートで次のように実行する。
必要なものと詳しい手順は [インストール](docs/install.md) にある。

```sh
images/guest/kernel/build.sh
images/guest/rootfs/build.sh
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest/dev
cargo build -p chloe-egress -p chloe-gateway
# allowed_users に自分の GitHub のログイン名を書いてから起動する。
target/debug/chloe-gateway --config images/guest/dev/gateway.example.toml
```

別の端末から、GitHub に登録した鍵で入る。

```sh
ssh -p 2222 <GitHub のログイン名>@127.0.0.1
```

## 文書

| 文書 | 内容 |
| --- | --- |
| [インストール](docs/install.md) | 必要なもの、ビルド、一台での起動、複数のホスト、更新、バックアップ |
| [使い方](docs/usage.md) | 接続、ワークスペース、シークレット、ポート転送、`chloe` コマンド |
| [設定](docs/configuration.md) | ゲートウェイ、`chloe-node`、GitHub App、Organization と team の設定の書き方 |
| [管理 API](docs/api.md) | HTTP(S) の API とトークン |
| [GitHub App の準備](docs/github-app.md) | アプリの登録と権限 |
| [開発](docs/development.md) | クレートの構成と確認の回し方 |
| [調査と計画](docs/research-and-plan.md) | 設計の前提と、ほかの選択肢の比較 |
| [実装手順](docs/implementation-steps.md) | 段階ごとの計画と結果 |

## リポジトリの構成

```text
crates/
  chloe-gateway      SSH の入口とコントロールプレーン (認証、管理コマンド、管理 API、内部 API)
  chloe-controller   ワークスペース、ランナー、シークレットの問い合わせ、データベース
  chloe-node         ランナーのホストの常駐プロセス
  chloe-mtls         コントロールプレーンとホストのあいだの相互 TLS
  chloe-egress       透過プロキシ (TLS の復号、許可リスト、差し替え、HTTP のログ)
  chloe-policy       許可リスト、パスの規則、差し替えの規則
  chloe-secrets      シークレットの暗号化と保管、1Password Connect
  chloe-github       GitHub App (インストールトークン、所属の確認、Device Flow)
  chloe-guest-agent  VM の中の常駐プロセス (vsock 越しの端末と exec)
  chloe-agent-proto  ホストと guest-agent のあいだのプロトコル
  chloe-runner       VM に直接つなぐ開発用の CLI
images/guest/
  kernel/            ゲストのカーネルのビルド
  rootfs/            ゲストのルートファイルシステムのビルド
  dev/               開発用のランナーのコンテナ、設定の例、VM を使う確認
migrations/          データベースのマイグレーション
docs/                文書
```

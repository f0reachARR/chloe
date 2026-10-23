# chloe

chloe は、Claude Code や Codex などのコーディングエージェントを、利用者ごとの使い捨てにできる microVM で動かすための SSH サーバーである。
利用者は GitHub のアカウントで `ssh` するだけで、自分のワークスペースの VM に入れる。
API キーや GitHub のトークンは VM に入れず、VM からの通信を中継するプロキシが、宛先ごとに本物の値へ差し替える。

```console
$ ssh alice@chloe.example.com
user@chloe:/workspace$ mise use -g <ツール>   # Claude Code や Codex などは mise で入れる
user@chloe:/workspace$ claude
```

## 状態

開発中である。
ランナー（ワークスペースの VM とプロキシ）は、特権コンテナの中で Firecracker を動かす開発用の実装と、systemd の unit で動かす実装がある。
systemd の実装では、root で動くのはネットワークの準備だけで、Firecracker とプロキシは専用の利用者で動く。
jailer による Firecracker の隔離と Kubernetes への配置はまだない。
ホストに配置するための設定の例と systemd の unit は `deploy/` にあるが、まだ実際のホストで確かめていない。
Web の画面の GitHub のログインは、GitHub の代わりのスタブを相手にしか確かめていない。
実装の順序と各段階の結果は [実装手順](docs/implementation-steps.md) にある。

## できること

- **GitHub での認証**：GitHub App を設定すると、SSH のログインに GitHub の Device Flow を使う。`ssh chloe@host` で誰でも同じユーザー名を使い、承認したときの公開鍵を覚えて次から使う。Organization と team の所属で共有ワークスペースに入れる。GitHub App がなければ、GitHub に登録した公開鍵でログインする。
- **ワークスペース**：利用者ごとの個人ワークスペースと、Organization や team の共有ワークスペース。VM の中では Docker も使える。最後の接続が切れてしばらくすると VM を止め、次の接続で起動し直す。エージェントが作業中のあいだは止めない。
- **シークレットの差し替え**：VM の中にはプレースホルダーだけを置く。VM からの HTTPS は透過プロキシで復号し、宛先とヘッダーの規則に合うときだけ、プレースホルダーを本物の値に差し替える。
- **通信の制限**：許可リストにない宛先への通信を遮断し、名前の解決も外に問い合わせない。宛先、メソッド、パスの規則で、API の使い方も絞れる。
- **管理**：SSH の `chloe` コマンド、`ssh -t <host> chloe` で開く管理の画面（TUI）、HTTP(S) の API で、ワークスペース、シークレット、外への通信の規則（個人、Organization、team、ワークスペースごと）を扱う。
- **複数のホスト**：ランナーを複数の物理サーバーに置ける（`chloe-node`）。ホストからコントロールプレーンへ相互 TLS のトンネルを張るので、コントロールプレーンからホストへ届かなくてもよい。
- **Web の画面**：ブラウザ（スマートフォンでも、PWA としても）から、エージェントからの依頼に答え、ワークスペースでエージェントが作業中かを見る。GitHub か、SSH の `chloe web login` が出す URL でログインする。
- **エージェントからの依頼**：VM の中のエージェントは、`chloe-ask`（CLI と MCP のツール）で、遮断された宛先への通信の許可や質問を人に出し、答えを待てる。人は Web の画面、管理の画面、`chloe inbox`、管理 API で答える。無人で動かしていても、エージェントは止まらずに答えを待てる。
- **監査**：ログイン、セッション、ポート転送、管理の操作、シークレットの問い合わせを JSON Lines で記録する。HTTP の通信は、ワークスペースごとにメタデータだけを記録し、利用者は `chloe ws log` と管理の画面で見られる。

## 構成

```text
             SSH                        chloe-node が張るトンネル (相互 TLS、複数のホストのとき)
利用者 ───────────────> chloe-gateway <────────────────────── chloe-node
                        (コントロールプレーン)                 (ランナーのホスト)
                        - GitHub での認証                       │
                        - ワークスペースとシークレット            │ ランナーごとに
                        - 管理コマンドと管理 API                 ▼
                        - 本物の値の問い合わせに答える   ┌──────────────────────┐
                                  ▲                     │ Firecracker の VM     │
                                  │ 本物の値の問い合わせ  │  guest-agent          │
                                  │                     │  mise、エージェント   │
                                  └──────────────────── │  Docker               │
                                                        ├──────────────────────┤
                                                        │ chloe-egress          │──> 許可した宛先
                                                        │ (透過プロキシ、差し替え) │
                                                        └──────────────────────┘
```

一台で動かすときは、ゲートウェイが同じホストの Docker でランナーを起動する。

## 分かっている制限

- Firecracker に jailer による隔離（chroot、namespace、cgroup）をかけていない。systemd の実装でも、Firecracker は専用の利用者の権限で動くだけである。
- 共有ワークスペースのメンバーは、VM の中では互いに root と同じことができる。
- プロキシの本文の検査（プレースホルダーが別の宛先に送られていないかの確認）は、圧縮した本文、チャンクで送る本文、1MiB を超える本文、WebSocket の中身を見ない。
- 中継のみの規則（`action = "passthrough"`）の宛先には、ほかの規則も差し替えも効かない（ほかの規則と宛先が重なる設定は受け付けない）。
- VM の DNS の許可リストで `*.example.com` と書くと、example.com そのものも引ける。
- VM を止めると、VM の中で動かしていたプロセスは終わる（スナップショットは [PoC](docs/poc/stage7-snapshot.md) だけで、採っていない）。
- `chloe ws rm` でワークスペースを消しても、ディスクは残る。
- 管理 API は、証明書を設定しなければ HTTP で待ち受ける。
- 複数のホストでは、ワークスペースをホストのあいだで移せず、置き場所はホストの空きを見ない。漏れたコントロールプレーンの証明書を失効させる手段がない。
- GitHub App を Any account で公開すると、だれでもインストールして使えるので、App の設定の `organizations` で限る必要がある。

## はじめかた

KVM が使える x86_64 の Linux で、リポジトリのルートで次のように実行する。
ここでは開発用の設定で、リポジトリの中のファイルをそのまま使う。
ホストに配置して動かす手順は [インストール](docs/install.md) にある。

```sh
images/guest/kernel/build.sh
images/guest/rootfs/build.sh
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest
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
| [インストール](docs/install.md) | deb の作り方と入れ方、一台での起動、複数のホスト、更新、バックアップ、手で置く手順 |
| [使い方](docs/usage.md) | 接続、ワークスペース、シークレット、ポート転送、`chloe` コマンド |
| [設定](docs/configuration.md) | ゲートウェイ、`chloe-node`、GitHub App、個人と Organization と team の設定、ワークスペースの規則の書き方 |
| [管理 API](docs/api.md) | HTTP(S) の API とトークン |
| [GitHub App の準備](docs/github-app.md) | アプリの登録と権限 |
| [開発](docs/development.md) | クレートの構成と確認の回し方 |
| [Web の画面の方針](docs/web.md) | Web の画面の技術、構成、依存の向き、安全のための決まり |
| [調査と計画](docs/research-and-plan.md) | 設計の前提と、ほかの選択肢の比較 |
| [OpenShell の調査](docs/research-openshell.md) | NVIDIA OpenShell に委ねられる範囲と、取り入れる候補 |
| [実装手順](docs/implementation-steps.md) | 段階ごとの計画と結果 |

## リポジトリの構成

```text
crates/
  chloe-gateway      SSH の入口とコントロールプレーン (認証、管理コマンド、管理 API、内部 API)
  chloe-controller   ワークスペース、ランナー、シークレットの問い合わせ、データベース
  chloe-node         ランナーのホストの常駐プロセス
  chloe-mtls         コントロールプレーンとホストのあいだの相互 TLS とトンネル
  chloe-egress       透過プロキシ (TLS の復号、許可リスト、差し替え、HTTP のログ)
  chloe-policy       許可リスト、パスの規則、差し替えの規則
  chloe-secrets      シークレットの暗号化と保管、1Password Connect
  chloe-github       GitHub App (インストールトークン、所属の確認、Device Flow)
  chloe-guest-agent  VM の中の常駐プロセス (vsock 越しの端末と exec)
  chloe-agent-proto  ホストと guest-agent のあいだのプロトコル
  chloe-ask          VM の中のエージェントが人に依頼を出す CLI と MCP のサーバー
  chloe-runner       VM に直接つなぐ開発用の CLI
images/guest/
  kernel/            ゲストのカーネルのビルド
  rootfs/            ゲストのルートファイルシステムのビルド
  runner/            ランナーの起動のスクリプト (開発用のコンテナと systemd の unit で共通)
  dev/               開発用のランナーのコンテナ、開発用の設定の例、VM を使う確認
web/                 Web の画面 (React、Mantine、TanStack Router と Query。型は web/openapi.json から作る)
deploy/              ホストに配置するための設定の例、systemd の unit、polkit の規則
migrations/          データベースのマイグレーション
docs/                文書
```

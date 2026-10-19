# OpenShell の調査

chloe が自作している部分を [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell) に委ねられるかを調べ、委ねる範囲を判断する。
調査日は 2026-10-19 である。
対象は `main` の 55bf786（2026-10-17）で、リリースの系列は v0.1.5 のプレリリースにあたる。
以下で示すパスは、OpenShell のリポジトリの中のものである。

## 結論の要約

- ランナー、プロキシ、方針、エージェントからの依頼を OpenShell に置き換え、chloe を SSH と GitHub での認証の入口だけにする形は採れない。とくに、VM の中で Docker を動かせないこと、利用者や Organization や team ごとにシークレットを重ねられないこと、本文に入ったプレースホルダーが別の宛先へ送られるのを止めないことの三つが、chloe の要求と脅威モデルに合わない。
- OpenShell のクレートを chloe のプロキシに部品として取り込む形にも、得るものが少ない。方針の判定も認証情報の差し替えも、seccomp で通信を仲介する OpenShell の実行方式とスキーマに結び付いており、クレートは crates.io に出ていない。
- 当面は chloe を自作のまま進め、OpenShell の設計のうち、chloe に欠けている四つを取り入れる候補にする。実行ファイルごとの通信の規則、サブスクリプションの更新トークンを VM の外で持つこと、GraphQL や MCP の操作単位の規則、通信の許可の依頼に対する危険度の判定である。
- 見直すのは、OpenShell の VM のドライバーが入れ子のコンテナを扱えるようになり、複数の利用者の設計（RFC 0011）が固まったときである。

## OpenShell の概要

OpenShell は、AI エージェントを方針で縛ったサンドボックスで動かすための実行環境である。
Rust で書かれ、ライセンスは Apache-2.0 である。
構成は三つに分かれる。

- **ゲートウェイ**：サンドボックスの寿命、認証情報（provider）、方針、ワークスペースを扱うコントロールプレーン。OpenShell のワークスペースは、サンドボックスと provider をまとめて権限を分ける入れ物であり、chloe のワークスペース（VM ごとの永続するディスク）とは別のものである。gRPC の API（`proto/openshell.proto`）を持ち、状態を SQLite か PostgreSQL に置く。
- **supervisor**：サンドボックスの外側で方針、認証情報、プロキシを持つ信頼された側のプロセス。
- **サンドボックス**：エージェントが動く側。権限を落とし、Landlock と seccomp を掛けてからエージェントを起動する。

サンドボックスを動かすドライバーには、Docker、Podman、Kubernetes、microVM がある。
外への通信は、seccomp のユーザー通知で `connect` などのシステムコールを捕まえ、supervisor に渡して判定する。
プロキシの環境変数には頼らないので、Go や musl のバイナリの通信も捕まる。

## chloe の部品との対応

| chloe の部品 | OpenShell で対応するもの | 委ねられるか |
| --- | --- | --- |
| ランナー（Firecracker、guest-agent、ディスク） | microVM ドライバー（libkrun） | 委ねられない（VM と Docker の節） |
| `chloe-egress`（透過プロキシ、差し替え） | supervisor のネットワーク層 | 委ねられない（外への通信の節） |
| `chloe-policy`（宛先、メソッド、パスの規則） | ネットワークの方針（OPA/Rego） | 委ねられない（外への通信の節） |
| `chloe-secrets`（1Password、DB） | credential ドライバー | 委ねられない（認証情報の節） |
| `chloe-gateway` の SSH と GitHub での認証 | なし（OIDC と相互 TLS だけ） | 該当なし |
| `chloe-controller`（停止の予約、作業中の判定） | サンドボックスの停止と起動だけ | 委ねられない（VM と Docker の節） |
| `chloe-ask`（通信の許可の依頼、質問） | policy advisor（通信の許可の提案だけ） | 一部が重なる（依頼の節） |
| `chloe-audit` と HTTP のログ | OCSF のイベント | 形式だけ参考になる |
| `chloe-node`（複数のホスト） | Kubernetes の上での冗長化だけ | 委ねられない（運用の節） |

## VM と Docker

OpenShell の microVM ドライバーは、Firecracker ではなく libkrun を使う（`crates/openshell-driver-vm/build.rs`）。
VM には NIC がなく、ゲストの中の `openshell-sandbox` が vsock 越しにホストの supervisor とつながり、外への通信はすべて supervisor を通る。
ルートファイルシステムは、読み取り専用のイメージの上に、サンドボックスごとの上書き用のディスク（既定で 4096MiB）を重ねたものである。
上書き用のディスクは停止と起動をまたいで残る。
ただし、このドライバーの README は自身を「Experimental」と書いている。

chloe がこのドライバーを使えない理由は、VM の中で Docker を動かせないことである。
OpenShell のサンドボックスでは、エージェントは権限（capability）のない利用者で動き、seccomp が `mount`、`setns`、`pivot_root`、`unshare` を禁じている（`docs/security/best-practices.mdx`）。
VM のドライバーでもこの仕組みは同じなので、dockerd は起動できない。
dockerd を動かす方法は文書にもソースにも見当たらなかった。
chloe は「VM の中で Docker を使える」ことを要求に含めており、ここが崩れると利用者の使い方が変わる。

寿命の管理も chloe の要求に届かない。

- **止める時間**：最後の接続が切れてから止める仕組みも、次の接続で起動し直す仕組みもない。止まったサンドボックスへの接続は断られる。
- **作業中の判定**：エージェントが作業中かを見て停止を待つ仕組みはない。
- **VM の大きさ**：vCPU の数とメモリはゲートウェイ全体の設定で、サンドボックスごとの指定は今のところ VM の割り当てを変えない（`docs/how-it-works/sandboxes/overview.mdx`）。
- **ディスク**：ワークスペースと Docker のデータを分けたディスクとして持つ仕組みはなく、ルートの上書き用のディスクが一つあるだけである。

対話の入口には使える API がある。
`ExecSandboxInteractive` は、標準入力と端末の大きさの変更を送り、標準出力と終了を受け取る双方向のストリームである。
chloe のゲートウェイが SSH の PTY をこれにつなぐことはできる。
しかし、上の三点が欠けたままでは、入口をつないでもランナーを置き換えられない。

## 外への通信

OpenShell の通信の捕捉は、chloe の透過プロキシと同じく、ツールがプロキシの設定を読むかに左右されない。
違うのは、捕まえたあとの扱いである。

| 項目 | chloe | OpenShell |
| --- | --- | --- |
| 捕捉 | nftables で 80 番と 443 番をプロキシへ | seccomp で `connect` を捕まえて supervisor へ |
| 許可リストにない名前 | プロキシのアドレスに引け、403 で案内を返す | DNS が REFUSED を返す（`policy_dns/wire.rs`） |
| HTTP/2 | ALPN で選ぶ | 使わない（ALPN を `http/1.1` に固定、`l7/tls.rs`） |
| CA | 配置ごとに一つ、ゲストに入れる | サンドボックスの起動ごとに作り、環境変数で示す |
| 規則の単位 | 宛先、メソッド、パス | 宛先、ポート、パス、メソッドに加え、実行ファイル、GraphQL の操作、MCP のツール、WebSocket のテキストフレーム |
| 方針の入れ替え | 動いたまま | ネットワークの規則は動いたまま |

chloe が段階 19 で作った「許可リストにない名前もプロキシに引き、403 と案内を返す」挙動は、OpenShell では DNS の段階で断られる。
OpenShell はその代わりに、断った名前への接続を通信の許可の提案に変える。
エージェントに理由を伝える経路はあるが、chloe の案内とは形が違う。

規則の細かさでは、OpenShell が chloe を上回る点がある。
実行ファイルの実体のパスで規則を分け、初めて使ったときのハッシュで固定する（`docs/how-it-works/policies/network-rules.mdx`）。
同じ宛先でも、`gh` には許し、ほかのプロセスには許さないといった規則を書ける。
GraphQL の操作名とフィールドで絞る規則もあり、chloe の [未決事項](research-and-plan.md#未決事項) にある GraphQL の扱いへの一つの答えになっている。

## 認証情報

OpenShell も、サンドボックスにはプレースホルダー（`openshell:resolve:env:KEY`）だけを置き、supervisor が送る直前に本物へ差し替える。
差し替えられる場所は、ヘッダー、Basic 認証、クエリ、パス、オプトインした本文、WebSocket のテキストフレームで、AWS の SigV4 の署名し直しもある。
差し替える前には、ネットワークの方針と、認証情報ごとに登録した宛先の両方に合うかを確かめ、合わなければ 403 で断る。

しかし、本文に入ったプレースホルダーの扱いが、chloe の脅威モデルと逆である。
ヘッダーのプレースホルダーを登録していない宛先へ送ると、OpenShell も 403（`credential_endpoint_mismatch`）で断る。
本文では、差し替えを有効にしていない宛先に対して、有効なプレースホルダーを差し替えずにそのまま通す（`crates/openshell-core/src/secrets_body.rs`）。
chloe は、登録した宛先でないところで本文のプレースホルダーを見つけたら、要求を遮断して監査ログに残す。
本物の値は漏れないので、OpenShell の設計としては筋が通っている。
ただし chloe では、プレースホルダーが攻撃者のサーバーに届いたことを検知して記録すること自体を防御の一部にしているので、この違いは小さくない。
なお chloe の本文の検査にも、圧縮した本文、チャンクで送る本文、1MiB を超える本文、WebSocket の中身を見ないという制限がある（[README](../README.md#分かっている制限)）。

認証情報の範囲も合わない。
OpenShell の provider はワークスペースに属し、サンドボックスごとに付ける。
provider の定義（profile）には、ゲートウェイ全体とワークスペースの二段があり、ワークスペースのものが優先する。
しかし、利用者、Organization、team の層はなく、team はラベルにすぎない（`rfc/0011-multi-player-design/README.md`）。
保管先は自前の DB（エンベロープ暗号化）、Vault、Kubernetes の Secret で、1Password はない。
保管先のドライバーは Unix ソケット越しの gRPC（`proto/credential_driver.proto`）で外に足せるので、1Password だけなら補える。

サブスクリプションのログインの扱いは、二つの点で chloe と異なる。

- **Codex の ChatGPT ログイン**：ホストの `auth.json` から取り込み、更新トークンをゲートウェイが持つ。サンドボックスには、プレースホルダーで作った `auth.json` だけを置く（`examples/codex-app-server/README.md`）。chloe は、サブスクリプションの認証情報を VM に置き、持ち出される危険を受け入れている。OpenShell のやり方は、この危険を小さくする。
- **Claude のサブスクリプション**：文書は API キーを作るよう求めている（`docs/how-it-works/providers/overview.mdx`）。chloe は VM の中でのログインを許している。

## 利用者と認証

OpenShell のゲートウェイが受け付ける認証は、一つの発行者の OIDC、相互 TLS、認証プロキシの JWT である。
GitHub でのログインはなく、GitHub の Organization や team を所属に写す仕組みもない。
利用者のあいだの分離は OpenShell のワークスペースの単位で、同じワークスペースのメンバーは互いのサンドボックスに入れる。
この複数の利用者の設計（RFC 0011）はまだ draft である。

chloe のゲートウェイが OpenShell を裏で使うとすると、利用者ごとに OIDC のトークンを用意するか、chloe が管理者として振る舞い、利用者ごとの判定を chloe 側でするかのどちらかになる。
OpenShell には、ほかの利用者の代わりに操作する仕組みがない。
後者では、OpenShell の認可は使わずに chloe の認可を重ねることになり、委ねる意味が薄れる。

## エージェントからの依頼

OpenShell の policy advisor は、`chloe-ask` の通信の許可の依頼と同じ問題を解いている。
エージェントはサンドボックスの中の `http://policy.local` に方針の変更を提案し、長いポーリング（最大 300 秒、繰り返せる）で答えを待つ。
人は CLI、TUI、API で承認し、却下の理由はエージェントに返る。
prover が Z3 で提案を検査し、資格情報の届く範囲が広がる、検査を通らない経路ができるといった危険を指摘する。
指摘のない提案だけを自動で承認する設定もある。

一方で、chloe にあって OpenShell にないものもある。

- **質問**：通信の許可と関係のない質問を人に出す経路はない。
- **通知**：新しい提案を人に知らせる仕組みは、型の定義だけで、送る側の実装がない。人がポーリングする。
- **既定**：advisor は既定で無効である。

prover の危険度の分類は、chloe が依頼を人に見せるときに使える。
たとえば「この宛先を許すと、どのシークレットが届くようになるか」を依頼の画面に出せば、人が判断しやすくなる。

## 運用

- **API の安定性**：v1 の proto はパッチリリースのあいだで互換を保つ方針だが、その方針の RFC（0014）はまだ review である。0.1.0 では、その場での更新ができず、すべてのサンドボックスを作り直す変更があった。
- **配布**：deb、rpm、snap、コンテナ、Helm がある。Rust の SDK は crates.io になく、git の依存としてだけ使える。SDK の API は相互 TLS のクライアント証明書を扱わない（自分で作った tonic のチャネルを渡せば使える可能性はあるが、確かめていない）。
- **複数のホスト**：ゲートウェイは一つのドライバーを使い、VM のドライバーはゲートウェイと同じホストで動く。Kubernetes の外で複数のホストに配る手段は、自分でドライバーを書くことしかない。
- **監査**：OCSF の形式で、通信、HTTP、SSH、方針の変更を記録する。ゲートウェイへのログインは失敗だけを記録し、管理の操作の成功を記録するイベントはまだ出ていない。
- **テレメトリー**：既定で有効で、`OPENSHELL_TELEMETRY_ENABLED=false` で止める。

## 統合の形の比較

委ねる範囲の違いで、三つの形を比べた。

| 形 | 内容 | 判断 |
| --- | --- | --- |
| A | OpenShell のゲートウェイにランナー、プロキシ、方針、依頼を任せ、chloe は SSH と GitHub での認証の入口だけにする | 採らない。VM の中の Docker、シークレットの層、本文のプレースホルダーの遮断が欠け、停止の管理と複数のホストも足りない |
| B | OpenShell のクレートを chloe のプロキシに取り込む | 採らない。判定と差し替えが seccomp の仲介と OpenShell のスキーマに結び付き、クレートは版も crates.io への公開もない |
| C | chloe を自作のまま進め、OpenShell の設計を取り入れる | 採る |

A のうち、chloe の VM の中で OpenShell の Docker ドライバーを動かし、エージェントのプロセスに Landlock と seccomp を重ねる使い方も考えられる。
VM の分離の内側に、実行ファイルごとの通信の規則が加わる。
ただし、認証情報と方針が chloe と OpenShell の二か所に分かれ、どちらの規則で断られたかを利用者が追いにくくなる。
実行ファイルごとの規則が要るなら、chloe のプロキシで判定するほうが単純である。

## 取り入れる候補

C の形で取り入れる候補を、chloe への効果が大きい順に挙げる。

1. **サブスクリプションの更新トークンを VM の外に置く**：Codex の ChatGPT ログインの更新トークンをコントロールプレーンが持ち、VM には差し替え用のプレースホルダーだけを置く。chloe が受け入れている持ち出しの危険を小さくできる。ただし、これは「ツール自身が VM の中でログインし、認証情報をホームディレクトリに保存する」という [これまでの判断](research-and-plan.md#サブスクリプションのログイン) を変える。実現には、ログインを VM の外（`chloe` コマンドなど）で行うか、プロキシがトークンの発行の応答を捕まえて本物の値を保管し、VM にはプレースホルダーを返すかのどちらかが要る。
2. **依頼の危険度の表示**：通信の許可の依頼に、その宛先で差し替えられるシークレットと、増える権限を添えて人に見せる。
3. **GraphQL の操作単位の規則**：GitHub の GraphQL の変更を操作名で絞る。未決事項の一つに答えられる。
4. **実行ファイルごとの規則**：ゲストの中のどのプロセスが送ったかをプロキシが知る手段が要る。VM の外のプロキシからは見えないので、guest-agent との協調が前提になり、手間が大きい。

provider の定義（`providers/*.yaml`）は、Anthropic、OpenAI、GitHub などの宛先と差し替えの場所をまとめた一覧であり、chloe の規則の既定値を作るときの参考になる。

## 見直す条件

次のすべてが満たされたら、A の形をもう一度検討する。

- microVM のドライバーが Experimental でなくなり、VM の中で dockerd を動かせる。
- サンドボックスごとの vCPU の数とメモリを指定できる。
- RFC 0011 が accepted になり、GitHub の Organization と team を所属に写せる（OIDC の仲介で足りるかも含めて）。
- 本文の中のプレースホルダーを宛先で遮断する設定を持つ。

## 参考資料

- [OpenShell](https://github.com/NVIDIA/OpenShell)
- [OpenShell の文書](https://docs.nvidia.com/openshell/latest/index.html)
- [OpenShell のアーキテクチャ](https://docs.nvidia.com/openshell/latest/about/architecture)
- [OpenShell の policy advisor](https://docs.nvidia.com/openshell/latest/how-it-works/policies/advisor)

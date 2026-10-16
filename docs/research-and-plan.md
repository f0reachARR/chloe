# chloe 調査と計画

SSH で接続してきた利用者を GitHub アカウントで認証し、その利用者専用の microVM 上で Claude Code や Codex を起動するための仕組み（以下 **chloe**）について、調査結果と実装計画をまとめる。
調査日は 2026-10-16 である。

## 要求と前提

利用者の要求は次の五つに分けられる。

- **SSH 入口**：SSH 接続を受け、SSH 公開鍵または GitHub Device Flow で利用者を認証する。
- **microVM 実行環境**：認証した利用者ごとにワークスペースをマウントした microVM を起動し、その中で Claude Code や Codex を動かす。
- **通信の仲介**：VM からの HTTPS 通信を TLS 復号し、必要なクレデンシャルを経路上で埋め込む。
- **ファイアウォールとログ**：VM の外向き通信を制限し、接続と操作を記録する。
- **1Password 連携**：埋め込むクレデンシャルの保管元に 1Password を使う。

実装言語は Rust を第一候補とし、Go も許容する。

運用の前提は次のとおり決まっている。

- **ホスト**：KVM が使える物理サーバー。特権コンテナを許す Kubernetes 上で動かす場合もある。
- **契約形態**：Claude と OpenAI のどちらも、API キーとサブスクリプションの両方を扱う。Codex の ChatGPT ログインも対象に含む。
- **VM 内に置いてよい認証情報**：API キーは VM に入れない。サブスクリプションのログイン（Codex の ChatGPT ログインなど、API キーでないもの）で得た認証情報は VM 内に置いてよい。
- **シークレットの保管先**：1Password Connect を使える。加えて、chloe 自身のデータベースに置くシークレットもある。
- **ワークスペース**：セッションをまたいで残す。保管形式はディスクイメージでよく、ホストから中身を扱えることや他への再利用は求めない。持ち主は個人、team、Organization のいずれでもよく、一人が複数持てる。SSH 接続時に選ぶ。
- **ルートファイルシステム**：`apt install` の結果を残すかどうかをワークスペースごとに選べる。宣言からワークスペース専用のイメージも作れる。
- **GitHub の認証**：Device Flow には GitHub App を使い、そのトークンを個人ワークスペースの GitHub の認証情報として使う。共有ワークスペースでは同じ GitHub App のインストールトークンを使う。
- **Docker**：VM の中で Docker を使えるようにする。
- **ログ**：HTTP リクエストのメタデータを記録する。リクエストとレスポンスの本文は記録しない。

## 結論の要約

- SSH 入口は `russh` で自作する。ContainerSSH は GitHub 認証と監査ログを持つが、VM につなぐにはゲスト内に sshd と認証情報を置く必要がある。
- VMM は Firecracker を使い、ワークスペースは VM ごとのディスクイメージ（virtio-blk）で渡す。jailer、seccomp、帯域制限、スナップショットが揃っており、ホスト側でファイルシステムを解釈しない。ホストのディレクトリ共有（virtio-fs）や PCI パススルーが必要になった場合に備え、VMM は Cloud Hypervisor と差し替えられるようにしておく。
- VM の中で Docker を動かす。ゲストカーネルに必要な機能を入れ、Docker のデータは専用のディスクイメージに置く。コンテナの通信もランナーのプロキシを通す。
- TLS 復号は HAProxy ではなく、`rama` を土台にした透過型の MITM プロキシで行う。宛先ごとに復号するかそのまま中継するかを選び、chloe の CA は VM 内に公開する。
- API キーと GitHub トークンは VM に渡さない。VM にはホスト名に束縛したプレースホルダーだけを渡し、プロキシが送信先を確認してから本物に差し替える。
- サブスクリプションのログイン（Codex の ChatGPT ログインなど）は、ツール自身が VM 内でログインし、認証情報を永続化したホームディレクトリに保存する。プロキシは差し替えをせず、許可リストとログだけを適用する。
- ワークスペース一つにつき **ランナー**（VM とプロキシを一つのネットワーク namespace に収めた単位）を一つ起動する。物理サーバーではプロセス群として、Kubernetes では Pod として同じ構成を動かす。
- シークレットの保管先は差し替え可能にし、1Password Connect と chloe のデータベース（エンベロープ暗号化）の二つを実装する。

## 既存 OSS の調査

### 同じ発想の先行事例

「秘密情報を VM に入れず、ホスト側の MITM プロキシで差し替える」構成は、2025 年以降の AI エージェント向けサンドボックスで定番になりつつある。
代表的なものを挙げる。

| プロジェクト | 言語 | VMM | ワークスペース | 秘密情報の扱い | ライセンス |
| --- | --- | --- | --- | --- | --- |
| [Matchlock](https://github.com/jingkaihe/matchlock) | Go | Firecracker（Linux）、Virtualization.framework（macOS） | vsock 越しの FUSE | `NAME@host` 形式でホストに束縛、MITM で差し替え | MIT |
| [microsandbox](https://github.com/zerocore-ai/microsandbox) | Rust | libkrun | ボリューム（方式は未確認） | `allowed_host` に束縛、VM に入れない | Apache-2.0 |
| agent-vm | 不明 | libkrun | ホストパスをマウント | TLS 復号プロキシでプレースホルダーを差し替え | 不明 |
| Docker Sandboxes | Go | microVM | プロジェクトをマウント | ホスト側プロキシが認証ヘッダーを付与 | 製品 |

Matchlock が最も近い。
ただし README 上の位置づけは「Experimental」であり、ログや監査の機能は記載がない。
microsandbox は Rust で書かれ、SDK として組み込む想定の設計である（常駐デーモンを持たない）。
一覧は [awesome-agent-sandbox](https://github.com/fishman/awesome-agent-sandbox) が網羅的である。

README を確認した Matchlock と microsandbox は、いずれも「手元の 1 台で 1 人が使う」ことを想定している（他は一覧の説明を読んだ範囲での判断である）。
複数の利用者が SSH で入ってきて、利用者ごとに認証と秘密情報を切り替える構成は、調べた範囲では見当たらなかった。
chloe が自作する価値があるのは、この多利用者の部分である。

### SSH 入口の候補

- **[ContainerSSH](https://containerssh.io/)**（Go、CNCF Sandbox）：接続ごとにコンテナを起動する SSH サーバー。認証と設定を Webhook で外部に委ねられ、監査ログとセッション録画を持つ。OAuth2 認証は GitHub に対応し、`keyboard-interactive` でログイン URL とコードを表示する。`requireOrgMembership` や `enforceUsername`（SSH ユーザー名と GitHub ログイン名の一致を要求）もある。バックエンドは Docker、Podman、Kubernetes、SSH プロキシであり、microVM はない。
- **[sshpiper](https://github.com/tg123/sshpiper)**（Go）：ユーザー名や公開鍵で上流の sshd に振り分ける SSH リバースプロキシ。上流への認証には別の鍵（mapping key）が要る。
- **[russh](https://docs.rs/russh)**（Rust、0.63 系）：Tokio ベースの低レベル SSH サーバー／クライアント実装。`publickey`、`keyboard-interactive`、OpenSSH 証明書に対応する。

ContainerSSH を採る場合は、SSH プロキシバックエンドで VM 内の sshd に中継し、VM の起動を設定 Webhook で行う構成になる。
この構成では、ゲスト内で sshd を動かし、ホスト鍵と上流への認証情報をゲストに置くことになる。
後述のとおり chloe はゲストに認証情報を置かず、vsock 越しの専用エージェントで PTY を中継する設計なので、この点が決定的に合わない。
加えて、ContainerSSH の文書で Device Flow が明記されているのは OIDC プロバイダーの場合であり、GitHub ではリダイレクトサーバー方式になる可能性がある。
そのため本計画は `russh` による自作を既定とし、ContainerSSH は Go を選ぶ場合の代替とする。

### VMM の候補

- **Firecracker**：最小構成で起動が速く、データセンターでの多利用者運用の実績がある。virtio-fs を持たないため、ワークスペースはブロックデバイス（ext4 イメージなど）で渡すか、Matchlock のように vsock 越しの FUSE で渡すことになる。
- **Cloud Hypervisor**（Rust）：virtio-fs、vsock、tap による virtio-net に対応し、REST API で操作できる。virtiofsd（Rust）は別プロセスで動くので、namespace や seccomp で個別に閉じ込められる。
- **libkrun**（Rust、C API）：virtio-fs でホストのディレクトリをそのまま見せられ、起動が速い。ネットワークは TSI（VMM がゲストのソケットを代理する方式）か、passt や gvproxy につなぐ virtio-net を選ぶ。README は TSI について「VMM とゲストを一つのネットワーク文脈として扱い、制限は VMM に掛けよ」と注意している。virtio-fs についても、ゲストが共有ディレクトリ外に到達することへの保護はなく、ホスト側でマウントポイントを隔離するよう求めている。

ワークスペースをディスクイメージにしてよいので、virtio-fs を持たないという Firecracker の欠点は chloe では問題にならない。
そのうえで Firecracker を選ぶ理由は四つある。

- **ホスト側の攻撃面が小さい**：virtio-fs ではホストの virtiofsd がゲストからのファイル操作を解釈するので、共有ディレクトリの外に出られないようにする隔離をホスト側で作り込む必要がある（libkrun の README が警告している点である）。ブロックデバイスなら、ホストはイメージのバイト列を読み書きするだけで、ファイルシステムの解釈はゲストのカーネル内で完結する。
- **多利用者向けの機能が揃っている**：jailer（cgroup、namespace、chroot を掛けてから権限を落として VMM を起動する仕組み）、スレッドごとの seccomp、ディスクとネットワークの帯域制限が最初からある。
- **スナップショットの実績**：VM をメモリごと保存して復元でき、AWS Lambda の SnapStart の基盤として本番で使われている。切断中の VM の一時停止に使う（後述）。
- **先行事例**：Matchlock や E2B など、AI エージェント向けのサンドボックスで採用例が多い。

Cloud Hypervisor は、virtio-fs、PCI パススルー、デバイスのホットプラグが必要になった場合の代替とする。
どちらも Unix ソケット越しの REST API で操作するので、ランナーの中で VMM を差し替えられるように作る。
libkrun は単体利用には軽いが、TSI では VM ごとの通信制御を VMM プロセス単位の namespace で行う必要があり、制御点が分散するため採らない[^libkrun-net]。

[^libkrun-net]: libkrun でも virtio-net と passt を使えば tap 相当の制御点は作れる。

Kubernetes 上で microVM を扱う既製の手段として Kata Containers（RuntimeClass で Firecracker や Cloud Hypervisor を使う）もある。
ただし Kata ではゲスト内のエージェントとネットワークの構成が Kata の設計に固定され、vsock 越しの PTY 中継や tap ごとの nftables を chloe の都合で組めない。
そのため Kubernetes でも、特権 Pod の中で Firecracker を直接起動する。

### TLS 復号プロキシの候補

HAProxy にも `generate-certificates` と `ca-sign-file` による証明書の動的生成がある。
しかしこの機能は SNI に応じて待ち受け側の証明書を偽造するもので、任意の宛先への CONNECT を受けて上流に再暗号化する転送プロキシとしての使い方は想定されていない。
加えて、ヘッダーの差し替えを Lua で書くことになり、シークレットの保管先や監査ログと結びつけにくい。
そのため HAProxy は採らない。

chloe のプロキシに求める性質は次の四つである。

- **透過プロキシ**：VM からの 443 番の通信を nftables で受け、SNI から宛先を決める。
- **選択的な復号**：宛先ごとに、復号して差し替えるか、復号せずに中継するかを選ぶ。
- **動的な証明書発行**：復号する宛先の証明書を、chloe の CA でその場で発行する。
- **HTTP/2 と WebSocket**：復号した通信で HTTP/2 と WebSocket を扱える。

候補を比べると次のようになる。

| 候補 | 言語 | 透過プロキシ | 復号しない中継 | HTTP/2 | 書き換えの書き方 | 状態 |
| --- | --- | --- | --- | --- | --- | --- |
| [rama](https://github.com/plabayo/rama) | Rust | TPROXY の例、SNI プロキシの例あり | `SniRouter` で SNI ごとに振り分け | あり（ALPN で自動選択） | Rust のサービスとレイヤー | 0.4.0（2026-08）、MIT/Apache-2.0 |
| [hudsucker](https://docs.rs/hudsucker) | Rust | なし（明示プロキシのみ） | `should_intercept_connect`、`should_intercept_tls` | `http2` 機能フラグ | `HttpHandler` トレイト | 0.25.0、MIT/Apache-2.0 |
| mitmproxy | Python | `transparent` モード | `ignore_hosts` | あり | Python のアドオン | 成熟 |
| Squid（ssl-bump） | C++ | `intercept` | `peek` と `splice` | 限定的 | 設定ファイル、ICAP、eCAP | 成熟 |
| [g3proxy](https://github.com/bytedance/g3) | Rust | TPROXY、SNI プロキシ | あり | あり | 設定ファイル、ICAP | 保守モード（後継は vey-proxy） |

**rama** は、Rust でネットワークのクライアント、サーバー、プロキシを組むためのフレームワークである。
サンプルに、TPROXY による透過プロキシ（元の宛先の復元を含む）、`SniRouter` で関心のある宛先だけを復号し残りをそのまま中継する SNI プロキシ、BoringSSL でのその場の証明書発行（`ServerCertIssuerData`）、WebSocket と本文のデコードを含む MITM、ICAP、HAR の記録がある。
chloe の四つの要求は、どれもサンプルの組み合わせで満たせる。
MITM の推奨経路は BoringSSL（`rama-tls-boring`）であり、rustls のサンプルは「互換用」と位置づけられている。
BoringSSL を使うとビルドに cmake と C コンパイラが要る。

**hudsucker** は明示プロキシとしてはまとまっているが、透過プロキシの受け口がない。
自前で受け口を書き、SNI を読んでから hudsucker の処理に渡す必要がある。
**mitmproxy** と **Squid** はどちらも要求を満たすが、シークレットの取得や差し替えの規則を chloe の Rust のコードと結びつけるには、アドオンや ICAP サーバーを別に書くことになる。
**g3proxy** は機能は近いが、開発が保守モードに移っている。

以上から、chloe のプロキシは rama を土台にする。
rama は 0.x であり、版が上がると API が変わる可能性があるので、rama に依存する部分はプロキシの受け口と TLS の層に閉じ込め、差し替えの規則や許可リストの判定は chloe の型で書く。
PoC では mitmproxy の `transparent` モードで先に挙動を確かめ、MVP で rama に移す。

- **[httpjail](https://simonwillison.net/2025/Sep/19/httpjail/)**（Coder、Rust）：単一プロセスを専用プロキシに閉じ込め、JavaScript のルールで HTTPS を許可、拒否、記録する。ルール評価の設計が参考になる。

### Claude Code と Codex がプロキシ越しに動く条件

Claude Code は `HTTPS_PROXY` を読み、追加の CA を `NODE_EXTRA_CA_CERTS` で受け取る（[corporate proxy](https://code.claude.com/docs/en/corporate-proxy)）。
`NO_PROXY` には対応していない。
認証は優先順位つきで複数の手段を持つ（[authentication](https://code.claude.com/docs/en/authentication)）。
chloe に関係するのは次の三つである。

- `ANTHROPIC_API_KEY`：`X-Api-Key` ヘッダーで送られる。
- `ANTHROPIC_AUTH_TOKEN`：`Authorization: Bearer` で送られる。
- `CLAUDE_CODE_OAUTH_TOKEN`：`claude setup-token` で発行する有効期限 1 年のサブスクリプション用トークン。モデルへのリクエストだけに使える。

Codex は `CODEX_CA_CERTIFICATE`（未設定なら `SSL_CERT_FILE`）で CA を受け取り、ログイン、通常の HTTPS、WebSocket のすべてに適用する（[Codex auth](https://developers.openai.com/codex/auth)）。
認証は API キーか ChatGPT ログインである。
ChatGPT ログインではトークンを `~/.codex/auth.json` に保存し、期限前に自動で更新する。
OpenAI の「Sign in with ChatGPT」のトークン仕様では、アクセストークンの有効期間は 1 時間、リフレッシュトークンは 30 日で、**リフレッシュトークンは更新のたびに新しいものへ入れ替わる**（[token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference)）。
Codex がこの仕様にそのまま従っているかは未確認である（chloe の MVP はこの細部に依存しない。後述）。

### 1Password の接続方法

1Password の公式 SDK は Go、JavaScript、Python にあり、Rust にはない（[SDK docs](https://developer.1password.com/docs/sdks/load-secrets)）。
SDK はサービスアカウントのトークンで認証し、`op://` 参照を `Secrets().Resolve()` で解決する。
サービスアカウントにはレート制限とリクエスト数の上限があり、REST API はない。

**1Password Connect** は自前のインフラに置く REST API サーバーである（[Secrets Automation](https://developer.1password.com/docs/secrets-automation)）。
初回取得後はローカルから応答し、レート制限がない。
アクセストークンは用途ごとに発行でき、対象の Vault を絞れる。
Rust からは `reqwest` で直接呼べる[^corteq]。

[^corteq]: 非公式の Rust crate `corteq-onepassword` は公式 SDK のコアライブラリを FFI で呼ぶ。公式のものではないため採用しない。

## 脅威モデル

chloe が防ぎたいのは次の三つである。

- **乗っ取られたエージェントによる秘密情報の持ち出し**：プロンプトインジェクションを受けた Claude Code や Codex が、環境変数やファイルにある秘密情報を外部に送る。秘密情報を VM に入れないことと、外向き通信を許可リストで絞ることで防ぐ。
- **VM からの脱出**：ゲスト内で任意コードが実行されることを前提にし、KVM による分離で他の利用者とホストを守る。VMM はさらに jailer で namespace、cgroup、seccomp を掛けて閉じ込める。ゲストが書き換えたディスクイメージには、ホストのカーネルのファイルシステム処理を突く細工が入りうるので、ホストではワークスペースのイメージをマウントしない。
- **内部ネットワークへの横移動**：VM からホストの管理ポート、他の VM、社内ネットワーク（RFC1918 アドレス）、Kubernetes の API サーバーとクラスタ内サービスへの通信を既定で遮断する。

プレースホルダー方式には固有の落とし穴がある。
プロキシが「プレースホルダーを見つけたら差し替える」だけの実装だと、乗っ取られたエージェントがプレースホルダーを攻撃者のサーバー宛てに送ったとき、プロキシが本物の秘密情報を埋め込んで送ってしまう。
**差し替えは、プレースホルダーごとに登録した宛先ホストへの通信に限って行う。**
それ以外の宛先でプレースホルダーを検出した場合は、リクエストを遮断して監査ログに残す。

サブスクリプションの認証情報は VM 内に置くので、この保護の対象外である。
乗っ取られたエージェントがそれを読むことは防げず、許可リストに載ったホスト（GitHub など、利用者が書き込めるサービスを含む）へ送ることも通信の制限だけでは防げない。
この持ち出しの危険は、運用上受け入れると判断した。
被害は、その利用者のアカウントに限られる（ランナーは利用者ごとに分かれている）。
持ち出しが疑われるときは、利用者が提供元でログインを取り消せば止められる。

## 全体構成

chloe は、利用者全体を扱う **コントロールプレーン** と、ワークスペースごとに起動する **ランナー** に分かれる。

```text
 利用者 (ssh)
     │
     ▼
┌─────────────── コントロールプレーン ────────────────┐
│ chloe-gateway (russh)                               │
│   ├─ 認証: 公開鍵 → GitHub の登録鍵と照合 / Device Flow │
│   └─ PTY をランナーの guest-agent へ中継              │
│ chloe-controller                                    │
│   ├─ 利用者、ワークスペース、ランナーの状態管理        │
│   ├─ ランナーの起動と停止 (プロセス or Pod)           │
│   └─ ランナーへのシークレット配布 API                 │
│ chloe-secrets ── 1Password Connect / chloe DB        │
│ DB (SQLite → PostgreSQL)                            │
└─────────────────────────────────────────────────────┘
     │ ランナーごとの短命トークンで相互認証
     ▼
┌──────────── ランナー (ワークスペースごと) ────────────┐
│ ネットワーク namespace (物理サーバー) / Pod (K8s)     │
│                                                     │
│  firecracker (jailer) ── VM ── guest-agent (vsock)   │
│     │ virtio-blk         │ tap0 (172.29.0.2)         │
│     │ rootfs/ws/docker   ▼                          │
│     │            nftables: tap0 からは :3128 だけ許可 │
│     │                    │                          │
│     │            chloe-egress (rama)                 │
│     │              ├─ 宛先の許可リスト判定             │
│     │              ├─ プレースホルダーの差し替え       │
│     │              └─ 通信ログ                         │
│     ▼                    │                          │
│  ワークスペース (永続)     ▼                          │
└─────────────────────── 外部 API ─────────────────────┘
```

ランナーの中のプロキシは、そのランナーの VM からの通信しか受けない。
そのため、受けた通信がどの利用者とワークスペースのものかを送信元 IP などで判定する必要がない。
物理サーバーでも Kubernetes でも、ランナーの中身は同じであり、違うのは誰がランナーを起動するか（chloe-controller が直接プロセスを起動するか、Pod を作成するか）だけである。

ゲスト内では sshd を動かさない。
代わりに `chloe-guest-agent` が vsock で待ち受け、ゲートウェイからの PTY 割り当てやコマンド実行の要求を受ける。
ゲスト内に SSH の鍵や認証情報を置かずに済み、接続の記録もゲートウェイ側で一か所にまとまる。

## 主要な設計判断

### 認証と利用者の同一性

利用者の主体は GitHub アカウントとする。
SSH ユーザー名を GitHub のログイン名として扱い（`ssh alice@chloe.example`）、次の順で認証する。

1. `publickey`：提示された公開鍵を、GitHub API の `GET /users/{username}/keys` で得た登録鍵と照合する。結果は短時間キャッシュする。
2. 鍵が一致しなければ `keyboard-interactive` に進み、GitHub Device Flow のユーザーコードと URL を表示する。利用者がブラウザで承認したら、取得したトークンでログイン名を確認する。
3. どちらの経路でも、指定した GitHub Organization（必要ならチーム）への所属を確認する。

非公開の Organization 所属は、Organization のメンバー情報を読む権限のあるトークンがないと確認できない。
公開鍵の経路では利用者のトークンが手元にないので、所属確認には chloe の GitHub App のインストールトークンを使う。

Device Flow には **GitHub App** を使う（OAuth App は使わない）。
GitHub App の設定で Device Flow を有効にし、`offline_access` を要求して、期限つきのユーザートークンを受け取る。
ユーザートークンの権限は、アプリに与えた権限と利用者自身の権限の重なりで、アプリをインストールした Organization やリポジトリにしか届かない。
有効期限は 8 時間で、6 か月有効のリフレッシュトークンで更新する。
OAuth App のトークンは期限がなく、scope の単位が粗い（`repo` を与えると利用者の全リポジトリに書ける）ので、エージェントに使わせるトークンとしては GitHub App のほうが漏れたときの被害が小さい。

Device Flow で得たトークンは、そのまま個人ワークスペースの GitHub の認証情報として使う（次の「GitHub の認証情報とアクセス制御」）。
リフレッシュトークンは chloe のデータベースに暗号化して保存し、公開鍵でログインしたときもこれで GitHub トークンを得る。
SAML SSO を使う Organization では、トークンの SSO 承認が別に要る。

### ワークスペースの持ち主と選択

ワークスペースの持ち主は、個人、GitHub の team、GitHub の Organization のいずれかにする。
一人の利用者が複数のワークスペースを持ち、同時に動かせる。
入れるかどうかは、SSH の認証で確かめた GitHub ユーザーが持ち主本人か、その team や Organization に属しているかで決める。

SSH 接続時の選び方は二通り用意する。

- **ユーザー名で指定する**：`ssh alice+myorg/api@chloe.example` のように、`+` のあとにワークスペースを書く。スクリプトやエディタの接続設定から使う。
- **対話で選ぶ**：ワークスペースを書かなければ、認証のあとに選択メニューを出す。

ワークスペースの作成、一覧、削除は、ゲートウェイの管理コマンド（`ssh chloe.example ws create myorg/api` など）で行う。

共有ワークスペース（team や Organization のもの）には二つの制約がある。

- **個人の認証情報は入れない**：プロキシはどの VM からの通信かは分かるが、VM の中で誰のプロセスが送ったかは区別できない。そのため共有ワークスペースでは、チームの認証情報（チームの API キー、GitHub App のインストールトークン）だけを差し替える。個人の Device Flow トークンを差し替えるのは個人ワークスペースに限る。
- **メンバー同士は VM の中で隔離されない**：Docker と sudo が使えるので、メンバーは VM の中で root と同じことができる。共有ワークスペースの信頼の範囲は、そのワークスペースに入れる人全員である。誰がいつ接続したかはゲートウェイの監査ログに残る。HTTP のログはワークスペース単位になる。

### ワークスペースとランナーの寿命

ワークスペースはセッションをまたいで残す。
一つのワークスペースには同時に一つのランナーだけを起動し、同じワークスペースへの二つ目の SSH 接続は既存の VM に新しい PTY を開く。
ext4 などの通常のファイルシステムは、二つのカーネルから同時に書き込まれると壊れるので、ワークスペースのイメージを書き込み用に開く VM は常に一台に限る。

VM には三つのディスクを付ける。

- **ルートファイルシステム**：Claude Code、Codex、git、Docker などを入れた共通イメージ。全 VM に読み取り専用で付け、`/etc` や `/var` への書き込みはゲスト内の tmpfs に重ねる（overlayfs）。更新はイメージの差し替えで行う。
- **ワークスペースイメージ**：ワークスペースごとの ext4 イメージ。`/workspace`（利用者のリポジトリなど）と `/home/user`（`~/.claude`、`~/.codex`、シェルの設定など）を置く。
- **Docker イメージ**：ワークスペースごとの ext4 イメージ。`/var/lib/docker` に付ける（次節）。

イメージのサイズがそのまま容量の上限になるので、ファイルシステムの quota は要らない。
イメージは疎ファイルとして作り、使った分だけホストのディスクを消費させる。
ゲストでファイルを消したときにホスト側の領域を回収できるか（virtio-blk の discard）は PoC で確かめる。

物理サーバーではイメージを `/var/lib/chloe/workspaces/{github_login}/{workspace}/` に置く。
Kubernetes では、ワークスペースごとに PersistentVolumeClaim（ReadWriteOnce）を作る。
`volumeMode: Block` にすれば生のブロックデバイスをそのまま Firecracker に渡せ、ファイルシステムの PVC 上にイメージファイルを置く形でもよい。

最後の SSH 接続が切れてから一定時間（たとえば 30 分）経つと、ランナーをスナップショットにして止める。
スナップショットには VM のメモリと状態が入るので、再接続時に復元すれば、tmux の中で動かしていた Claude Code や Docker のコンテナも含めて元に戻る。
ただし、復元までのあいだに外部 API との TLS 接続やストリーミング中の応答は切れるので、ツールが再接続できるかは PoC で確かめる。
スナップショットもそれなりの容量（VM のメモリサイズ程度）を使うので、さらに長く（たとえば 7 日）接続がなければスナップショットを捨て、次回は通常の起動にする。
このときもワークスペースとホームディレクトリはイメージに残る。

### パッケージとツールチェーンの保持

ホームディレクトリ、`/workspace`、Docker のデータは常に残る。
rustup、mise、nvm、`~/.local` などホームの下に入れるツールチェーンはこれで足りる。
`apt install` や `/usr/local`、`/opt` に入れたものはルートファイルシステムに書かれるので、ルートの扱いによって残るかどうかが変わる。
ルートの扱いはワークスペースごとに次の二つから選ぶ。

- **使い捨て**：共通イメージに tmpfs を重ね、起動のたびに捨てる。mise などホームの下だけで完結するワークスペースに向く。共通イメージを更新しても食い違いが起きない。
- **保持**：共通イメージに重ねる層をワークスペースのイメージに置き、起動をまたいで残す。`apt install` がそのまま残る。共通イメージを更新すると、残した層の dpkg の管理情報が新しいイメージと食い違うことがあるので、更新を検出したら、残した層を捨てるか引き継ぐかを利用者に選ばせる。

どちらの扱いでも、ワークスペースに宣言したパッケージやセットアップから、そのワークスペース専用のルートファイルシステムを作れるようにする（共通イメージを土台にした Dockerfile か、パッケージの一覧）。
同じ環境を作り直せるので、チームや Organization のワークスペースに向く。
共通イメージを更新したら作り直す。

### VM の中の Docker

VM の中で Docker を動かすには、ゲストカーネルとルートファイルシステムに次のものを入れる。

- **カーネル**：cgroup v2、各種 namespace、overlayfs、veth とブリッジ、netfilter（nf_tables と Docker が使う iptables 互換の部分）、seccomp。Firecracker の推奨するゲストカーネルの設定は最小構成なので、chloe 用の設定を作る。
- **ユーザーランド**：dockerd、containerd、BuildKit、Docker Compose。利用者は `docker` グループに入れる。VM そのものが境界なので、VM 内で root 相当になることは許容する。

Docker のデータを専用のイメージに分けるのは、容量の上限をワークスペースと別に決められ、壊れたときや肥大化したときにワークスペースを残したまま作り直せるからである。

コンテナの通信は、VM の中で Docker のブリッジから NAT されて tap に出る。
ランナーのプロキシは透過型なので、コンテナの通信も設定なしでプロキシを通る（次節）。
dockerd 自身によるイメージの取得は、ゲストのシステムの信頼ストアに入れた chloe の CA で検証が通る。

コンテナの中には chloe の CA が入っていないので、そのままではコンテナ内から復号対象の宛先への HTTPS 通信が証明書の検証で失敗する。
これは TLS 復号を行う以上避けられない問題であり、CA 証明書の公開（`http://ca.chloe.internal/ca.pem` など）と、復号しない宛先の登録で対処する（どちらも「証明書の扱い」の節で述べる）。
Docker Hub やパッケージのミラーのような、差し替えの要らない宛先を「中継のみ」にしておけば、CA を入れていないコンテナでもイメージやパッケージの取得はそのまま動く。

ワークスペースの中で開発サーバーを動かすことも多いので、ゲートウェイは SSH のポート転送（`ssh -L`）を受け、guest-agent 経由で VM 内のポートにつなぐ。
VM のポートを外部に直接公開することはしない。

### Kubernetes でのランナー

Kubernetes では、ランナー一つを Pod 一つとして作る。
Pod は `/dev/kvm` を使い、Pod のネットワーク namespace の中に tap と nftables を作るため、特権（少なくとも `NET_ADMIN` と `/dev/kvm` へのアクセス）を要する[^kvm-device]。
VM から見える外向きの出口はランナー内のプロキシだけなので、Pod の NetworkPolicy ではさらに「プロキシからの外向き通信は外部の許可ホストだけ」「Pod への内向き通信はゲートウェイからだけ」と二重に絞る。

[^kvm-device]: `/dev/kvm` をコンテナに渡す方法は、特権 Pod にするか、KubeVirt などが提供するデバイスプラグインを使うかの二つがある。後者なら Pod 全体を特権にしなくて済むが、tap と nftables の操作に `NET_ADMIN` は依然として要る。

ゲートウェイからランナーの guest-agent への PTY 中継は、ランナー内の小さな中継プロセスが vsock を TCP（mTLS）に橋渡しして行う。
物理サーバーでは同じ中継を Unix ソケットで行う。

### 通信の捕捉方式

捕捉は透過プロキシを基本にする。
tap から外へ向かう 80 番と 443 番の通信を nftables でランナー内のプロキシに転送し、宛先は TLS の SNI（HTTP なら `Host`）から判定する。
透過プロキシなら、プロキシの設定を無視するプログラム（Docker のコンテナ内に多い）も同じ経路に乗る。
明示プロキシ（CONNECT 方式）の受け口も同じプロキシに残し、`HTTPS_PROXY` を設定したツールはそちらを使えるようにする。
それ以外のポートへの通信はすべて遮断するので、どちらの方式にも乗らない通信は素通りせずに失敗する。

透過プロキシでは、上流への接続先を SNI のホスト名をプロキシ自身が名前解決して決め、ゲストが接続しようとした IP アドレスは使わない。
元の IP アドレスに接続すると、SNI には許可されたホスト名を書き、実際には攻撃者のサーバーに接続する、という形で許可リストを迂回できるからである。
SNI のない TLS 接続と、SNI を暗号化する ECH（Encrypted Client Hello）を使う接続は、宛先を判定できないので遮断する。
QUIC（UDP の 443 番）も遮断し、HTTP/3 を使うクライアントを TCP に戻させる。

### 証明書の扱い

復号する宛先の証明書は、プロキシが chloe の CA でその場で発行し、ホスト名ごとにキャッシュする。
CA はワークスペースごとに作る。
CA の秘密鍵はホスト側のデータベースに暗号化して置き、プロキシのメモリにだけ読み込む。
ワークスペースごとに分けるのは、一つのランナーのプロキシから CA の秘密鍵が漏れても、他のワークスペースの通信を復号できないようにするためである。

CA 証明書（公開鍵の側）は VM 内に公開する。

- **ファイル**：guest-agent が起動時に `/etc/chloe/ca.pem` に置き、ゲストのシステムの信頼ストアにも加える。`SSL_CERT_FILE`、`NODE_EXTRA_CA_CERTS`、`CODEX_CA_CERTIFICATE`、`REQUESTS_CA_BUNDLE` などの環境変数もこのファイルを指す。
- **HTTP**：プロキシが特定のホスト名（たとえば `http://ca.chloe.internal/ca.pem`）への要求に CA 証明書を返す[^mitm-it]。コンテナや Dockerfile の中からは `curl` でこれを取得して信頼ストアに加えられる。

[^mitm-it]: mitmproxy が `mitm.it` で CA 証明書を配布しているのと同じ方式である。このホスト名は外部に存在しないので、プロキシが応答しなければ誰にも届かない。

復号しない宛先は、管理者が許可リストに「中継のみ」として登録する。
証明書を固定（ピン留め）していて chloe の CA を受け付けないクライアントの宛先や、CA を入れていないコンテナから使う宛先（パッケージのミラーなど）が対象になる。
これらの宛先では差し替えができず、暗号化された中の `Host` も確かめられないので、同じ CDN 上の別のサイトへ抜けられる余地が残る。
そのため、利用者が自分でこの扱いを追加することはできないようにする。

TLS 復号の対象外になる通信も整理しておく。

- **git over SSH**：HTTP ではないので復号も差し替えもできない。ゲストの git 設定で `url."https://github.com/".insteadOf` を指定して HTTPS に寄せ、22 番ポートは遮断する。
- **DNS**：明示プロキシでは宛先の名前解決をプロキシが行うので、ゲストに外部 DNS は要らない。名前解決を必要とするツールのために、許可リストに沿って応答する DNS をランナー内に置くかは PoC で判断する。
- **HTTP/2**：Codex の HTTP クライアントが h2 を ALPN で要求する可能性がある。MITM 側で h2 を扱うか、http/1.1 だけを提示するかを PoC で確かめる。

### クレデンシャルの差し替え

差し替えの規則は、ランナー起動時にコントロールプレーンからプロキシへ渡す。
一つの規則は「プレースホルダー、宛先ホスト、差し替える場所（ヘッダー名と形式）、シークレットの参照」の組である。

```toml
[[inject]]
name        = "anthropic"
placeholder = "sk-ant-oat01-chloe-<ランナーごとの乱数>"
hosts       = ["api.anthropic.com"]
header      = "authorization"
format      = "Bearer {secret}"
source      = "op://chloe/{github_login}/anthropic_oauth_token"

[[inject]]
name        = "github"
placeholder = "ghp_chloe<ランナーごとの乱数>"
hosts       = ["github.com", "api.github.com"]
header      = "authorization"
format      = "basic-password"   # git の Basic 認証を復号してパスワード部を差し替える
source      = "chloe://{github_login}/github_user_token"
```

プレースホルダーはワークスペースの作成時に一度だけ乱数で作り、データベースに置く。
ランナーは起動時にそれを設定用のドライブで VM に渡し、guest-agent が起動するすべてのプロセスの環境変数に入れる。
起動ごとに作り直さないのは、`codex login --with-api-key` のように、ツールが API キー（ここではプレースホルダー）を永続化したホームディレクトリに書くことがあり、起動ごとに変えると古い値が残って認証エラーになるからである。
プレースホルダーは宛先に束縛されているので、漏れても害がない。
作り直したいときは `chloe secret rotate` で作り直す（未実装）。
Claude Code などがトークンの形式を手元で検査している可能性があるので、プレースホルダーは本物と同じ接頭辞と長さにそろえる（PoC で確認する）。

### API キーと GitHub トークン

API キーと GitHub トークンは、前節の規則でヘッダーを差し替える。

- **Claude（API キー）**：`ANTHROPIC_API_KEY` にプレースホルダーを渡し、`api.anthropic.com` 宛ての `X-Api-Key` を差し替える。
- **OpenAI（API キー）**：`OPENAI_API_KEY` にプレースホルダーを渡し、OpenAI の API ホスト宛ての `Authorization` を差し替える。
- **GitHub**：git の認証ヘルパーがプレースホルダーを返すようにし、プロキシが Basic 認証を復号してパスワード部を差し替える。`gh` CLI には `GH_TOKEN` としてプレースホルダーを渡す。

### GitHub の認証情報とアクセス制御

GitHub の認証情報は、一つの GitHub App から二種類を作る。

- **個人ワークスペース**：Device Flow で得たユーザートークンを使う。操作は利用者本人として記録される。
- **共有ワークスペース**：chloe が GitHub App の秘密鍵でインストールトークンを発行して使う。操作はアプリのボット（`アプリ名[bot]`）として記録され、誰が操作したかはゲートウェイの監査ログで追う。利用者に結びつく API（`/user` や通知など）は使えない。

GitHub App の秘密鍵は 1Password か chloe のデータベースに置き、トークンの発行と更新はホスト側で行う。
VM に入るのはプレースホルダーだけである。

GitHub への操作の範囲は、次の層を重ねて絞る。

1. **GitHub App の設定**：アプリの権限（contents、pull requests、issues、workflows など）と、インストール先のリポジトリの選択。GitHub が強制する、いちばん外側の枠である。
2. **トークンの発行時の絞り込み**：インストールトークンは、発行時にリポジトリ（最大 500 件）と権限をアプリの範囲内で絞れ、有効期限は 1 時間である。ワークスペースごとに、そのワークスペースで扱うリポジトリと権限だけを持つトークンを発行する。ユーザートークンは権限を絞れないが、トークンを受け取るときに一つのリポジトリに限定することはできる。
3. **プロキシの規則**：プロキシは GitHub への通信を復号しているので、宛先のパスと本文で判定できる。
   - REST API（`api.github.com`）は、メソッドとパス（`/repos/{owner}/{repo}/...`）でリポジトリと操作を絞れる。たとえば削除系のメソッドや管理系のエンドポイントを拒否できる。
   - git（`github.com`）は、パスでリポジトリを、`git-upload-pack`（取得）と `git-receive-pack`（push）で操作を区別できる。push の本文の先頭にある参照の更新命令（pkt-line 形式）を読めば、`main` への push やブランチの削除を拒否できる。
   - GraphQL（`api.github.com/graphql`）は、一つのエンドポイントに問い合わせ文が入るので、対象と操作を正確に判定するのは難しい。絞るとしても、参照（query）は通し、変更（mutation）は操作名の許可リストで絞る程度にとどまる。`gh` CLI は pull request の作成などに GraphQL を使うので、許可リストには `gh` が使う操作を入れる必要がある。当面は GraphQL を判定せずに通し、トークンの権限だけで絞る。
4. **GitHub 側の保護**：ブランチ保護やルールセットは、上の層と関係なく GitHub が強制する。保護したいブランチはここでも守る。

共有ワークスペースのインストールトークンに与えるリポジトリと権限は、Organization の管理者が決める。

3 のうち、メソッドとパスの規則は実装した（ゲートウェイの設定の `[[policy.rules]]`、全ワークスペースに共通）。
git の push の本文を読んでブランチ単位で拒否する規則と、ワークスペースごとの規則はまだない。

プロキシの規則は 1 と 2 で表せない細かい制限（ブランチ単位の push の制限、ユーザートークンのリポジトリの限定）を補うものとして使う。
push の本文はパックファイルを含んで大きくなるので、参照の更新命令は本文全体を読み込まずに先頭だけを読んで判定する。

### サブスクリプションのログイン

サブスクリプションのログインで得た認証情報は、VM 内に置く。
Codex の ChatGPT ログインは、ゲスト内の `auth.json` にあるリフレッシュトークンで Codex 自身がトークンを更新し、更新のたびにリフレッシュトークンが入れ替わる。
プレースホルダーを置くと、この更新要求をプロキシが代行しなければならず、OpenAI の認証通信の細部に依存する実装になる。
VM 内に置いてよいと決めたので、この代行は作らない。

- **Codex（ChatGPT ログイン）**：利用者は初回に VM 内で `codex login --device-auth` を実行する。`~/.codex/auth.json` は永続化したホームディレクトリに保存されるので、以後のセッションでもログイン状態が続く。
- **Claude Code（サブスクリプション）**：VM 内で `/login` を実行すれば、`~/.claude/.credentials.json` に保存される。

Claude Code では、利用者が手元で `claude setup-token` を実行して発行した 1 年トークンをシークレットとして登録し、`CLAUDE_CODE_OAUTH_TOKEN` にプレースホルダーを渡す方式も選べる。
このトークンは更新されない静的な値なので、API キーと同じ差し替えの規則に乗る。
VM 内に置かずに済む方式があるものはそちらを既定にし、`/login` は利用者が選んだ場合に使う。

どちらの場合も、プロキシは認証のヘッダーに触れず、許可リストの判定と HTTP ログの記録だけを行う。
Codex の ID トークンの扱いなど、ツール側の認証の細部に chloe が依存しないことが、この方式の利点である。

将来、サブスクリプションの認証情報も VM から外したくなった場合は、プロキシがトークンの発行応答を横取りして本物をデータベースに保存し、ゲストにはプレースホルダーを返して、更新要求も代行する方式がとれる[^token-broker]。

[^token-broker]: Codex のソース（2026-10-16 時点）では、`auth.json` の ID トークンとアクセストークンを JWT として署名検証なしでデコードし、ID トークンからメールアドレス、プラン、アカウント ID を、アクセストークンから `exp` を読む。更新は `exp` の 5 分前、最終更新から 8 日経過、401 応答のいずれかで `https://auth.openai.com/oauth/token` に JSON で要求し、この URL は環境変数 `CODEX_REFRESH_TOKEN_URL_OVERRIDE` で差し替えられる。Claude Code（2.1.301）は `~/.claude/.credentials.json` に `accessToken`、`refreshToken`、`expiresAt` などを保存し、`https://platform.claude.com/v1/oauth/token` で更新する。どちらも、ゲストには遠い期限を持つプレースホルダーを渡し、本物のアクセストークンの更新はプロキシが自分で行えば、ツール側からの更新要求はほぼ起きない。リフレッシュトークンは更新のたびに入れ替わるので、同じ利用者の複数のランナーからの更新は一本化する必要がある。

### シークレットの保管先

シークレットの保管先は差し替え可能な抽象（`SecretStore`）にし、参照の書き方で保管先を選ぶ。

- `op://{vault}/{item}/{field}`：1Password Connect から読む。利用者や管理者が 1Password で管理する静的なシークレット（API キー、`setup-token` で発行したトークンなど）に使う。
- `chloe://{github_login}/{name}`：chloe のデータベースから読む。chloe が自動で書き換えるシークレット（GitHub のリフレッシュトークンなど）と、1Password を使わない利用者のシークレットに使う。

自動で書き換えるトークンを chloe のデータベースに置くのは、数時間ごとの書き込みを 1Password に流すと、Vault の履歴が更新で埋まり、Connect の可用性が chloe の動作に直結するからである。
サブスクリプションの認証情報は VM のホームディレクトリにあるので、どちらの保管先にも入らない。

データベースのシークレットはエンベロープ暗号化で保存する。
シークレットごとにデータ鍵を作って AES-256-GCM で暗号化し、データ鍵はマスター鍵で暗号化して一緒に保存する。
GCM の追加認証データに利用者名とシークレット名を入れ、暗号文を別の利用者の行へ移し替えても復号できないようにする。
マスター鍵は物理サーバーではファイル（起動時に 1Password から取得してもよい）、Kubernetes では Secret か外部の KMS から読む。

1Password の Vault は、利用者ごとに分けるか共有の Vault にアイテムを並べるかを選べるようにし、Connect のトークンには chloe が読む Vault だけを許可する。
取得した値はプロキシのメモリ上にだけ置き、TTL つきでキャッシュする。
ディスクやログには書かず、型で包んで解放時にゼロ埋めする（`secrecy` と `zeroize` を使う）。

利用者が自分のシークレットを登録する手段として、ゲートウェイに管理用のコマンドを用意する（`ssh alice@chloe.example chloe secret set anthropic`）。
入力はゲストを経由せずゲートウェイが直接受けるので、VM 内のプロセスには見えない。

### ログと監査

ログは二種類に分ける。

- **監査イベント**：ログイン成功と失敗、ランナーの起動と停止、PTY の開閉、シークレットの取得と更新（値は書かず参照名だけ）、遮断した通信。
- **HTTP ログ**：時刻、利用者、ワークスペース、宛先ホスト、メソッド、パス、ステータス、送受信バイト数、所要時間、許可と遮断の判定、適用した差し替え規則の名前。

リクエストとレスポンスの本文、およびヘッダーの値は記録しない。
パスのクエリ文字列にトークンを載せる API もあるので、クエリはキー名だけを残して値を伏せる。
SSH の PTY 出力の録画も、利用者のコードがそのまま含まれるので既定では行わない。

出力は JSON Lines とし、Loki や OpenTelemetry Collector などへの転送は後段に任せる。

## 実装言語とクレート構成

Rust で書く。
SSH（`russh`）、TLS 復号と透過プロキシ（`rama`、BoringSSL）、VMM（Firecracker の REST API）、1Password Connect と GitHub API（`reqwest`）、データベース（`sqlx`）、Kubernetes（`kube`）、nftables（`nft` コマンドの JSON 入力、または `rustables`）のすべてに Rust の部品がある。
Go を選ぶ理由が生じるのは、ContainerSSH を入口に使う場合か、Connect を置かずに 1Password のサービスアカウントと公式 Go SDK を使いたい場合であり、今回の前提ではどちらにも当たらない。

```text
crates/
  chloe-gateway      SSH 入口、認証、PTY 中継、管理コマンド
  chloe-controller   利用者、ワークスペース、ランナーの管理、シークレット配布 API
  chloe-runner       ランナー内の起動処理 (firecracker と jailer、ディスクイメージ、tap、nftables、スナップショット)
  chloe-egress       MITM プロキシ、許可リスト、差し替え、HTTP ログ
  chloe-secrets      SecretStore と 1Password Connect、DB の実装
  chloe-policy       利用者とプロファイルごとの許可リストと差し替え規則
  chloe-audit        監査イベントの型と出力
  chloe-guest-agent  ゲスト内常駐、vsock 越しの PTY と exec
images/
  guest/             ゲストのルートファイルシステムとカーネルの構築
deploy/
  systemd/           物理サーバー向け
  kubernetes/        Kubernetes 向けマニフェスト
```

コントロールプレーン（gateway、controller、secrets）は一つのプロセスにまとめてよい。
ランナーはワークスペースごとに別プロセス群とし、root 相当の権限はランナーの起動処理だけに持たせる。
外部に露出する SSH 入口が乗っ取られても、他人のランナーやシークレットに直接触れないようにするためである[^privsep]。

[^privsep]: コントロールプレーンはシークレット配布 API を持つので、これを一つのプロセスにまとめる段階では、ゲートウェイの乗っ取りはシークレットの漏えいにつながる。強化の段階でゲートウェイを別プロセスに分ける。

## 段階的な計画

### PoC：一人の利用者で通信の差し替えが成り立つか

手作業で組み、設計の前提を確かめる段階である。

- Docker を動かせる設定のゲストカーネルを作り、Firecracker でゲストを起動する。ルートファイルシステム、ワークスペース、Docker の三つのイメージを付ける。
- ネットワーク namespace の中に tap と nftables を作り、プロキシ以外への通信を遮断する。
- mitmproxy のアドオンでホスト束縛の差し替えを書く。
- ゲスト内で Claude Code と Codex を起動し、プレースホルダーだけで会話できることを確かめる。

確認する項目は次のとおりである。

- Claude Code がプレースホルダー形式の `CLAUDE_CODE_OAUTH_TOKEN` と `ANTHROPIC_API_KEY` を受け付けるか。
- Codex の `codex login --device-auth` と Claude Code の `/login` が、プロキシ越しの VM 内で完了するか。認証に必要なホストを許可リストに加える。
- ストリーミング応答（SSE）と WebSocket がプロキシ越しに途切れないか。
- Codex が h2 を要求するか、要求する場合に mitmproxy と rama で扱えるか。
- rama の SNI プロキシの例と TPROXY の例を組み合わせ、選択的な復号、その場の証明書発行、ヘッダーの差し替えが一つのプロセスで動くか。
- 許可リストに必要なホスト（`api.anthropic.com`、`statsig.anthropic.com`、`sentry.io`、OpenAI の各ホスト、GitHub、npm などのパッケージレジストリ）の洗い出し。
- 許可外のホストにプレースホルダーを送ったときに遮断されること。
- ツール自身の内部サンドボックスがゲスト内で動くか。Codex は Landlock と seccomp を、Claude Code のサンドボックスは bubblewrap とユーザー namespace を使う。最小構成のゲストカーネルではこれらが欠けて起動しない可能性がある。カーネルに Landlock とユーザー namespace を入れるか、VM を境界とみなして内部サンドボックスを無効にするかを決める。
- VM 内の Docker で、イメージの取得、`docker build`、`docker compose up` がプロキシ越しに動くか。透過プロキシで、プロキシの設定を無視するコンテナの通信を捕捉できるか。
- スナップショットからの復元後に、Claude Code、Codex、Docker のコンテナが動き続けるか。復元にかかる時間とスナップショットの容量。
- virtio-blk の discard でホスト側の領域を回収できるか。npm install や git の操作など、小さなファイルを大量に扱う処理の速度。
- 特権 Pod の中で同じ構成が動くか（`/dev/kvm`、jailer、tap、nftables）。

終了条件は、上記の確認項目の結果が記録され、MVP の設計に反映されていることである。

### MVP：複数の利用者が物理サーバーで使える

- `chloe-gateway`：公開鍵と Device Flow の認証、Organization 所属の確認、PTY 中継、シークレット登録コマンド。
- `chloe-controller`：ワークスペースとランナーの管理、接続がなくなってからの自動停止。
- `chloe-runner`：ネットワーク namespace、tap、nftables、jailer つきの Firecracker の起動、ディスクイメージの作成、スナップショットによる停止と復元。
- `chloe-egress`：rama による透過プロキシと明示プロキシの受け口、選択的な復号、ワークスペースごとの CA による証明書発行、差し替え、許可リスト、CA 証明書の公開。
- ゲストイメージ：Docker の動くカーネル、Claude Code、Codex、Docker、プロキシと CA の設定。
- SSH のポート転送。
- `chloe-secrets`：1Password Connect とデータベース（SQLite、エンベロープ暗号化）。
- 監査イベントと HTTP ログの JSON Lines 出力。

終了条件は、二人の利用者が同時に接続し、それぞれのシークレットで Claude Code と Codex が動き、一方のプレースホルダーを他方の VM から使えないことを自動テストで確かめられることである。
切断して再接続したとき、ワークスペースとログイン状態が残っていることも確かめる。

### Kubernetes 対応

- ランナーを Pod として作る実装を `chloe-controller` に加える。
- ワークスペースごとの PersistentVolumeClaim と、NetworkPolicy による二重の制限。
- データベースを PostgreSQL に切り替え、マスター鍵を Kubernetes Secret か KMS から読む。

終了条件は、MVP と同じ自動テストが Kubernetes 上でも通ることである。

### 強化：運用に耐える

- CPU とメモリの上限、ディスクとネットワークの帯域制限（Firecracker の rate limiter）。
- RFC1918 とリンクローカル（メタデータサービスを含む）への遮断を既定にする。
- CONNECT の宛先、TLS の SNI、HTTP の `Host` の一致を要求し、IP アドレス直指定の CONNECT を拒否する。許可リストは CONNECT の宛先で判定するので、これらが食い違うとドメインフロンティングで許可リストを迂回できる。
- ゲートウェイとシークレット配布 API を別プロセスに分ける。
- 起動待ちを減らすための VM の事前起動プールやスナップショット。
- 許可リストの実行時変更と、遮断された宛先の許可申請。

## 未決事項

前提が決まったことで、残る判断は次の点に絞られた。

- **Kubernetes のストレージ**：ワークスペースの PersistentVolume をノードのローカルディスクにするか、ネットワークストレージにするか。ローカルディスクではランナーが特定のノードに固定される。
- **ランナーの停止までの時間**：切断後どれだけ VM を動かし続けてからスナップショットにするか、スナップショットをどれだけ残すか。スナップショットで状態は戻せるが、止まっているあいだは Claude Code の作業も進まない。
- **容量の既定値**：ワークスペースと Docker のイメージの大きさ、VM のメモリと CPU の既定値と上限。
- **利用者による許可リストの変更**：利用者が自分のワークスペースの許可ホストを追加できるようにするか、管理者だけが変えられるようにするか。
- **GraphQL の扱い**：変更（mutation）を操作名の許可リストで絞るかどうか。当面は判定せずに通し、トークンの権限だけで絞る。

## 参考資料

- [Matchlock](https://github.com/jingkaihe/matchlock)
- [microsandbox](https://github.com/zerocore-ai/microsandbox)
- [awesome-agent-sandbox](https://github.com/fishman/awesome-agent-sandbox)
- [Why agent sandboxes are converging on libkrun, not Firecracker](https://iii.dev/blog/why-agent-sandboxes-are-converging-on-libkrun-not-firecracker/)
- [libkrun](https://github.com/containers/libkrun)
- [Firecracker](https://github.com/firecracker-microvm/firecracker)
- [ContainerSSH OAuth2 認証](https://containerssh.io/v0.5/reference/auth-oauth2/)
- [sshpiper](https://github.com/tg123/sshpiper)
- [russh](https://docs.rs/russh)
- [hudsucker](https://docs.rs/hudsucker)
- [rama](https://github.com/plabayo/rama)
- [g3](https://github.com/bytedance/g3)
- [httpjail の紹介（Simon Willison）](https://simonwillison.net/2025/Sep/19/httpjail/)
- [Claude Code: corporate proxy](https://code.claude.com/docs/en/corporate-proxy)
- [Claude Code: authentication](https://code.claude.com/docs/en/authentication)
- [Codex: authentication](https://developers.openai.com/codex/auth)
- [Sign in with ChatGPT: token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference)
- [1Password SDKs: load secrets](https://developer.1password.com/docs/sdks/load-secrets)
- [1Password Secrets Automation](https://developer.1password.com/docs/secrets-automation)
- [HAProxy の証明書生成機能のパッチ](https://git01.mediatek.com/plugins/gitiles/haproxy/+/31af49d62bb398f43d0e955ebf9a7157c38e2d56)

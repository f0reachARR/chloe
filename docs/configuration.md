# 設定

設定のファイルは TOML で書く。
知らない項目があると、読み込みに失敗する（書き間違いに気付けるように）。
相対パスは、プロセスを起動したディレクトリから解決する。
systemd で動かすときは、すべて絶対パスで書く。

| ファイル | 読むもの | 例 |
| --- | --- | --- |
| ゲートウェイの設定 | `chloe-gateway --config` | `deploy/gateway.toml`（開発用は `images/guest/dev/gateway.example.toml`） |
| `chloe-node` の設定 | `chloe-node run --config` | `deploy/node.toml` |
| GitHub App の設定 | ゲートウェイの設定の `github_app` | [GitHub App の準備](github-app.md) |
| Organization と team の設定 | `chloe policy set`、管理 API | 下の「Organization と team の設定」 |

## ゲートウェイ

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `listen` | （必須） | SSH の待ち受け（`0.0.0.0:22` など） |
| `host_key` | （必須） | SSH のホスト鍵（OpenSSH の形式）。なければ作る |
| `database` | （必須） | SQLite のデータベース。なければ作る |
| `master_key` | （必須） | シークレットを暗号化する鍵。なければ作る |
| `audit_log` | なし | 監査ログ（JSON Lines、権限 600）を追記するファイル |
| `github_url` | `https://github.com` | 公開鍵を読む先（`{github_url}/{login}.keys`） |
| `github_app` | なし | GitHub App の設定のパス |
| `allowed_users` | `[]` | ログインを許す GitHub のログイン名。GitHub App の Organization のメンバーに加えて許す |
| `admins` | `[]` | サーバーの運用者。どの Organization と team の設定とシークレットも変えられる |
| `memberships` | `{}` | 共有ワークスペースに入れる利用者を、GitHub の所属に加えて書く（開発用） |
| `idle_timeout_secs` | `1800` | 最後の接続が切れてから VM を止めるまでの秒数 |
| `activity_poll_secs` | `60` | エージェントが作業中のとき、作業中かを尋ね直す間隔の秒数 |
| `max_busy_wait_secs` | `43200` | エージェントが作業中でも、この秒数が経ったら止める |
| `reconcile_interval_secs` | `60` | 動いているランナーと記録を突き合わせる間隔の秒数 |
| `[policy]` | 空 | すべてのワークスペースに共通の許可リストと規則 |
| `[runner]` | — | 同じホストの Docker にランナーを置く。`[cluster]` とどちらか一方を書く |
| `[cluster]` | — | 複数のホスト（`chloe-node`）にランナーを置く |
| `[api]` | なし | HTTP(S) の管理 API |

`memberships` は、持ち主ごとにログイン名を並べる。

```toml
[memberships]
"org:acme" = ["alice"]
"team:acme/dev" = ["alice", "bob"]
```

### `[policy]`

すべてのワークスペースに共通の、VM から外への通信の決まりである。

| 項目 | 内容 |
| --- | --- |
| `allow` | つないでよい宛先。プロキシで復号し、規則と差し替えを適用する |
| `passthrough` | 復号せずに中継だけする宛先（証明書を固定するクライアント向け）。規則も差し替えも効かない |
| `rules` | 宛先、メソッド、パスの規則（下を参照） |

宛先は名前で書く。
`*.example.com` は example.com のサブドメインに一致し、example.com そのものには一致しない。
VM の DNS は、`allow` と `passthrough` にある名前だけを引ける（`*.example.com` は、DNS では example.com そのものも引ける）。

シークレットの差し替え先（`api.anthropic.com` など）は、そのシークレットを登録したワークスペースでだけ、自動で許可リストに足す。

規則は上から順に評価し、最初に一致した規則で、許すか拒否するかを決める。
どの規則にも一致しなければ、許可リストだけで決める。

```toml
[[policy.rules]]
name = "github-org-only"          # ログに残す名前 (省略可)
hosts = ["api.github.com"]
methods = ["GET", "POST"]         # 省略するとすべてのメソッド
paths = ["/repos/acme/**", "/user"]
action = "allow"                  # allow か deny

[[policy.rules]]
hosts = ["api.github.com"]
action = "deny"
```

パスはグロブで書く。
`*` は `/` を含まない文字列に、一つの階層まるごとの `**` は 0 個以上の階層に一致する。
照合の前にパスを正規化し（パーセントエンコードを戻し、`//`、`.`、`..` を解決する）、大文字と小文字を区別しない。
クエリは照合に含めない。

### `[runner]`

同じホストの Docker で、ワークスペースごとにランナーのコンテナを起動する。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `guest_dir` | （必須） | カーネル（`kernel/vmlinux`）とルートファイルシステム（`rootfs.ext4`）のディレクトリ |
| `egress_bin` | （必須） | コンテナに渡す `chloe-egress` のバイナリ |
| `state_root` | （必須） | ワークスペースごとの状態（ディスク、ログ、CA）を置くディレクトリ |
| `image` | `chloe-dev-runner` | ランナーのコンテナのイメージ |
| `name_prefix` | `chloe-` | コンテナの名前の接頭辞。同じホストで複数のゲートウェイを動かすときに分ける |

VM の大きさは固定である（4 vCPU、メモリ 4GiB、ワークスペースのディスク 20GB、Docker のディスク 40GB）。

### `[cluster]`

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `listen` | （必須） | `chloe-node` から受ける内部 API（相互 TLS）の待ち受け |
| `server_names` | （必須） | 内部 API の証明書に入れる名前。`chloe-node` がつなぐホスト名か IP アドレス |
| `pki_dir` | （必須） | CA の証明書と鍵を置くディレクトリ。なければ作る |
| `join_ttl_secs` | `3600` | `node add` で作る参加用のトークンの有効期間 |

コントロールプレーンの証明書は、起動のたびに CA で発行し直す（鍵はファイルに置かない）。

### `[api]`

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `listen` | （必須） | 管理 API の待ち受け |
| `tls_cert`、`tls_key` | なし | TLS の証明書と鍵（PEM）。どちらも書けば HTTPS、どちらもなければ HTTP |

## `chloe-node`

| 項目 | 内容 |
| --- | --- |
| `listen` | コントロールプレーンからの API（相互 TLS）の待ち受け。`node add` の URL と同じポートにする |
| `credentials` | `chloe-node join` で受け取った証明書と鍵のディレクトリ |
| `control_plane` | コントロールプレーンの内部 API の URL（本物の値の問い合わせを中継する先） |
| `[runner]` | ゲートウェイの `[runner]` と同じ |

## GitHub App

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `app_id` | （必須） | App ID |
| `client_id` | （必須） | Client ID（Device Flow に使う） |
| `private_key` | （必須） | 秘密鍵の参照（`file:/path/to/key.pem`、`env:NAME`、`op://vault/item/field`） |
| `organizations` | なし | 使う Organization を限る。書かなければ、アプリをインストールしたすべての Organization を使う |
| `organization` | なし | `organizations` を一つだけ書くときの書き方 |
| `api_url`、`web_url` | `https://api.github.com`、`https://github.com` | GitHub の API と Web の URL |

アプリを Any account で公開すると、だれでも自分の Organization にアプリをインストールでき、そのメンバーが chloe にログインできるようになる。
そのときは `organizations` で、使ってよい Organization に限る。

## Organization と team の設定

`chloe policy set acme`（Organization）か `chloe policy set acme/dev`（team）で、TOML を標準入力から渡す。
管理 API では、同じ項目を JSON で渡す。
変えられるのは、GitHub でその Organization の管理者である利用者と、ゲートウェイの設定の `admins` である。

| 項目 | 内容 |
| --- | --- |
| `allow` | 足す宛先 |
| `rules` | 足す規則（書き方はゲートウェイの `[[policy.rules]]` と同じ） |
| `root` | 新しく作るワークスペースのルートの扱いの既定（`ephemeral` か `persistent`） |
| `[github]` | 共有ワークスペースに渡す GitHub のインストールトークンの範囲 |

```toml
allow = ["registry.example.com"]
root = "persistent"

[[rules]]
hosts = ["api.github.com"]
paths = ["/repos/acme/**"]
action = "allow"

[github]
repositories = ["web", "api"]                  # 省くとインストールのすべてのリポジトリ
permissions = { contents = "write", pull_requests = "write" }   # 省くとインストールの権限すべて
```

ワークスペースの方針は、ゲートウェイの `[policy]`、Organization の設定、team の設定の順に重ねて作る。

- **宛先**：足し合わせる。
- **規則**：上の層のものを先に並べる。最初に一致した規則で決まるので、下の層は上の層の規則を覆せない（サーバーの運用者の規則を、Organization の設定で覆すことはできない）。
- **`root` と `[github]`**：もっとも下の層で書いた値を使う。

`passthrough` は、Organization と team の設定には書けない。
復号しない宛先には規則が効かないので、書けると、上の層の規則を素通りする宛先を作れてしまうためである。

個人のワークスペースには、ゲートウェイの `[policy]` だけを使う（どの Organization の設定を重ねるかが決まらないので）。

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
| `ssh_user` | `chloe` | 誰でも使う SSH のユーザー名。このユーザー名と GitHub に存在しない名前では、利用者を結び付けた公開鍵か Device Flow で決める（`github_app` が要る） |
| `github_app` | なし | GitHub App の設定のパス |
| `allowed_users` | `[]` | ログインを許す GitHub のログイン名。GitHub App の Organization のメンバーに加えて許す |
| `admins` | `[]` | サーバーの運用者。どの Organization と team の設定とシークレットも変えられる |
| `memberships` | `{}` | 共有ワークスペースに入れる利用者を、GitHub の所属に加えて書く（開発用） |
| `idle_timeout_secs` | `1800` | 最後の接続が切れてから VM を止めるまでの秒数 |
| `activity_poll_secs` | `60` | エージェントが作業中のとき、作業中かを尋ね直す間隔の秒数 |
| `max_busy_wait_secs` | `43200` | エージェントが作業中でも、この秒数が経ったら止める |
| `reconcile_interval_secs` | `60` | 動いているランナーと記録を突き合わせる間隔の秒数 |
| `[github_token]` | github.com 向け | GitHub のトークンを VM に渡す定義（下を参照） |
| `[policy]` | 空 | すべてのワークスペースに共通の許可リストと規則 |
| `[disks]` | 下を参照 | ワークスペースのディスクの大きさの既定と上限 |
| `[runner]` | — | 同じホストの Docker にランナーを置く。`[cluster]` とどちらか一方を書く |
| `[cluster]` | — | 複数のホスト（`chloe-node`）にランナーを置く |
| `[api]` | なし | HTTP(S) の管理 API |

`memberships` は、持ち主ごとにログイン名を並べる。

```toml
[memberships]
"org:acme" = ["alice"]
"team:acme/dev" = ["alice", "bob"]
```

### `[github_token]`

GitHub のトークン（`github_token`）を、VM のどの環境変数に入れ、どの宛先のどのヘッダーで差し替えるかを決める。
書き方は、[シークレットの定義](usage.md#シークレット)から値を除いたものである。
欄は一つ（`value`）に限る（値は GitHub App やトークンの登録から決まるため）。
省くと次の定義を使う。

```toml
[github_token]
env = ["GH_TOKEN", "GITHUB_TOKEN"]
placeholder = "ghu_{id}"

[[github_token.inject]]
hosts = ["github.com"]
header = "authorization"
format = "basic-password"          # git の HTTPS（Basic 認証のパスワード）

[[github_token.inject]]
hosts = ["api.github.com", "uploads.github.com"]
header = "authorization"
```

GitHub Enterprise Server に向けるときは、`hosts` をそのホストに書き換える。
GitHub App の設定の `api_url` と `web_url` も、同じホストに合わせる。

### `[policy]`

すべてのワークスペースに共通の、VM から外への通信の決まりである。
宛先、メソッド、パスの規則（`[[policy.rules]]`）だけで書く。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `name` | なし | ログに残す名前 |
| `hosts` | （必須） | 宛先 |
| `methods` | すべてのメソッド | メソッド |
| `paths` | すべてのパス | パス |
| `action` | （必須） | `allow`（許す）、`deny`（拒否する）、`passthrough`（復号せずに中継だけする） |

規則は上から順に評価し、最初に一致した規則で決める。
どの規則にも一致しなければ遮断する。
そのため、パスを絞った `allow` の規則は、その宛先のほかのパスを許さない。

```toml
[[policy.rules]]
name = "github-no-delete"
hosts = ["api.github.com"]
methods = ["DELETE"]
paths = ["/repos/**"]
action = "deny"

[[policy.rules]]
name = "default"
hosts = ["github.com", "api.github.com", "pypi.org"]
action = "allow"

[[policy.rules]]
hosts = ["registry-1.docker.io", "*.docker.com"]
action = "passthrough"
```

宛先は名前で書く。
`*.example.com` は example.com のサブドメインに一致し、example.com そのものには一致しない。
VM の DNS は、`allow` と `passthrough` の規則にある名前だけを引ける（`*.example.com` は、DNS では example.com そのものも引ける）。
`deny` の規則だけに書いた名前は引けない。

`passthrough` は、証明書を固定するクライアント向けである。
復号しないので、規則も差し替えも効かない。
`methods` と `paths` は書けない。
また、ほかの規則と宛先が重なってはいけない（`*.docker.com` と `hub.docker.com` のように、どちらにも一致する名前があれば重なる）。
接続の時点で中継に決まるので、重なると、もう一方の規則が順番にかかわらず効かなくなるためである。
重なりがあると、ゲートウェイは起動しない。

シークレットの差し替えと署名の宛先（`api.anthropic.com` や、登録した定義の `hosts`）は、そのシークレットを使うワークスペースでだけ、`allow` の規則として一番後ろに足す。
ただし、すでにどれかの規則に書いた宛先と重なる宛先には足さない（`*.amazonaws.com` は、規則に `s3.amazonaws.com` があれば足さない）。
足すと、パスを絞って許した宛先のほかのパスまで許してしまうためである。
規則に書いた宛先は、書いた規則だけで許すかどうかが決まる。

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

VM の CPU とメモリは固定である（4 vCPU、メモリ 4GiB）。
ディスクの大きさは `[disks]` で決める。

### `[disks]`

ワークスペースのディスクと Docker のディスクの大きさ（GiB）。
利用者は `chloe ws create` と `chloe ws disk` で、ワークスペースごとに上限まで大きくできる。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `workspace_gib` | `20` | ワークスペースのディスク（`/workspace`、ホームディレクトリ、永続のルートの層）の既定 |
| `docker_gib` | `40` | Docker のディスクの既定 |
| `max_workspace_gib` | `200` | ワークスペースのディスクの上限 |
| `max_docker_gib` | `400` | Docker のディスクの上限 |

ディスクは疎なファイルなので、ホストの容量は大きさではなく使った分だけ消費する。
そのため、ワークスペースの数に上限を掛けた値はホストの容量を超えていても起動できる。
ただし、利用者がディスクを使い込むと、ホストのディスクがあふれる可能性がある。
上限は、ホストの空き容量とワークスペースの数を見て決める。

大きさを指定していないワークスペースは、既定を上げると次の起動で広がる。
既定を下げても、すでにあるディスクは縮まない。
`[cluster]` のときも、ここに書く（`chloe-node` の設定には書かない）。

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
| `rules` | 足す規則（書き方はゲートウェイの `[[policy.rules]]` と同じ。`passthrough` は書けない） |
| `root` | 新しく作るワークスペースのルートの扱いの既定（`ephemeral` か `persistent`） |
| `[github]` | 共有ワークスペースに渡す GitHub のインストールトークンの範囲 |

```toml
root = "persistent"

[[rules]]
hosts = ["registry.example.com"]
action = "allow"

[[rules]]
hosts = ["api.github.com"]
paths = ["/repos/acme/**"]
action = "allow"

[github]
repositories = ["web", "api"]                  # 省くとインストールのすべてのリポジトリ
permissions = { contents = "write", pull_requests = "write" }   # 省くとインストールの権限すべて
```

ワークスペースの方針は、ゲートウェイの `[policy]`、Organization の設定、team の設定の順に重ねて作る。

- **規則**：上の層のものを先に並べる。最初に一致した規則で決まるので、下の層は上の層の規則を覆せない（サーバーの運用者の規則を、Organization の設定で覆すことはできない）。
- **`root` と `[github]`**：もっとも下の層で書いた値を使う。

`passthrough` の規則は、Organization と team の設定には書けない。
復号しない宛先には規則が効かないので、書けると、上の層の規則を素通りする宛先を作れてしまうためである。
ゲートウェイの `passthrough` の規則と宛先が重なる規則も、書けない（理由は [`[policy]`](#policy) と同じ）。
ゲートウェイの設定をあとから変えて、保存してある設定と重なったときは、そのワークスペースのランナーを起動しない。

個人のワークスペースには、ゲートウェイの `[policy]` だけを使う（どの Organization の設定を重ねるかが決まらないので）。

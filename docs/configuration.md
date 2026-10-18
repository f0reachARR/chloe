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
| 個人、Organization、team の設定とワークスペースの規則 | `chloe policy set`、`chloe ws policy set`、管理 API、管理の画面 | 下の「個人、Organization、team の設定」と「ワークスペースの規則」 |

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
| `[ban]` | 下を参照 | 認証を通らない接続が多い接続元を、しばらく受け付けない |

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
宛先、メソッド、パスの規則（`[[policy.rules]]`）で書く。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `allow_personal_rules` | `false` | 利用者が、自分の個人の設定と個人のワークスペースの規則を変えられるようにする（[設定の重ね方](#設定の重ね方)） |

規則の項目は次のとおりである。

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

同じホストで、ワークスペースごとにランナーを起動する。
`kind` で、ランナーの動かし方を選ぶ。

| `kind` | 動かし方 |
| --- | --- |
| `docker`（既定） | ランナーのコンテナ（`chloe-dev-runner`）を Docker の特権コンテナで起動する。ゲートウェイを動かす利用者は `docker` のグループに入る |
| `systemd` | ランナーを systemd の unit（`chloe-runner@ws-<番号>.service`）で起動する。root で動くのはネットワークの準備だけで、ゲートウェイを動かす利用者に root の権限は要らない（[インストール](install.md#ランナーを-systemd-で動かす)） |

`kind = "docker"` のときの項目は次のとおり。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `guest_dir` | （必須） | カーネル（`kernel/vmlinux`）とルートファイルシステム（`rootfs.ext4`）のディレクトリ |
| `egress_bin` | （必須） | コンテナに渡す `chloe-egress` のバイナリ |
| `state_root` | （必須） | ワークスペースごとの状態（ディスク、ログ、CA）を置くディレクトリ |
| `image` | `chloe-dev-runner` | ランナーのコンテナのイメージ |
| `name_prefix` | `chloe-` | コンテナの名前の接頭辞。同じホストで複数のゲートウェイを動かすときに分ける |

`kind = "systemd"` のときの項目は次のとおり。
パスは、ランナーの unit と同じファイル（`env_file`）から読む。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `env_file` | `/etc/chloe/runner.env` | ランナーの設定のファイル |

`env_file` は `KEY=VALUE` の行で書く（例は `deploy/runner.env`）。
ランナーの unit のうち root で動く部分も読むので、root だけが書けるようにする。

| 項目 | 内容 |
| --- | --- |
| `CHLOE_STATE_ROOT` | ワークスペースごとの状態を置くディレクトリ（絶対パス）。ゲートウェイを動かす利用者が書けること |
| `CHLOE_GUEST_DIR` | カーネルとルートファイルシステムのディレクトリ |
| `CHLOE_EGRESS_BIN` | `chloe-egress` のバイナリ |
| `CHLOE_NET_POOL` | ランナーのネットワーク namespace とホストをつなぐ veth のアドレス（既定 `10.250.0.0/16`）。ランナー一つに /30 を使うので、/16 なら 16384 個まで同時に動かせる。ホストのネットワークと重ならない範囲にする |

一つのホストに置けるランナーの設定は一組である（unit がこのファイルを読むため）。

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
ディスクを広げるのはランナーである。
ランナーを Docker で動かすときは、更新したあと、どのホストでもランナーのコンテナのイメージ（`chloe-dev-runner`）をビルドし直す（[インストール](install.md#更新)）。

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

### `[ban]`

SSH の接続のうち、認証を通らずに終わったものを接続元ごとに数える。
認証に失敗した接続も、何もせずに切った接続も、同じ一回に数える。
`window_secs` のあいだにその数が `max_failures` に達した接続元は、`ban_secs` のあいだ SSH を始める前に切る。
認証を通ると、それまでの数を忘れる。
IPv4 はアドレスごと、IPv6 は /64 ごとに数える。

| 項目 | 既定 | 内容 |
| --- | --- | --- |
| `max_failures` | `10` | ban するまでの回数。`0` なら ban しない |
| `window_secs` | `600` | 回数を数える期間の秒数 |
| `ban_secs` | `600` | ban する秒数 |
| `ignore` | `["127.0.0.0/8", "::1"]` | 数えない接続元（アドレスか CIDR） |

ban したことは監査ログに残る。ban の状態はメモリにだけ置くので、ゲートウェイを再起動すると消える。
ロードバランサーのヘルスチェックや監視は、SSH を始めずに切るので失敗に数える。それらの接続元は `ignore` に書く。
`ignore` を書くと既定を置き換えるので、ループバックも数えないなら一緒に書く。
接続元を書き換えるロードバランサー（PROXY プロトコルなし）の後ろでは、すべての接続が同じ接続元に見えるので、`max_failures = 0` にする。

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

## 個人、Organization、team の設定

`chloe policy set acme`（Organization）か `chloe policy set acme/dev`（team）で、TOML を標準入力から渡す。
`chloe policy set` のように持ち主を省くと、自分の個人の設定になる。
管理 API では、同じ項目を JSON で渡す。
管理の画面では、規則は Rules タブで、そのほかは Settings タブで編集する。

| 持ち主 | 変えられる人 |
| --- | --- |
| Organization、team | GitHub でその Organization の管理者である利用者と、ゲートウェイの設定の `admins` |
| 個人 | 本人。ゲートウェイの `[policy]` で `allow_personal_rules = true` にしたときだけ |

| 項目 | 内容 |
| --- | --- |
| `rules` | 足す規則（書き方はゲートウェイの `[[policy.rules]]` と同じ。`passthrough` は書けない） |
| `root` | 新しく作るワークスペースのルートの扱いの既定（`ephemeral` か `persistent`） |
| `[github]` | 共有ワークスペースに渡す GitHub のインストールトークンの範囲（個人の設定には書けない） |

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

個人の設定は、その人が持ち主のワークスペース（個人のワークスペース）にだけ重ねる。
その人が入る Organization や team のワークスペースには重ねない。
共有のランナーの規則が、入った人によって変わらないようにするためである。
個人のワークスペースは利用者自身の GitHub のトークンを使うので、`[github]` は書けない。

## ワークスペースの規則

`chloe ws policy set acme/web` で、規則だけを TOML で標準入力から渡す。
管理 API では `/v1/workspace-policies/{name}` に JSON で渡す。
管理の画面では、Rules タブでワークスペースを選んで編集する。

```toml
[[rules]]
hosts = ["staging.example.com"]
action = "allow"
```

書けるのは `rules` だけである。
`root` はワークスペースそのものが持つので、`chloe ws root` で変える。
`[github]` は下の層ほど優先されるので、ワークスペースで書けると、持ち主の設定で絞った範囲を広げられてしまう。
ワークスペースを消すと、その規則も消える。

変えられる人は、そのワークスペースの持ち主のシークレットを変えられる人と同じである。

| ワークスペース | 変えられる人 |
| --- | --- |
| 個人 | 本人。ゲートウェイの `[policy]` で `allow_personal_rules = true` にしたときだけ |
| Organization | Organization の管理者と、ゲートウェイの設定の `admins` |
| team | Organization の管理者、team の maintainer、ゲートウェイの設定の `admins` |

## 設定の重ね方

ワークスペースの方針は、次の順に層を重ねて作る。

| ワークスペース | 層の順 |
| --- | --- |
| 個人 | ゲートウェイの `[policy]`、個人の設定、ワークスペースの規則 |
| Organization | ゲートウェイの `[policy]`、Organization の設定、ワークスペースの規則 |
| team | ゲートウェイの `[policy]`、Organization の設定、team の設定、ワークスペースの規則 |

- **規則**：上の層のものを先に並べる。最初に一致した規則で決まるので、下の層は上の層の規則を覆せない（サーバーの運用者の規則を、Organization の設定で覆すことはできない）。
- **`root` と `[github]`**：もっとも下の層で書いた値を使う（ワークスペースの規則は、どちらも持たない）。

そのため、下の層でできるのは、上の層のどの規則にも一致しない宛先を許すか拒否することだけである。
上の層が許した宛先を、下の層の `deny` で塞ぐことはできない。

`allow_personal_rules` を有効にしていないときは、個人の設定と個人のワークスペースの規則を重ねない（保存してあっても効かない）。

`passthrough` の規則は、ゲートウェイの `[policy]` のほかには書けない。
復号しない宛先には規則が効かないので、書けると、上の層の規則を素通りする宛先を作れてしまうためである。
ゲートウェイの `passthrough` の規則と宛先が重なる規則も、書けない（理由は [`[policy]`](#policy) と同じ）。
ゲートウェイの設定をあとから変えて、保存してある設定と重なったときは、そのワークスペースのランナーを起動しない。

シークレットの宛先は、どの層の規則にもない宛先だけを一番後ろに足す（[`[policy]`](#policy) を参照）。
そのため、ある層で `api.anthropic.com` を `deny` にすると、その層を重ねるワークスペースでは、そのシークレットの差し替えも効かなくなる。

管理の画面の Rules タブでは、選んだスコープで効く規則を、評価する順に層の名前と一緒に出す。

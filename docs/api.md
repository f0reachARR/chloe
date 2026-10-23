# 管理 API

SSH の `chloe` コマンドと同じ操作を、HTTP(S) と JSON で行う。
有効にする手順は [インストール](install.md#管理-api-を有効にする) にある。
パスはすべて `/api/v1` で始まる（以前の `/v1` からは移した）。
OpenAPI のスペックは `chloe-gateway openapi` で書き出せ、リポジトリの `web/openapi.json` にも置いてある。

## 認証

SSH で、API のトークンを作る。
トークンはこのときにしか表示しない（データベースにはハッシュだけを置く）。

```sh
ssh alice@chloe.example.com chloe token create ci --expires 30d
```

トークンを `Authorization: Bearer` で渡す。

```sh
export CHLOE_TOKEN=chloe_...
curl -H "Authorization: Bearer $CHLOE_TOKEN" https://chloe.example.com:8080/api/v1/me
```

- トークンの権限は、作った利用者の権限と同じである。
- 利用者がログインできなくなったら（Organization から抜けたなど）、トークンも使えない。
- `--expires` を省くと期限がない。期限は `30d`（日）か `12h`（時間）で書く。
- `chloe token list` で一覧を、`chloe token revoke <ID>` で失効させる。API の `/api/v1/tokens` でも同じことができる。

API の呼び出しは、利用者、トークンの ID、メソッド、パス、ステータスを監査ログに残す（本文は残さない）。

### Web の画面のセッション

[Web の画面](usage.md#web-の画面) は、トークンの代わりにセッションの cookie（`chloe_session`）で同じ API を呼ぶ。
cookie はブラウザが自動で付けるので、ほかのサイトから送らせた要求を断るために、次のように扱う。

- GET と HEAD のほかの要求と、WebSocket への切り替え（`Upgrade` のある要求。端末）は、`Origin` がゲートウェイの `public_url` と一致しなければ 403 にする。
- トークンを作る（`POST /api/v1/tokens`）ことと、シークレットを登録する（`PUT /api/v1/secrets/{name}`）ことは、cookie では 403 にする。
- 監査ログには `via = "web"` とセッションの ID を残す。

ログインの経路は `/api/auth` にある（`GET /api/auth/methods`、`GET /api/auth/github`、`POST /api/auth/link`、`POST /api/auth/logout`）。

## 誤り

誤りのときは、ステータスと `{"error": "..."}` を返す。

| ステータス | 意味 |
| --- | --- |
| 400 | 指定が正しくない（JSON、持ち主やシークレットの種類の書き方、設定の項目） |
| 401 | トークンもセッションもない、知らない、期限切れか失効した |
| 403 | 権限がない |
| 404 | ない（入れないワークスペースも、ないものとして扱う） |
| 409 | すでにある |

## 一覧

ワークスペースの名前（`{name}`）は、SSH のユーザー名の `+` のあとと同じ書き方（`api`、`acme/web`、`acme/dev/tools`）。
持ち主（`{owner}`、`owner=`）は Organization（`acme`）か team（`acme/dev`）。

| メソッドとパス | 内容 |
| --- | --- |
| `GET /api/v1/me` | トークンかセッションの利用者 |
| `GET /api/v1/events` | 依頼の出来事（SSE） |
| `GET /api/v1/workspaces[?status=true]` | 入れるワークスペースの一覧（`status=true` なら、エージェントが作業中かを VM に尋ねる） |
| `POST /api/v1/workspaces` | ワークスペースを作る |
| `GET /api/v1/workspaces/{name}[?status=true]` | ワークスペース |
| `PATCH /api/v1/workspaces/{name}` | ルートの扱い、ディスクや VM の大きさ、止めるまでの時間を変える、止める |
| `DELETE /api/v1/workspaces/{name}[?disks=true]` | ワークスペースを消す（`disks=true` でなければディスクは残る） |
| `GET /api/v1/workspace-requests/{name}[?lines=]` | HTTP の要求の記録の末尾（既定 200 件、2000 件まで） |
| `GET /api/v1/workspace-terminal/{name}[?cols=&rows=]` | シェルの端末（WebSocket） |
| `GET /api/v1/workspace-ports/{name}` | VM の中で公開したポートと、ブラウザで開く URL |
| `DELETE /api/v1/workspace-ports/{name}?port=` | ポートの公開を取り消す |
| `GET /api/v1/secrets[?owner=]` | シークレットの名前の一覧 |
| `PUT /api/v1/secrets/{kind}[?owner=]` | シークレットを登録する |
| `DELETE /api/v1/secrets/{kind}[?owner=]` | シークレットを消す |
| `GET /api/v1/policies/{owner}` | Organization か team の設定 |
| `PUT /api/v1/policies/{owner}` | 設定を置き換える |
| `DELETE /api/v1/policies/{owner}` | 設定を消す |
| `GET /api/v1/tokens` | 自分のトークンの一覧（値は含まない） |
| `POST /api/v1/tokens` | トークンを作る |
| `DELETE /api/v1/tokens/{id}` | トークンを失効させる |
| `GET /api/v1/asks[?all=true]` | 入れるワークスペースの、エージェントからの依頼（`all=true` でなければ答えを待つものだけ） |
| `GET /api/v1/asks/{id}` | 依頼 |
| `POST /api/v1/asks/{id}/answer` | 依頼に答える |

権限は SSH のコマンドと同じである。
シークレットの `owner=` を省くと、自分の個人のシークレットを扱う。

## 例

以下では `api` を次のように定める。

```sh
api() { curl -s -H "Authorization: Bearer $CHLOE_TOKEN" -H 'content-type: application/json' "https://chloe.example.com:8080$@"; }
```

### ワークスペース

```sh
api /api/v1/workspaces
# [{"name":"default","root":"ephemeral","running":true,"workspace_disk_gib":20,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800,"pending_asks":1}, ...]
api '/api/v1/workspaces?status=true'
# [{"name":"default",...,"pending_asks":1,"busy":["claude-4f1c…"]}, ...]   (busy は作業中のエージェントの印 (<ツール>-<セッション>)。止まっていれば []、VM が答えなければ null)

api /api/v1/workspaces -X POST -d '{"name": "acme/web", "root": "persistent", "workspace_disk_gib": 50}'
# {"name":"acme/web","root":"persistent","running":false,"workspace_disk_gib":50,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800,"pending_asks":0}   (201)

api /api/v1/workspaces/acme/web -X PATCH -d '{"root": "ephemeral"}'
api /api/v1/workspaces/acme/web -X PATCH -d '{"docker_disk_gib": 80}'   # 次の起動から
api /api/v1/workspaces/acme/web -X PATCH -d '{"vcpus": 8, "memory_mib": 16384}'   # 次の起動から
api /api/v1/workspaces/acme/web -X PATCH -d '{"idle_timeout": "never"}'
api /api/v1/workspaces/acme/web -X PATCH -d '{"running": false}'    # 止める
api /api/v1/workspaces/acme/web -X DELETE                             # 204
```

`root` を省いて作ると、持ち主（個人、Organization と team）の設定の既定を使う。
`workspace_disk_gib` と `docker_disk_gib` は GiB で、省くとゲートウェイの設定の既定を使う。
変えるときは大きくするだけで、今より小さい値と上限を超える値は 400 を返す。
新しい大きさは、次にワークスペースを起動したときに効く。
`vcpus`（1 から 32）と `memory_mib`（MiB、512 以上）も、省くとゲートウェイの設定の既定を使い、次の起動から効く。
`idle_timeout` は `chloe ws idle` と同じ書き方（`30m`、`2h`、`never`、`default`）で、返す `idle_timeout_secs` は既定を当てはめた秒数（止めないなら `null`）。
`"running": true` は受け付けない（ワークスペースは SSH でつないだときに起動する）。

### シークレット

```sh
api /api/v1/secrets/hf_token -X PUT -d @hf.json                                         # 204
api '/api/v1/secrets/hf_token?owner=acme/dev' -X PUT -d @hf.json
api /api/v1/secrets/hf_token -X PUT -d '{"value": "hf_..."}'      # 値だけを変える（定義はそのまま）
api /api/v1/secrets
# {"names":["hf_token"],"owner":"alice"}
api /api/v1/secrets/hf_token
# {"name":"hf_token","owner":"alice","builtin":false,"spec":{"fields":{"value":{...}},"inject":[...]}}
api /api/v1/secrets/hf_token -X DELETE
```

本文は、[使い方](usage.md#シークレット)の TOML と同じ項目を JSON で書く。

```json
{"env": ["HF_TOKEN"], "placeholder": "hf_{id}", "value": "hf_...",
 "inject": [{"hosts": ["huggingface.co"], "header": "authorization"}]}
```

値はシェルの履歴に残らないよう、ファイルや標準入力から渡す（`-d @value.json`）。

### 個人、Organization、team の設定

設定の項目は [設定](configuration.md#個人organizationteam-の設定) と同じで、JSON で書く。
自分の個人の設定は `/api/v1/policy` で読み書きする（ゲートウェイで `allow_personal_rules` を有効にしたときだけ変えられる）。

```sh
api /api/v1/policies/acme -X PUT \
  -d '{"rules": [{"hosts": ["registry.example.com"], "action": "allow"}], "root": "persistent"}'
# {"updated_runners":2}   (設定を送った、動いているランナーの数)

api /api/v1/policies/acme
# {"rules":[{"hosts":["registry.example.com"],"action":"allow"}],"root":"persistent"}

api /api/v1/policies/acme/dev -X PUT \
  -d '{"github": {"repositories": ["web"], "permissions": {"contents": "read"}}}'

api /api/v1/policy -X PUT -d '{"rules": [{"hosts": ["example.com"], "action": "allow"}]}'
```

### ワークスペースの規則

規則だけを JSON で書く（[設定](configuration.md#ワークスペースの規則)）。

```sh
api /api/v1/workspace-policies/acme/web -X PUT \
  -d '{"rules": [{"hosts": ["staging.example.com"], "action": "allow"}]}'
# {"updated_runners":1}

api /api/v1/workspace-policies/acme/web
api /api/v1/workspace-policies/acme/web -X DELETE
```

### HTTP の要求の記録

ワークスペースの HTTP の要求の記録の末尾を、古い順の配列で返す。
欄は `chloe-egress` が記録する一行と同じで、知らない欄が増えることがある。

```sh
api '/api/v1/workspace-requests/acme/web?lines=2'
# [{"ts":1792152059.1,"event":"passthrough","host":"registry.npmjs.org"},
#  {"ts":1792152060.4,"event":"request","host":"api.anthropic.com","sni":"api.anthropic.com",
#   "method":"POST","path":"/v1/messages","query_keys":[],"http_version":"HTTP/2.0","status":200,
#   "request_bytes":1834,"body_scanned":true,"error":null,"decision":"allowed",
#   "injected":["anthropic_api_key-0"],"response_bytes":5120,"duration_ms":2310}]
```

### エージェントからの依頼

本文には `allow`、`deny`、`answer` のどれか一つを書く。
`allow` は通信の許可に、`answer` は質問に使い、`deny` はどちらにも使える。
`can_allow` は、トークンの利用者が通信の許可を承認できるかを示す。

```sh
api /api/v1/asks
# [{"workspace":"acme/web","id":12,"kind":"network","host":"docs.example.com","reason":"API の仕様を読む",
#   "status":"pending","created_at":1792152059,"expires_at":1792238459,"can_allow":true}]
api /api/v1/asks/12/answer -X POST -d '{"allow": true}'
api /api/v1/asks/13/answer -X POST -d '{"answer": "dev"}'
api /api/v1/asks/14/answer -X POST -d '{"deny": true, "message": "今は不要"}'
# 答えた依頼に答え直すと 409
```

`/api/v1/events` は、依頼が出た、答えた、取り下げたという出来事を SSE で流す。
入れるワークスペースの出来事だけが届く。
出来事の名前は `ask` で、取りこぼしたときは `lagged` が届くので、一覧を読み直す。
15 分で閉じるので、つなぎ直す（そのときにトークンかセッションを確かめ直す）。

```sh
curl -sN -H "Authorization: Bearer $CHLOE_TOKEN" https://chloe.example.com:8080/api/v1/events
# event: ask
# data: {"type":"created","id":15,"workspace":"acme/web"}
```

### 端末

`/api/v1/workspace-terminal/{name}` は、WebSocket に切り替えて、ワークスペースのシェルを開く（SSH の `ssh -t` と同じ）。
ワークスペースが止まっていれば起動し、開いているあいだは止めない。
接続を閉じるとシェルは終わる。
`cols` と `rows` は最初の端末の大きさ（既定 80 と 24）で、端末の種類は `xterm-256color` になる。

- バイナリのフレームは、端末の入力と出力そのものである。
- テキストのフレームは JSON である。送るのは大きさの変更（`{"type":"resize","cols":120,"rows":40}`）だけで、受けるのは次の四つ。

| フレーム | 意味 |
| --- | --- |
| `{"type":"starting"}` | ワークスペースを用意している（止まっていれば VM を起動する。1 分ほどかかる） |
| `{"type":"started"}` | シェルが起動した |
| `{"type":"exit","code":0}` | シェルが終わった。このあと閉じる |
| `{"type":"error","message":"..."}` | 開けなかったか、続けられなくなった。このあと閉じる |

開いているあいだは、1 分ごとにトークンかセッションを確かめ直し、失効していれば `error` を送って閉じる。
入れないワークスペースは、切り替える前に 404 で断る。

```sh
websocat -H "Authorization: Bearer $CHLOE_TOKEN" --binary \
  'wss://chloe.example.com:8080/api/v1/workspace-terminal/acme%2Fweb?cols=120&rows=40'
```

### 公開したポート

VM の中で `chloe-ask port publish` で公開したポートは、`url` をブラウザで開くと、Web の画面のログインを通って VM の中のサーバーに届く（[使い方](usage.md#vm-の中のサーバーをブラウザで開く)）。
公開は VM の中からしかできず、管理 API では一覧と取り消しだけができる。

```sh
api /api/v1/workspace-ports/acme%2Fweb
# [{"port":5173,"scheme":"http","url":"https://k3x7q2m5vbtd.chloe-preview.example.net/","created_at":1792152059}]
api '/api/v1/workspace-ports/acme%2Fweb?port=5173' -X DELETE   # 204
```

プロキシのホスト（`url` のホスト）は、管理 API と同じトークンを `Authorization: Bearer` で受ける。
トークンは VM の中のサーバーに渡さない。

```sh
curl -H "Authorization: Bearer $CHLOE_TOKEN" https://k3x7q2m5vbtd.chloe-preview.example.net/
```

ブラウザで開くと、プロキシのホストは Web の画面の `/api/auth/proxy` へ送り、Web の画面のセッションで一度だけ使えるコードを作って、プロキシのホストの `/__chloe/auth` へ戻す。
プロキシのホストはコードを、そのホストだけに付く cookie（`chloe_proxy`）に替える。
この cookie は Web の画面のセッションに結び付いていて、ログアウトや `chloe web revoke` で使えなくなる。
Web の画面にログインしていなければ、ログインのあと（`/api/auth/continue`）に開こうとしたポートへ戻す。

### トークン

```sh
api /api/v1/tokens -X POST -d '{"name": "deploy", "expires": "7d"}'
# {"id":3,"token":"chloe_..."}   (201。値はこのときだけ返す)
api /api/v1/tokens
api /api/v1/tokens/3 -X DELETE
```

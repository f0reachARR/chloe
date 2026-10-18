# 管理 API

SSH の `chloe` コマンドと同じ操作を、HTTP(S) と JSON で行う。
有効にする手順は [インストール](install.md#管理-api-を有効にする) にある。

## 認証

SSH で、API のトークンを作る。
トークンはこのときにしか表示しない（データベースにはハッシュだけを置く）。

```sh
ssh alice@chloe.example.com chloe token create ci --expires 30d
```

トークンを `Authorization: Bearer` で渡す。

```sh
export CHLOE_TOKEN=chloe_...
curl -H "Authorization: Bearer $CHLOE_TOKEN" https://chloe.example.com:8080/v1/me
```

- トークンの権限は、作った利用者の権限と同じである。
- 利用者がログインできなくなったら（Organization から抜けたなど）、トークンも使えない。
- `--expires` を省くと期限がない。期限は `30d`（日）か `12h`（時間）で書く。
- `chloe token list` で一覧を、`chloe token revoke <ID>` で失効させる。API の `/v1/tokens` でも同じことができる。

API の呼び出しは、利用者、トークンの ID、メソッド、パス、ステータスを監査ログに残す（本文は残さない）。

## 誤り

誤りのときは、ステータスと `{"error": "..."}` を返す。

| ステータス | 意味 |
| --- | --- |
| 400 | 指定が正しくない（JSON、持ち主やシークレットの種類の書き方、設定の項目） |
| 401 | トークンがない、知らない、期限切れか失効した |
| 403 | 権限がない |
| 404 | ない（入れないワークスペースも、ないものとして扱う） |
| 409 | すでにある |

## 一覧

ワークスペースの名前（`{name}`）は、SSH のユーザー名の `+` のあとと同じ書き方（`api`、`acme/web`、`acme/dev/tools`）。
持ち主（`{owner}`、`owner=`）は Organization（`acme`）か team（`acme/dev`）。

| メソッドとパス | 内容 |
| --- | --- |
| `GET /v1/me` | トークンの利用者 |
| `GET /v1/workspaces` | 入れるワークスペースの一覧 |
| `POST /v1/workspaces` | ワークスペースを作る |
| `GET /v1/workspaces/{name}` | ワークスペース |
| `PATCH /v1/workspaces/{name}` | ルートの扱い、ディスクや VM の大きさ、止めるまでの時間を変える、止める |
| `DELETE /v1/workspaces/{name}[?disks=true]` | ワークスペースを消す（`disks=true` でなければディスクは残る） |
| `GET /v1/workspace-requests/{name}[?lines=]` | HTTP の要求の記録の末尾（既定 200 件、2000 件まで） |
| `GET /v1/secrets[?owner=]` | シークレットの名前の一覧 |
| `PUT /v1/secrets/{kind}[?owner=]` | シークレットを登録する |
| `DELETE /v1/secrets/{kind}[?owner=]` | シークレットを消す |
| `GET /v1/policies/{owner}` | Organization か team の設定 |
| `PUT /v1/policies/{owner}` | 設定を置き換える |
| `DELETE /v1/policies/{owner}` | 設定を消す |
| `GET /v1/tokens` | 自分のトークンの一覧（値は含まない） |
| `POST /v1/tokens` | トークンを作る |
| `DELETE /v1/tokens/{id}` | トークンを失効させる |

権限は SSH のコマンドと同じである。
シークレットの `owner=` を省くと、自分の個人のシークレットを扱う。

## 例

以下では `api` を次のように定める。

```sh
api() { curl -s -H "Authorization: Bearer $CHLOE_TOKEN" -H 'content-type: application/json' "https://chloe.example.com:8080$@"; }
```

### ワークスペース

```sh
api /v1/workspaces
# [{"name":"default","root":"ephemeral","running":true,"workspace_disk_gib":20,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800}, ...]

api /v1/workspaces -X POST -d '{"name": "acme/web", "root": "persistent", "workspace_disk_gib": 50}'
# {"name":"acme/web","root":"persistent","running":false,"workspace_disk_gib":50,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800}   (201)

api /v1/workspaces/acme/web -X PATCH -d '{"root": "ephemeral"}'
api /v1/workspaces/acme/web -X PATCH -d '{"docker_disk_gib": 80}'   # 次の起動から
api /v1/workspaces/acme/web -X PATCH -d '{"vcpus": 8, "memory_mib": 16384}'   # 次の起動から
api /v1/workspaces/acme/web -X PATCH -d '{"idle_timeout": "never"}'
api /v1/workspaces/acme/web -X PATCH -d '{"running": false}'    # 止める
api /v1/workspaces/acme/web -X DELETE                             # 204
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
api /v1/secrets/hf_token -X PUT -d @hf.json                                         # 204
api '/v1/secrets/hf_token?owner=acme/dev' -X PUT -d @hf.json
api /v1/secrets/hf_token -X PUT -d '{"value": "hf_..."}'      # 値だけを変える（定義はそのまま）
api /v1/secrets
# {"names":["hf_token"],"owner":"alice"}
api /v1/secrets/hf_token
# {"name":"hf_token","owner":"alice","builtin":false,"spec":{"fields":{"value":{...}},"inject":[...]}}
api /v1/secrets/hf_token -X DELETE
```

本文は、[使い方](usage.md#シークレット)の TOML と同じ項目を JSON で書く。

```json
{"env": ["HF_TOKEN"], "placeholder": "hf_{id}", "value": "hf_...",
 "inject": [{"hosts": ["huggingface.co"], "header": "authorization"}]}
```

値はシェルの履歴に残らないよう、ファイルや標準入力から渡す（`-d @value.json`）。

### 個人、Organization、team の設定

設定の項目は [設定](configuration.md#個人organizationteam-の設定) と同じで、JSON で書く。
自分の個人の設定は `/v1/policy` で読み書きする（ゲートウェイで `allow_personal_rules` を有効にしたときだけ変えられる）。

```sh
api /v1/policies/acme -X PUT \
  -d '{"rules": [{"hosts": ["registry.example.com"], "action": "allow"}], "root": "persistent"}'
# {"updated_runners":2}   (設定を送った、動いているランナーの数)

api /v1/policies/acme
# {"rules":[{"hosts":["registry.example.com"],"action":"allow"}],"root":"persistent"}

api /v1/policies/acme/dev -X PUT \
  -d '{"github": {"repositories": ["web"], "permissions": {"contents": "read"}}}'

api /v1/policy -X PUT -d '{"rules": [{"hosts": ["example.com"], "action": "allow"}]}'
```

### ワークスペースの規則

規則だけを JSON で書く（[設定](configuration.md#ワークスペースの規則)）。

```sh
api /v1/workspace-policies/acme/web -X PUT \
  -d '{"rules": [{"hosts": ["staging.example.com"], "action": "allow"}]}'
# {"updated_runners":1}

api /v1/workspace-policies/acme/web
api /v1/workspace-policies/acme/web -X DELETE
```

### HTTP の要求の記録

ワークスペースの HTTP の要求の記録の末尾を、古い順の配列で返す。
欄は `chloe-egress` が記録する一行と同じで、知らない欄が増えることがある。

```sh
api '/v1/workspace-requests/acme/web?lines=2'
# [{"ts":1792152059.1,"event":"passthrough","host":"registry.npmjs.org"},
#  {"ts":1792152060.4,"event":"request","host":"api.anthropic.com","sni":"api.anthropic.com",
#   "method":"POST","path":"/v1/messages","query_keys":[],"http_version":"HTTP/2.0","status":200,
#   "request_bytes":1834,"body_scanned":true,"error":null,"decision":"allowed",
#   "injected":["anthropic_api_key-0"],"response_bytes":5120,"duration_ms":2310}]
```

### トークン

```sh
api /v1/tokens -X POST -d '{"name": "deploy", "expires": "7d"}'
# {"id":3,"token":"chloe_..."}   (201。値はこのときだけ返す)
api /v1/tokens
api /v1/tokens/3 -X DELETE
```

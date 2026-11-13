# Admin API

The admin API performs the same operations as the `chloe` command over SSH, using HTTP(S) and JSON.
See [Installation](install.md#enable-the-admin-api) for how to enable it.
All paths start with `/api/v1` (they moved from the earlier `/v1`).
`chloe-gateway openapi` writes out the OpenAPI spec, and a copy lives in the repository at `web/openapi.json`.

## Authentication

Create an API token over SSH.
The token is shown only at this point (the database stores only its hash).

```sh
ssh alice@chloe.example.com chloe token create ci --expires 30d
```

Pass the token in `Authorization: Bearer`.

```sh
export CHLOE_TOKEN=chloe_...
curl -H "Authorization: Bearer $CHLOE_TOKEN" https://chloe.example.com:8080/api/v1/me
```

- A token has the same permissions as the user who created it.
- If the user can no longer log in (for example, they left the organization), the token stops working too.
- Without `--expires`, the token never expires. Write the expiry as `30d` (days) or `12h` (hours).
- `chloe token list` lists your tokens, and `chloe token revoke <ID>` revokes one. The API's `/api/v1/tokens` does the same.

Every API call records the user, token ID, method, path, and status in the audit log (request bodies are not recorded).

### Web UI sessions

The [web UI](usage.md#web-ui) calls the same API with a session cookie (`chloe_session`) instead of a token.
Because the browser attaches the cookie automatically, the API handles it as follows to reject requests forged from other sites.

- Requests other than GET and HEAD, and upgrades to WebSocket (requests with `Upgrade`; the terminal), get 403 unless `Origin` matches the gateway's `public_url`.
- Creating a token (`POST /api/v1/tokens`) gets 403 with a cookie.
- Registering a secret (`PUT /api/v1/secrets/{name}`) works with a cookie. When you write a definition, you must always supply values for every field; the API has no way to change only the definition while keeping the registered values ([Secrets](#secrets)).
- The audit log records `via = "web"` and the session ID.

The login endpoints live under `/api/auth` (`GET /api/auth/methods`, `GET /api/auth/github`, `POST /api/auth/link`, `POST /api/auth/logout`).

## Errors

On error, the API returns a status and `{"error": "..."}`.

| Status | Meaning |
| --- | --- |
| 400 | Invalid input (JSON, owner or secret kind syntax, settings fields) |
| 401 | No token or session, or it is unknown, expired, or revoked |
| 403 | Not permitted |
| 404 | Not found (workspaces you cannot access are also treated as not found) |
| 409 | Already exists |

## Endpoints

Workspace names (`{name}`) use the same syntax as the part after `+` in the SSH user name (`api`, `acme/web`, `acme/dev/tools`).
An owner (`{owner}`, `owner=`) is an organization (`acme`) or a team (`acme/dev`).

| Method and path | Description |
| --- | --- |
| `GET /api/v1/me` | The user behind the token or session |
| `GET /api/v1/events` | Ask events (SSE) |
| `GET /api/v1/owners` | Owners you can access (user, organization, team): where you can create workspaces |
| `GET /api/v1/workspaces[?status=true]` | Workspaces you can access (with `status=true`, asks each VM whether an agent is busy) |
| `POST /api/v1/workspaces` | Create a workspace |
| `GET /api/v1/workspaces/{name}[?status=true]` | A workspace |
| `PATCH /api/v1/workspaces/{name}` | Change the root mode, disk or VM size, or idle timeout; stop the workspace |
| `DELETE /api/v1/workspaces/{name}[?disks=true]` | Delete a workspace (disks are kept unless `disks=true`) |
| `GET /api/v1/workspace-policies/{name}` | Workspace rules (`ETag` is the version) |
| `PUT /api/v1/workspace-policies/{name}` | Replace the workspace rules (`If-Match` checks the version) |
| `DELETE /api/v1/workspace-policies/{name}` | Delete the workspace rules |
| `GET /api/v1/workspace-rules/{name}` | Rules in effect for the workspace with their sources, the workspace's own rules and their version, and whether you can change them |
| `GET /api/v1/workspace-requests/{name}[?lines=]` | Tail of the HTTP request log (200 entries by default, up to 2000) |
| `GET /api/v1/workspace-terminal/{name}[?cols=&rows=]` | Shell terminal (WebSocket) |
| `GET /api/v1/workspace-ports/{name}` | Ports published in the VM, with the URLs to open in a browser |
| `DELETE /api/v1/workspace-ports/{name}?port=` | Unpublish a port |
| `GET /api/v1/secrets[?owner=]` | Secret names and definitions |
| `GET /api/v1/secrets/{kind}[?owner=]` | A secret's definition (values are not returned) |
| `GET /api/v1/secret-templates` | Secret templates (the same ones as the admin TUI; definitions only) |
| `PUT /api/v1/secrets/{kind}[?owner=]` | Register a secret |
| `DELETE /api/v1/secrets/{kind}[?owner=]` | Delete a secret |
| `GET /api/v1/policy-scopes` | Configuration layers you can view (gateway, user, organization, team), with whether you can change them, their rule counts, and workspaces |
| `GET /api/v1/policy-rules?scope=` | Rules in effect at a layer with their sources, the layer's own settings and their version, and whether you can change them |
| `GET /api/v1/policy` | Your user settings (`ETag` is the version) |
| `PUT /api/v1/policy` | Replace your user settings (`If-Match` checks the version) |
| `GET /api/v1/policies/{owner}` | Organization or team settings (`ETag` is the version) |
| `PUT /api/v1/policies/{owner}` | Replace the settings (`If-Match` checks the version) |
| `DELETE /api/v1/policies/{owner}` | Delete the settings |
| `GET /api/v1/tokens` | Your tokens (without their values) |
| `POST /api/v1/tokens` | Create a token |
| `DELETE /api/v1/tokens/{id}` | Revoke a token |
| `GET /api/v1/asks[?all=true]` | Asks from agents in workspaces you can access (only those awaiting an answer unless `all=true`) |
| `GET /api/v1/asks/{id}` | An ask |
| `POST /api/v1/asks/{id}/answer` | Answer an ask |

Permissions are the same as for the SSH commands.
If you omit `owner=` for secrets, you work with your own personal secrets.

## Examples

The examples below define `api` as follows.

```sh
api() { curl -s -H "Authorization: Bearer $CHLOE_TOKEN" -H 'content-type: application/json' "https://chloe.example.com:8080$@"; }
```

### Workspaces

```sh
api /api/v1/workspaces
# [{"name":"default","root":"ephemeral","running":true,"workspace_disk_gib":20,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800,"pending_asks":1}, ...]
api '/api/v1/workspaces?status=true'
# [{"name":"default",...,"pending_asks":1,"busy":["claude-4f1c…"]}, ...]   (busy lists markers of busy agents (<tool>-<session>): [] if idle, null if the VM did not answer)

api /api/v1/workspaces -X POST -d '{"name": "acme/web", "root": "persistent", "workspace_disk_gib": 50}'
# {"name":"acme/web","root":"persistent","running":false,"workspace_disk_gib":50,"docker_disk_gib":40,"vcpus":4,"memory_mib":4096,"idle_timeout_secs":1800,"pending_asks":0}   (201)

api /api/v1/workspaces/acme/web -X PATCH -d '{"root": "ephemeral"}'
api /api/v1/workspaces/acme/web -X PATCH -d '{"docker_disk_gib": 80}'   # from the next start
api /api/v1/workspaces/acme/web -X PATCH -d '{"vcpus": 8, "memory_mib": 16384}'   # from the next start
api /api/v1/workspaces/acme/web -X PATCH -d '{"idle_timeout": "never"}'
api /api/v1/workspaces/acme/web -X PATCH -d '{"running": false}'    # stop
api /api/v1/workspaces/acme/web -X DELETE                             # 204
```

`GET /api/v1/owners` returns the owners you can create workspaces under, in the order user, organizations, then their teams.
Prefixing a workspace name with `prefix` gives the `name` to use when creating it.
With a GitHub App, the teams of organizations you belong to are listed even if they have no workspaces (results from GitHub are cached for 5 minutes).

```sh
api /api/v1/owners
# [{"owner":"user:alice","kind":"user","name":"alice","prefix":""},
#  {"owner":"org:acme","kind":"org","name":"acme","prefix":"acme/"},
#  {"owner":"team:acme/dev","kind":"team","name":"acme/dev","prefix":"acme/dev/"}]
```

If you omit `root` when creating a workspace, the default from the owner's (user, organization, or team) settings applies.
`workspace_disk_gib` and `docker_disk_gib` are in GiB; if omitted, the gateway configuration's defaults apply.
You can only grow them: a value smaller than the current one or above the limit returns 400.
The new size takes effect the next time the workspace starts.
`vcpus` (1 to 32) and `memory_mib` (MiB, 512 or more) likewise fall back to the gateway configuration's defaults when omitted, and take effect from the next start.
`idle_timeout` uses the same syntax as `chloe ws idle` (`30m`, `2h`, `never`, `default`), and the returned `idle_timeout_secs` is the number of seconds after applying defaults (`null` if the workspace never stops).
`"running": true` is not accepted (a workspace starts when you connect over SSH).

### Secrets

```sh
api /api/v1/secrets/hf_token -X PUT -d @hf.json                                         # 204
api '/api/v1/secrets/hf_token?owner=acme/dev' -X PUT -d @hf.json
api /api/v1/secrets/hf_token -X PUT -d '{"value": "hf_..."}'      # change only the value (definition unchanged)
api /api/v1/secrets
# {"owner":"alice","names":["hf_token"],"entries":[{"name":"hf_token","owner":"alice","builtin":false,"spec":{...}}]}
api /api/v1/secrets/hf_token
# {"name":"hf_token","owner":"alice","builtin":false,"spec":{"fields":{"value":{...}},"inject":[...]}}
api /api/v1/secrets/hf_token -X DELETE
```

The body is the same fields as the TOML in [Usage](usage.md#secrets), written as JSON.

```json
{"env": ["HF_TOKEN"], "placeholder": "hf_{id}", "value": "hf_...",
 "inject": [{"hosts": ["huggingface.co"], "header": "authorization"}]}
```

Pass values from a file or standard input (`-d @value.json`) so they don't end up in your shell history.

Writing any part of the definition (environment variables, placeholder, rules, files) replaces the definition.
In that case, also write the values for every field (for a secret with several fields, even when changing values, write both the definitions and values in `fields`).
You cannot change only the definition while keeping the registered values.
Changing injection destinations requires someone who knows the value to enter it again, so even a hijacked web UI session cannot send a registered real value to other destinations.
`entries` returns names and definitions (excluding the GitHub user token `github_user_token`).
`can_manage_secrets` in `/api/v1/owners` indicates whether you can change that owner's secrets.

```sh
api /api/v1/secret-templates
# [{"name":"anthropic_api_key","spec":{"fields":{"value":{"env":["ANTHROPIC_API_KEY"],"placeholder":"sk-ant-api03-{id}"}},"inject":[...]}}, ...]
```

### User, organization, and team settings

The settings fields are the same as in [Configuration](configuration.md#user-organization-and-team-settings), written as JSON.
Read and write your own user settings at `/api/v1/policy` (you can change them only if the gateway enables `allow_personal_rules`).

```sh
api /api/v1/policies/acme -X PUT \
  -d '{"rules": [{"hosts": ["registry.example.com"], "action": "allow"}], "root": "persistent"}'
# {"updated_runners":2}   (number of running runners the settings were sent to)

api /api/v1/policies/acme
# {"rules":[{"hosts":["registry.example.com"],"action":"allow"}],"root":"persistent"}

api /api/v1/policies/acme/dev -X PUT \
  -d '{"github": {"repositories": ["web"], "permissions": {"contents": "read"}}}'

api /api/v1/policy -X PUT -d '{"rules": [{"hosts": ["example.com"], "action": "allow"}]}'
```

As with workspace rules, `GET` returns the settings version in `ETag`; if you pass that version to `PUT` in `If-Match` and the settings changed after you read them, it returns 412 ([Workspace rules](#workspace-rules)).

`GET /api/v1/policy-scopes` returns the configuration layers you can view, in the order gateway, user, organizations, then their teams.
A layer (`scope`) is written `gateway`, `user:alice`, `org:acme`, or `team:acme/dev`.
`GET /api/v1/policy-rules?scope=` returns the rules in effect at that layer with their sources, plus the layer's own settings in `own` and the version in `version` (the gateway layer's version is `none`, and it cannot be changed).
An owner's layer is visible only to its members and to those who can change its settings; a user layer is visible only to that user.

```sh
api /api/v1/policy-scopes
# [{"scope":"gateway","kind":"gateway","name":"gateway","editable":false,"rules":3,"workspaces":[]},
#  {"scope":"user:alice","kind":"user","name":"alice","editable":false,"rules":0,"workspaces":["default"]},
#  {"scope":"org:acme","kind":"org","name":"acme","editable":true,"rules":1,"workspaces":["acme/web"]}]
api '/api/v1/policy-rules?scope=team:acme/dev'
# {"rules":[...],"own":{"rules":[...],"github":{"repositories":["web"]}},"editable":true,"version":"9a1b2c3d4e5f6071"}
```

### Workspace rules

Write only the rules, as JSON ([Configuration](configuration.md#workspace-rules)).

```sh
api /api/v1/workspace-policies/acme/web -X PUT \
  -d '{"rules": [{"hosts": ["staging.example.com"], "action": "allow"}]}'
# {"updated_runners":1}

api /api/v1/workspace-policies/acme/web
api /api/v1/workspace-policies/acme/web -X DELETE
```

`GET /api/v1/workspace-policies/{name}` returns the rules' version in `ETag`.
If you pass that version to `PUT` in `If-Match` and the rules changed after you read them (someone else saved them, or approving a network ask added a rule), it returns 412 without replacing them.
Without `If-Match`, it replaces them unchecked.
When there are no rules, the version is `none`.

```sh
api -i /api/v1/workspace-policies/acme/web | grep -i etag
# etag: "3f9c0d1e2a4b5c6d"
api /api/v1/workspace-policies/acme/web -X PUT -H 'If-Match: "3f9c0d1e2a4b5c6d"' \
  -d '{"rules": [{"hosts": ["staging.example.com"], "action": "deny"}]}'
# if they changed: {"error":"the rules of acme/web changed after you read them; read them again"}   (412)
```

`GET /api/v1/workspace-rules/{name}` returns the rules in effect for the workspace in evaluation order (from the top layer), each with its source.
The source (`source.kind`) is the gateway configuration (`gateway`), user, organization, or team settings (`owner`, with the owner in `source.owner`), the workspace rules (`workspace`), or rules added for secret injection (`secret`).
`own` is the workspace's own rules, `version` is their version (pass it in `If-Match`), and `editable` is whether the user can change the workspace rules.

```sh
api /api/v1/workspace-rules/acme/web
# {"rules":[{"source":{"kind":"gateway"},"rule":{"hosts":["github.com"],"action":"allow"}},
#           {"source":{"kind":"owner","owner":"acme"},"rule":{"hosts":["registry.example.com"],"action":"allow"}},
#           {"source":{"kind":"workspace"},"rule":{"hosts":["staging.example.com"],"action":"allow"}}],
#  "own":[{"hosts":["staging.example.com"],"action":"allow"}],"editable":true,"version":"3f9c0d1e2a4b5c6d"}
```

### HTTP request log

Returns the tail of the workspace's HTTP request log as an array, oldest first.
The fields match the lines `chloe-egress` records, and unknown fields may be added.

```sh
api '/api/v1/workspace-requests/acme/web?lines=2'
# [{"ts":1792152059.1,"event":"passthrough","host":"registry.npmjs.org"},
#  {"ts":1792152060.4,"event":"request","host":"api.anthropic.com","sni":"api.anthropic.com",
#   "method":"POST","path":"/v1/messages","query_keys":[],"http_version":"HTTP/2.0","status":200,
#   "request_bytes":1834,"body_scanned":true,"error":null,"decision":"allowed",
#   "injected":["anthropic_api_key-0"],"response_bytes":5120,"duration_ms":2310}]
```

### Asks from agents

The body contains exactly one of `allow`, `deny`, or `answer`.
`allow` is for network asks, `answer` is for questions, and `deny` works for both.
`can_allow` indicates whether the token's user can approve network asks.

```sh
api /api/v1/asks
# [{"workspace":"acme/web","id":12,"kind":"network","host":"docs.example.com","reason":"read the API spec",
#   "status":"pending","created_at":1792152059,"expires_at":1792238459,"can_allow":true}]
api /api/v1/asks/12/answer -X POST -d '{"allow": true}'
api /api/v1/asks/13/answer -X POST -d '{"answer": "dev"}'
api /api/v1/asks/14/answer -X POST -d '{"deny": true, "message": "not needed right now"}'
# answering an already-answered ask returns 409
```

`/api/v1/events` streams events over SSE when an ask is created, answered, or withdrawn.
You receive only events for workspaces you can access.
The event name is `ask`; if you miss events, you receive `lagged`, so re-read the list.
The stream closes after 15 minutes, so reconnect (the token or session is re-checked at that point).

```sh
curl -sN -H "Authorization: Bearer $CHLOE_TOKEN" https://chloe.example.com:8080/api/v1/events
# event: ask
# data: {"type":"created","id":15,"workspace":"acme/web"}
```

### Terminal

`/api/v1/workspace-terminal/{name}` upgrades to WebSocket and opens a shell in the workspace (equivalent to `ssh -t` over SSH).
If the workspace is stopped, it starts it, and it does not stop the workspace while the terminal is open.
Closing the connection ends the shell.
`cols` and `rows` set the initial terminal size (80 and 24 by default), and the terminal type is `xterm-256color`.

- Binary frames are the raw terminal input and output.
- Text frames are JSON. The only frame you send is a resize (`{"type":"resize","cols":120,"rows":40}`); you receive the following four.

| Frame | Meaning |
| --- | --- |
| `{"type":"starting"}` | Preparing the workspace (starts the VM if it is stopped; takes about a minute) |
| `{"type":"started"}` | The shell has started |
| `{"type":"exit","code":0}` | The shell has exited. The connection closes next |
| `{"type":"error","message":"..."}` | The shell could not be opened or could not continue. The connection closes next |

While the terminal is open, the token or session is re-checked every minute; if it has been revoked, the server sends `error` and closes.
Workspaces you cannot access are rejected with 404 before the upgrade.

```sh
websocat -H "Authorization: Bearer $CHLOE_TOKEN" --binary \
  'wss://chloe.example.com:8080/api/v1/workspace-terminal/acme%2Fweb?cols=120&rows=40'
```

### Published ports

For a port published inside the VM with `chloe-ask port publish`, opening `url` in a browser goes through the web UI login and reaches the server in the VM ([Usage](usage.md#open-servers-in-the-vm-from-a-browser)).
Ports can be published only from inside the VM; the admin API can only list and unpublish them.

```sh
api /api/v1/workspace-ports/acme%2Fweb
# [{"port":5173,"scheme":"http","url":"https://k3x7q2m5vbtd.chloe-preview.example.net/","created_at":1792152059}]
api '/api/v1/workspace-ports/acme%2Fweb?port=5173' -X DELETE   # 204
```

The proxy host (the host in `url`) accepts the same token as the admin API in `Authorization: Bearer`.
The token is not passed on to the server in the VM.

```sh
curl -H "Authorization: Bearer $CHLOE_TOKEN" https://k3x7q2m5vbtd.chloe-preview.example.net/
```

When you open it in a browser, the proxy host redirects to the web UI's `/api/auth/proxy`, which uses the web UI session to create a one-time code and redirects back to the proxy host's `/__chloe/auth`.
The proxy host exchanges the code for a cookie (`chloe_proxy`) scoped to that host only.
This cookie is bound to the web UI session and stops working when you log out or run `chloe web revoke`.
If you are not logged in to the web UI, you are sent back to the port you tried to open after logging in (`/api/auth/continue`).

### Tokens

```sh
api /api/v1/tokens -X POST -d '{"name": "deploy", "expires": "7d"}'
# {"id":3,"token":"chloe_..."}   (201; the value is returned only this once)
api /api/v1/tokens
api /api/v1/tokens/3 -X DELETE
```

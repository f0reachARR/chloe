# Configuration

Configuration files are written in TOML.
Unknown keys cause loading to fail (so that typos get noticed).
Relative paths are resolved from the directory the process was started in.
When running under systemd, use absolute paths everywhere.

| File | Read by | Example |
| --- | --- | --- |
| Gateway configuration | `chloe-gateway --config` | `deploy/gateway.toml` (for development, `images/guest/dev/gateway.example.toml`) |
| `chloe-node` configuration | `chloe-node run --config` | `deploy/node.toml` |
| GitHub App configuration | `github_app` in the gateway configuration | [Setting up the GitHub App](github-app.md) |
| User, organization, and team settings and workspace rules | `chloe policy set`, `chloe ws policy set`, the admin API, the admin TUI | [User, organization, and team settings](#user-organization-and-team-settings) and [Workspace rules](#workspace-rules) below |

## Gateway

| Key | Default | Description |
| --- | --- | --- |
| `listen` | (required) | SSH listen address (e.g. `0.0.0.0:22`) |
| `host_key` | (required) | SSH host key (OpenSSH format). Created if missing |
| `database` | (required) | SQLite database. Created if missing |
| `master_key` | (required) | Key that encrypts secrets. Created if missing |
| `audit_log` | none | File the audit log (JSON Lines, mode 600) is appended to |
| `github_url` | `https://github.com` | Where public keys are fetched from (`{github_url}/{login}.keys`) |
| `ssh_user` | `chloe` | Shared SSH user name anyone can use. With this name, or a name that does not exist on GitHub, the user is determined by a linked public key or by Device Flow (requires `github_app`) |
| `github_app` | none | Path to the GitHub App configuration |
| `allowed_users` | `[]` | GitHub logins allowed to log in, in addition to members of the GitHub App's organizations |
| `admins` | `[]` | Server operators. They can change any organization's and team's settings and secrets |
| `memberships` | `{}` | Users who may enter shared workspaces, in addition to their GitHub memberships (for development) |
| `idle_timeout_secs` | `1800` | Seconds after the last connection closes before the VM is stopped. Users can change it per workspace with `chloe ws idle` |
| `activity_poll_secs` | `60` | While an agent is busy, how often (in seconds) to ask again whether it is busy |
| `max_busy_wait_secs` | `43200` | Stop the VM after this many seconds even if an agent is busy |
| `reconcile_interval_secs` | `60` | How often (in seconds) to reconcile running runners with the records |
| `[github_token]` | for github.com | How the GitHub token is passed to the VM (see below) |
| `[policy]` | empty | Allowlist and rules shared by all workspaces |
| `[machine]` | see below | Default vCPU count and memory for workspace VMs |
| `[disks]` | see below | Default and maximum workspace disk sizes |
| `[runner]` | — | Place runners in Docker on the same host. Write either this or `[cluster]` |
| `[cluster]` | — | Place runners on multiple hosts (`chloe-node`) |
| `[api]` | none | HTTP(S) admin API |
| `[ban]` | see below | Temporarily refuse sources with many connections that fail authentication |

`memberships` lists logins per owner.

```toml
[memberships]
"org:acme" = ["alice"]
"team:acme/dev" = ["alice", "bob"]
```

### `[github_token]`

Defines which environment variables in the VM receive the GitHub token (`github_token`), and which header for which destinations it is injected into.
The format is the [secret definition](usage.md#secrets) without the value.
Only one field (`value`) is allowed (the value comes from the GitHub App or the registered token).
If omitted, the following definition is used.

```toml
[github_token]
env = ["GH_TOKEN", "GITHUB_TOKEN"]
placeholder = "ghu_{id}"

[[github_token.inject]]
hosts = ["github.com"]
header = "authorization"
format = "basic-password"          # git over HTTPS (Basic auth password)

[[github_token.inject]]
hosts = ["api.github.com", "uploads.github.com"]
header = "authorization"
```

To target GitHub Enterprise Server, change `hosts` to that server's hosts.
Set `api_url` and `web_url` in the GitHub App configuration to the same hosts.

### `[policy]`

The rules for outbound traffic from VMs, shared by all workspaces.
They are written as rules on destination, method, and path (`[[policy.rules]]`).

| Key | Default | Description |
| --- | --- | --- |
| `allow_personal_rules` | `false` | Let users change their own user settings and their personal workspaces' rules ([Configuration layers](#configuration-layers)). Only when enabled can users also approve network access requested by agents in their personal workspaces |

Rules have the following keys.

| Key | Default | Description |
| --- | --- | --- |
| `name` | none | Name recorded in logs |
| `hosts` | (required) | Destinations |
| `methods` | all methods | Methods |
| `paths` | all paths | Paths |
| `action` | (required) | `allow`, `deny`, or `passthrough` (relay without decrypting) |

Rules are evaluated top to bottom, and the first matching rule decides.
If no rule matches, the request is blocked.
So an `allow` rule restricted to certain paths does not allow other paths on that destination.

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

Destinations are written as host names.
`*.example.com` matches subdomains of example.com, but not example.com itself.
The VM's DNS forwards upstream only names that appear in `allow` and `passthrough` rules (for DNS, `*.example.com` also forwards example.com itself).
For any other name, it answers with the proxy's address without asking upstream; HTTP(S) to it is refused by the proxy with 403.
This prevents exfiltrating data through DNS queries.
Single-label names and names under `.local`, `.internal`, `.lan`, and `.home.arpa` cannot be resolved.

`passthrough` is for clients that pin certificates.
Since traffic is not decrypted, neither rules nor injection apply.
`methods` and `paths` cannot be set.
Its destinations also must not overlap with those of other rules (they overlap if some name matches both, as with `*.docker.com` and `hub.docker.com`).
Passthrough is decided at connection time, so on overlap the other rule would never take effect, regardless of order.
The gateway does not start if there is an overlap.

Destinations used for secret injection and signing (`api.anthropic.com`, or the `hosts` of a registered definition) are appended as a final `allow` rule, only in workspaces that use that secret.
However, a destination is not appended if it overlaps with a destination already written in some rule (`*.amazonaws.com` is not appended if a rule has `s3.amazonaws.com`).
Appending it would allow other paths on a destination you allowed only for certain paths.
For destinations written in rules, only those rules decide whether traffic is allowed.

Paths are globs.
`*` matches a string not containing `/`, and `**` as a whole path segment matches zero or more segments.
Paths are normalized before matching (percent-encoding decoded, `//`, `.`, and `..` resolved), and matching is case-insensitive.
The query string is not part of matching.

### `[runner]`

Starts a runner per workspace on the same host.
`kind` selects how runners are run.

| `kind` | How runners are run |
| --- | --- |
| `docker` (default) | Starts the runner container (`chloe-dev-runner`) as a privileged Docker container. The user running the gateway must be in the `docker` group |
| `systemd` | Starts the runner as a systemd unit (`chloe-runner@ws-<number>.service`). Only network setup runs as root; the user running the gateway needs no root privileges ([Installation](install.md#runner-and-host-networking)) |

With `kind = "docker"`, the keys are as follows.

| Key | Default | Description |
| --- | --- | --- |
| `guest_dir` | (required) | Directory with the kernel (`kernel/vmlinux`) and root filesystem (`rootfs.ext4`) |
| `egress_bin` | (required) | `chloe-egress` binary passed into the container |
| `state_root` | (required) | Directory for per-workspace state (disks, logs, CA) |
| `image` | `chloe-dev-runner` | Runner container image |
| `name_prefix` | `chloe-` | Container name prefix. Use different prefixes when running multiple gateways on the same host |

With `kind = "systemd"`, the keys are as follows.
Paths are read from the same file the runner unit reads (`env_file`).

| Key | Default | Description |
| --- | --- | --- |
| `env_file` | `/etc/chloe/runner.env` | Runner configuration file |

`env_file` consists of `KEY=VALUE` lines (see `deploy/runner.env` for an example).
The parts of the runner unit that run as root also read it, so make it writable only by root.

| Key | Description |
| --- | --- |
| `CHLOE_STATE_ROOT` | Directory for per-workspace state (absolute path). Must be writable by the user running the gateway |
| `CHLOE_GUEST_DIR` | Directory with the kernel and root filesystem |
| `CHLOE_EGRESS_BIN` | `chloe-egress` binary |
| `CHLOE_NET_POOL` | Addresses for the veth pairs connecting runner network namespaces to the host (default `10.250.0.0/16`). Each runner uses a /30, so a /16 allows up to 16384 runners at once. Choose a range that does not overlap the host's networks |

A host can have only one runner configuration (because the unit reads this file).

Default VM vCPU count and memory are set in `[machine]`, and disk sizes in `[disks]`.
The gateway writes per-workspace values into the runner configuration, so `VCPUS` or `MEM_MIB` in `runner.env` have no effect.

### `[machine]`

Default size of workspace VMs.
Users can change it per workspace with `chloe ws create` and `chloe ws machine`.

| Key | Default | Description |
| --- | --- | --- |
| `vcpus` | `4` | Number of vCPUs (1 to 32) |
| `memory_mib` | `4096` | Memory (MiB, at least 512) |

There is no upper limit.
Firecracker can allocate more CPU and memory to VMs than the host has, so many large VMs running at once can exhaust the host's memory.

### `[disks]`

Sizes (GiB) of the workspace disk and the Docker disk.
Users can grow them per workspace, up to the maximum, with `chloe ws create` and `chloe ws disk`.

| Key | Default | Description |
| --- | --- | --- |
| `workspace_gib` | `20` | Default workspace disk size (`/workspace`, the home directory, the persistent root layer) |
| `docker_gib` | `40` | Default Docker disk size |
| `max_workspace_gib` | `200` | Maximum workspace disk size |
| `max_docker_gib` | `400` | Maximum Docker disk size |

Disks are sparse files, so they consume host storage only for what is used, not their full size.
So workspaces can start even if the number of workspaces times the maximum exceeds host capacity.
However, if users fill their disks, the host's disk can run out.
Choose the maximums based on the host's free space and the number of workspaces.

Workspaces without an explicit size grow on their next start when you raise the default.
Lowering the default does not shrink existing disks.
With `[cluster]`, set these here too (not in the `chloe-node` configuration).
The runner is what grows the disk.
When running runners in Docker, rebuild the runner container image (`chloe-dev-runner`) on every host after updating ([Installation](install.md#run-the-runner-in-docker)).

### `[cluster]`

| Key | Default | Description |
| --- | --- | --- |
| `listen` | (required) | Listen address of the internal API (mTLS) that `chloe-node` connects to |
| `server_names` | (required) | Names put in the internal API's certificate: the host name or IP address `chloe-node` connects to |
| `pki_dir` | (required) | Directory for the CA certificate and key. Created if missing |
| `join_ttl_secs` | `3600` | Lifetime of join tokens created by `node add` |

The control plane's certificate is reissued by the CA on every start (its key is not stored in a file).

### `[api]`

| Key | Default | Description |
| --- | --- | --- |
| `listen` | (required) | Listen address of the admin API and web UI |
| `tls_cert`, `tls_key` | none | TLS certificate and key (PEM). HTTPS if both are set, HTTP if neither is |
| `public_url` | none | URL used to open the web UI in a browser (`https://chloe.example.com`; if you put a reverse proxy in front, its URL). When set, web UI login is enabled. Must not contain a path |
| `web_root` | none | Web UI files (`/usr/share/chloe/web` in the deb). If unset, no UI files are served and only `/api` is handled |
| `session_ttl_secs` | `1209600` (14 days) | Web UI session lifetime |
| `proxy_url` | none | Base for the proxy hosts that open ports published in the VM (`https://preview.example.com`). Each published port gets a host `<name>.<domain>` with a random name. Requires `public_url` too (login goes through the web UI). Must not contain a path, and the host cannot be an IP address |

`public_url` is used for the GitHub login callback URL and to check the `Origin` of requests made with a web UI session.
If it is `https://`, cookies get the `Secure` attribute.

The `proxy_url` domain needs wildcard DNS (`*.preview.example.com`) and a certificate ([Installation](install.md#open-servers-running-in-the-vm)).
Agents can rewrite pages served from the VM, so we recommend using a different registrable domain than `public_url` (e.g. `chloe.example.com` and `chloe-preview.example.net`).
If they are the same site, pages from the VM could overwrite the web UI's cookies.

### `[ban]`

SSH connections that end without passing authentication are counted per source.
A failed authentication and a connection closed without doing anything each count as one.
A source whose count reaches `max_failures` within `window_secs` is disconnected before SSH starts, for `ban_secs`.
Successful authentication resets the count.
IPv4 is counted per address, IPv6 per /64.

| Key | Default | Description |
| --- | --- | --- |
| `max_failures` | `10` | Count at which a source is banned. `0` disables banning |
| `window_secs` | `600` | Counting window in seconds |
| `ban_secs` | `600` | Ban duration in seconds |
| `ignore` | `["127.0.0.0/8", "::1"]` | Sources not counted (addresses or CIDRs) |

Bans are recorded in the audit log. Ban state is kept only in memory and is cleared when the gateway restarts.
Load balancer health checks and monitoring disconnect without starting SSH, so they count as failures; list their sources in `ignore`.
Setting `ignore` replaces the default, so include loopback too if you still want it excluded.
Behind a load balancer that rewrites the source address (without the PROXY protocol), all connections appear to come from the same source, so set `max_failures = 0`.

## `chloe-node`

| Key | Description |
| --- | --- |
| `listen` | Listen address of the API (mTLS) the control plane connects to. Use the same port as in the `node add` URL |
| `credentials` | Directory with the certificate and key received from `chloe-node join` |
| `control_plane` | URL of the control plane's internal API (where real-value lookups are relayed) |
| `[runner]` | Same as the gateway's `[runner]` |

## GitHub App

| Key | Default | Description |
| --- | --- | --- |
| `app_id` | (required) | App ID |
| `client_id` | (required) | Client ID (used for Device Flow) |
| `private_key` | (required) | Reference to the private key (`file:/path/to/key.pem`, `env:NAME`, `op://vault/item/field`) |
| `client_secret` | none | Reference to the client secret. When set, users can log in to the web UI with GitHub ([Setting up the GitHub App](github-app.md#information-to-note-after-registration)) |
| `organizations` | none | Restrict which organizations are used. If unset, every organization the app is installed in is used |
| `organization` | none | Shorthand for `organizations` with a single entry |
| `api_url`, `web_url` | `https://api.github.com`, `https://github.com` | GitHub API and web URLs |

If you make the app public (Any account), anyone can install it in their own organization, and its members can then log in to chloe.
In that case, restrict `organizations` to the organizations allowed to use it.

## User, organization, and team settings

Pass TOML on standard input to `chloe policy set acme` (organization) or `chloe policy set acme/dev` (team).
If you omit the owner, as in `chloe policy set`, you set your own user settings.
The admin API takes the same keys as JSON.
In the admin TUI, rules are edited in the Rules tab and everything else in the Settings tab.

| Owner | Who can change it |
| --- | --- |
| Organization, team | Users who are administrators of that organization on GitHub, and the gateway configuration's `admins` |
| User | The user themselves, only when the gateway's `[policy]` has `allow_personal_rules = true` |

| Key | Description |
| --- | --- |
| `rules` | Additional rules (same format as the gateway's `[[policy.rules]]`; `passthrough` is not allowed) |
| `root` | Default root handling for newly created workspaces (`ephemeral` or `persistent`) |
| `[github]` | Scope of the GitHub installation token passed to shared workspaces (not allowed in user settings) |

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
repositories = ["web", "api"]                  # omit for all repositories in the installation
permissions = { contents = "write", pull_requests = "write" }   # omit for all of the installation's permissions
```

User settings are layered only onto workspaces that user owns (personal workspaces).
They are not layered onto organization or team workspaces the user enters.
This keeps a shared runner's rules from changing depending on who enters it.
Personal workspaces use the user's own GitHub token, so `[github]` is not allowed.

## Workspace rules

Pass only rules, as TOML on standard input, to `chloe ws policy set acme/web`.
In the admin API, send JSON to `/api/v1/workspace-policies/{name}`.
In the admin TUI, select the workspace in the Rules tab and edit it.

```toml
[[rules]]
hosts = ["staging.example.com"]
action = "allow"
```

Only `rules` can be set.
`root` belongs to the workspace itself, so change it with `chloe ws root`.
`[github]` takes precedence in lower layers, so allowing it per workspace would let a workspace widen a scope its owner's settings narrowed.
Deleting a workspace also deletes its rules.

Whoever can change the secrets of the workspace's owner can change its rules.

| Workspace | Who can change it |
| --- | --- |
| Personal | The user themselves, only when the gateway's `[policy]` has `allow_personal_rules = true` |
| Organization | Organization administrators and the gateway configuration's `admins` |
| Team | Organization administrators, team maintainers, and the gateway configuration's `admins` |

## Configuration layers

A workspace's policy is built by layering the following, in order.

| Workspace | Layer order |
| --- | --- |
| Personal | Gateway `[policy]`, user settings, workspace rules |
| Organization | Gateway `[policy]`, organization settings, workspace rules |
| Team | Gateway `[policy]`, organization settings, team settings, workspace rules |

- **Rules**: rules from upper layers come first. Since the first matching rule decides, lower layers cannot override upper layers' rules (organization settings cannot override the server operator's rules).
- **`root` and `[github]`**: the value from the lowest layer that sets it is used (workspace rules have neither).

So all a lower layer can do is allow or deny destinations that no upper-layer rule matches.
A lower layer's `deny` cannot block a destination an upper layer allowed.

When `allow_personal_rules` is not enabled, user settings and personal workspace rules are not layered (they have no effect even if saved).

`passthrough` rules can only be written in the gateway's `[policy]`.
Rules do not apply to destinations that are not decrypted, so allowing them elsewhere would let lower layers create destinations that bypass upper layers' rules.
Rules whose destinations overlap the gateway's `passthrough` rules are also not allowed (for the same reason as in [`[policy]`](#policy)).
If a later change to the gateway configuration makes it overlap with saved settings, that workspace's runner is not started.

Secret destinations are appended last only if no rule in any layer covers them (see [`[policy]`](#policy)).
So if a layer sets `api.anthropic.com` to `deny`, injection for that secret also stops working in workspaces that use that layer.

The admin TUI's Rules tab shows the rules in effect for the selected scope, in evaluation order, along with the name of each layer.

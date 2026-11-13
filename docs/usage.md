# Usage

This guide is for people who use chloe by logging in over SSH.
To set up a server, see [Installation](install.md).

## Connect

Log in with the username `chloe` (everyone uses the same username, just like `git@github.com`).
The first time you log in, SSH asks you to sign in to GitHub.
Press Enter and a Device Flow code appears.
Enter the code at `https://github.com/login/device`, approve it, and press Enter again.
Device Flow runs only once per connection. If it fails, or you approved with a different account, reconnect.
The GitHub account you approved with becomes your chloe user.

```sh
ssh chloe@chloe.example.com
```

The public key you used when you approved is bound to that user.
From then on, you can log in with the same key without Device Flow.
If you have no key, you go through Device Flow every time.
If ssh-agent holds several keys, the first key the client offers is the one that gets bound (pin the key with `-i` and `IdentitiesOnly yes`).

You can also log in with your GitHub login as the username (`ssh alice@chloe.example.com`).
This works with a public key registered on GitHub or a bound key, but you must complete Device Flow at least once.
Keys are bound only when you log in as `chloe`. If you log in as `alice` with a key that is not registered on GitHub, you go through Device Flow every time.
If your GitHub token expires or you delete it with `chloe github logout`, the next connection goes through Device Flow again.
A username that does not exist on GitHub is treated the same as `chloe`.

You can put the workspace to enter after a `+` in the username.
Without it, you enter your personal workspace `default` (it is created if it does not exist).
However, when you open a shell with a terminal allocated (that is, when you pass no command to `ssh`) and you can enter workspaces other than `default`, a workspace picker appears.
Choose with ↑↓ and press Enter to enter; q or Esc closes it.
m opens the admin TUI (the same one `ssh -t chloe@chloe.example.com chloe` opens); closing it with q returns you to the picker.
Workspaces you create in the admin TUI show up in the picker when you return.
The workspace you pick is also used for later shells and port forwarding on the same connection.
When you pass a command, or for a port-forwarding-only connection that opens no shell (`ssh -N -L`), the picker does not appear and you enter `default`.

| Username | Workspace you enter |
| --- | --- |
| `chloe` or `alice` | Your personal workspace `default` (in a terminal, the picker may appear) |
| `chloe+api` | Your personal workspace `api` |
| `chloe+acme/web` | Shared workspace `web` of organization acme |
| `chloe+acme/dev/tools` | Shared workspace `tools` of team dev in acme |

Workspaces other than `default` must be created first with `chloe ws create`.
If you pass a command, it runs in that workspace and the session ends (`ssh chloe@chloe.example.com 'git status'`).
`chloe whoami` shows who you are logged in as, and `chloe key list` shows your bound keys.
If you lose a key, remove the binding with `chloe key rm <fingerprint>`.

If the server has no GitHub App configured, Device Flow is unavailable.
Log in with your GitHub login as the username and a public key registered on GitHub.

A stopped workspace starts when you connect (this takes a few seconds).

## Inside the VM

Each workspace has one VM.

- **OS and user**: Ubuntu 26.04. The user is `user`, who can use `sudo`.
- **Hostname**: Set to the workspace name and shown in the prompt (`acme/dev/tools` becomes `acme-dev-tools`). The exact name is in the environment variable `CHLOE_WORKSPACE`.
- **Preinstalled**: Docker, [mise](https://mise.jdx.dev/), git, Python 3, curl, tmux, and so on. Tools such as Claude Code and Codex are not installed; install them with mise (see [Install tools](#install-tools)).
- **What persists**: `/workspace` and the home directory (`/home/user`) live on the workspace disk and survive VM stops. Docker images and container data also persist, on a separate disk.
- **Disk sizes**: By default the workspace disk is 20GiB and the Docker disk is 40GiB (configurable in the gateway settings). If you run short, grow them with something like `chloe ws disk <name> --workspace 50G` (see [Grow the disks](#grow-the-disks)).
- **What does not persist**: By default, changes to the root filesystem (such as packages installed with `apt install`) are lost when the VM stops. To keep them, set the workspace's root to `persistent` (`chloe ws root <name> persistent`).
- **git**: Connects to GitHub over HTTPS. SSH remotes of the form `git@github.com:` are rewritten to HTTPS too. Authentication happens automatically if `GH_TOKEN` is set (see [Use GitHub](#use-github)).

## Install tools

Install coding agents and language toolchains inside the VM with mise.
What you install goes in the home directory (`~/.local/share/mise`), which is on the workspace disk, so it survives VM stops.

```sh
mise use -g jq            # use in every directory (written to ~/.config/mise/config.toml)
mise use node@24          # use in this directory (written to mise.toml)
```

Tools installed with mise are available both in an interactive terminal and when you run `ssh <host> '<command>'`.
A `mise.toml` written by someone else (for example, one checked into a repository) is not read until you trust it with `mise trust`.

The destinations mise fetches tools from are subject to the outbound traffic rules too.
The default rules allow GitHub (`github.com`, `*.githubusercontent.com`), mise's version list (`mise-versions.jdx.dev`), npm, PyPI, and crates.io.
For tools fetched from other destinations (such as `nodejs.org` for Node.js), add the destination to the rules before installing (see [Outbound traffic](#outbound-traffic)).
If `GITHUB_TOKEN` is set, calls to the GitHub API are authenticated, so you are less likely to hit rate limits.

Claude Code and Codex come preconfigured with hooks that tell the VM when they are busy (`/etc/claude-code/managed-settings.d/`, `/etc/codex/hooks.json`).
If you install them with mise, the VM is not stopped while they are busy, with no extra setup (see [When the VM stops](#when-the-vm-stops)).

## When the VM stops

The VM is stopped once a set amount of time has passed since the last connection closed (30 minutes by default; configurable in the gateway settings).
While Claude Code or Codex is busy, the VM is not stopped; it is stopped after the work finishes (by default it waits up to 12 hours).
The VM is also not stopped while an SSH connection with port forwarding is open.

Stopping the VM ends the processes that were running inside it (development servers, tmux sessions).
Files persist as described in [Inside the VM](#inside-the-vm).

To stop it right away, or to restart it after changing the root mode, the disks, or the VM size, stop it with `chloe ws stop <name>`.

You can change the idle timeout per workspace with `chloe ws idle`.
Write it like `30m`, `2h`, or `1d`; `never` keeps the VM running even with no connections, and `default` reverts to the time in the gateway settings.

```sh
ssh chloe@chloe.example.com chloe ws idle api 2h
ssh chloe@chloe.example.com chloe ws idle api never   # keep running until stopped with chloe ws stop
ssh chloe@chloe.example.com chloe ws idle api         # show the current timeout
```

If the VM is running with no connections, the countdown restarts from the moment you change it.

## Resize the VM

`chloe ws machine` changes the VM's vCPU count and memory.
vCPUs range from 1 to 32; memory is written like `8G` or `512M` (512M or more).
Whichever one you omit stays unchanged.
The default is 4 vCPUs and 4GiB of memory (configurable in the gateway settings).

```sh
ssh chloe@chloe.example.com chloe ws machine api --cpus 8 --memory 16G
ssh chloe@chloe.example.com chloe ws machine api     # show the current size
```

As with disks, the new size takes effect the next time the VM starts.
To apply it right away, stop the VM with `chloe ws stop <name>` and reconnect.
To set it at creation time, write something like `chloe ws create <name> --cpus 8 --memory 16G --idle 2h`.

## Grow the disks

`chloe ws disk` grows the workspace disk and the Docker disk.
Sizes are in GiB (`50G`, `50GiB`, and `50` all work).
A disk you omit stays unchanged.

```sh
ssh chloe@chloe.example.com chloe ws disk api --workspace 50G --docker 80G
ssh chloe@chloe.example.com chloe ws disk api     # show the current sizes
```

The new size takes effect the next time the VM starts.
This is because a disk cannot be grown while the VM is running (before boot, the filesystem is checked and then grown).
To grow it right away, stop the VM with `chloe ws stop <name>` and reconnect.

Disks can only grow, never shrink.
Shrinking ext4 takes time and fails if the contents do not fit.
To start over, delete the workspace together with its disks using `chloe ws rm <name> --disks`, then create it again.
The maximum size is set in the gateway settings (`[disks]`).
To set the size at creation time, write something like `chloe ws create <name> --workspace 50G`.

Changes kept on a persistent root (`persistent`) also go on the workspace disk.
If you install a lot with `apt install`, give the workspace disk some headroom.

## Secrets

API keys and GitHub tokens never enter the VM.
Environment variables inside the VM hold placeholders instead of the real values.
The proxy that relays the VM's traffic replaces placeholders with real values only in specific headers to specific destinations.
Programs and agents inside the VM can neither read the real values nor send them to other destinations.

You register a secret by passing its definition and value as TOML on standard input.
The definition specifies the VM environment variables that receive the placeholder, the placeholder's format, and the destinations and location where it is injected.
The SSH connection does not pass through the VM, so the value never reaches the VM.

```toml
# hf.toml
env = ["HF_TOKEN"]             # VM environment variables that receive the placeholder
placeholder = "hf_{id}"        # placeholder format; {id} is replaced with a per-workspace random string
value = "hf_..."               # the real value

[[inject]]
hosts = ["huggingface.co", "*.huggingface.co"]
header = "authorization"       # replace the placeholder in this header with the real value
# format = "basic-password"    # to inject it as a Basic auth password
```

```sh
ssh chloe@chloe.example.com chloe secret set hf_token --spec < hf.toml
ssh chloe@chloe.example.com chloe secret set hf_token < new-value.txt   # change only the value (definition unchanged)
ssh chloe@chloe.example.com chloe secret list
ssh chloe@chloe.example.com chloe secret show hf_token   # show the definition (the value is not shown)
ssh chloe@chloe.example.com chloe secret rm hf_token
```

`deploy/secrets/` has example definitions for common secrets.
Replace the `value` line with the real value and register it.

| Example | Environment variables | Injected for |
| --- | --- | --- |
| `anthropic_api_key.toml` | `ANTHROPIC_API_KEY` | `api.anthropic.com` |
| `claude_oauth_token.toml` | `CLAUDE_CODE_OAUTH_TOKEN` | `api.anthropic.com` |
| `openai_api_key.toml` | `OPENAI_API_KEY`, `CODEX_API_KEY` | `api.openai.com` |
| `aws.toml` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | `*.amazonaws.com` ([signing](#signing-schemes-aws)) |
| `apt.toml` | None (a [file](#tools-that-dont-read-environment-variables) in `/etc/apt/auth.conf.d/`) | apt repositories that require authentication |

```sh
ssh chloe@chloe.example.com chloe secret set anthropic_api_key --spec < anthropic_api_key.toml
```

A registered value is used from the next request on.
In a running workspace, shells opened after that get the new placeholder (the environment of shells that are already open does not change).

- **Name**: Starts with a lowercase letter and uses lowercase letters, digits, and `_` (no consecutive `_`, and no trailing `_`).
- **placeholder**: Must contain exactly one `{id}`. The other allowed characters are alphanumerics and `-_.~+/=:`. If omitted, it is just `{id}`. Some tools check the format locally, so it helps to match the real value's prefix.
- **inject**: On requests to `hosts`, replaces the placeholder in `header` with the real value. `hosts` can match subdomains with the `*.example.com` form. With `format = "basic-password"`, the Basic auth username can also be made a field and injected (see [Inject the username too](#inject-the-username-too)).
- The rule's destinations are automatically allowed in workspaces that use the secret. However, a destination that overlaps one already listed in the gateway's or organization's rules is governed by those rules.
- If a placeholder is sent to a destination other than the rule's destinations, the proxy blocks the request. Besides headers and the URL, it also checks the body, up to 1 MiB.

To change a definition, pass the definition and value together again with `--spec`.
The GitHub token (`github_token`) is registered as a value only, without a definition (see [Use GitHub](#use-github)).
The operator changes the environment variables and destinations it uses with [`[github_token]`](configuration.md#github_token) in the gateway settings.

### Signing schemes (AWS)

For schemes like AWS, where requests are signed with a key, replacing a placeholder in a header is not enough.
The tool inside the VM signs with the placeholder key, and the proxy re-creates the signature with the real key.
Since there are several values, you write the environment variables, placeholder, and value for each field (`fields`) (see `deploy/secrets/aws.toml` for an example).

```toml
# aws.toml
[fields.access_key_id]
env = ["AWS_ACCESS_KEY_ID"]
placeholder = "AKIA{id}"
value = "AKIA..."

[fields.secret_access_key]
env = ["AWS_SECRET_ACCESS_KEY"]
value = "..."

# For temporary credentials, add a session token field too.
# [fields.session_token]
# env = ["AWS_SESSION_TOKEN"]
# value = "..."

[[sign]]
scheme = "aws-sigv4"
hosts = ["*.amazonaws.com"]
```

```sh
ssh chloe@chloe.example.com chloe secret set aws --spec < aws.toml
```

To change only the values, pass a TOML file with just `value` for each field, using `--spec`.

`aws-sigv4` uses the fields `access_key_id` and `secret_access_key`, and if a `session_token` field exists, it also replaces the `x-amz-security-token` header.
The following requests cannot be re-signed, so the proxy blocks them (the reason in the log is `unsignable`).

- Presigned URLs (signed with `X-Amz-Credential` in the query)
- Requests signed per chunk (such as `x-amz-content-sha256: STREAMING-AWS4-HMAC-SHA256-PAYLOAD`)
- Requests whose body exceeds 1 MiB or has an unknown size and that lack an `x-amz-content-sha256` header (the body hash cannot be computed)

The S3 SDKs send `x-amz-content-sha256`, so even large uploads go through.
`STREAMING-UNSIGNED-PAYLOAD-TRAILER`, which does not sign chunks (the scheme newer SDKs use when uploading with checksums), also goes through.

### Tools that don't read environment variables

For tools like apt that read credentials from a config file, chloe places a file containing the placeholder (`files`) in the VM.
The tool sends the placeholder from the file, and the proxy replaces it with the real value using the same rules as for environment variables.
In the following example, apt sends the password from `auth.conf.d` via Basic auth, so it is injected with `basic-password` (see `deploy/secrets/apt.toml` for an example).

```toml
# apt.toml
value = "..."

[[inject]]
hosts = ["apt.example.com"]
header = "authorization"
format = "basic-password"

[[files]]
path = "/etc/apt/auth.conf.d/chloe-apt.conf"
content = "machine apt.example.com login deploy password {value}\n"
```

- **path**: An absolute path inside the VM. Files cannot be placed under `/run/chloe`, `/proc`, `/sys`, or `/dev`.
- **content**: `{value}` is replaced with the secret's placeholder. For a secret with several fields, use the field name, as in `{access_key_id}`. A `{...}` that is not a field name is left as-is.
- **owner**: `root` (the default) or `user`. Use `user` for files in the home directory (such as `/home/user/.netrc`).
- **mode**: Written in octal (such as `"0600"`). The default is `0644`. A placeholder can only be sent to the registered destinations, so it does not matter who can read it.

In a running workspace, the files are placed again as soon as you register.
If you delete the secret, or the workspace can no longer use it, the placed files are removed too.
However, in a workspace whose root is discarded on every boot (`ephemeral`), stopping the VM also discards the list of placed files.
So a file placed on the workspace disk (`/home/user`, `/workspace`) remains if you delete the secret while the workspace is stopped.

Write apt repositories in `sources.list` with `https://`.
With `http://`, the real password travels in plaintext beyond the proxy (the same goes for any secret injected for an `http://` destination).

### Inject the username too

`basic-password` normally injects only the password and leaves the username as the tool inside the VM sent it.
If you also want to hide the username from the VM, or change it together with the password, create a username field and put that field's name in the rule's `username_field`.
Put the password field in `field`.

```toml
[fields.username]
env = ["REPO_USER"]
placeholder = "user-{id}"
value = "deploy"

[fields.password]
env = ["REPO_PASSWORD"]
value = "..."

[[inject]]
field = "password"
username_field = "username"
hosts = ["repo.example.com"]
header = "authorization"
format = "basic-password"
```

Inside the VM, the username also holds a placeholder (`curl --user "$REPO_USER:$REPO_PASSWORD"`, or `{username}` in a file).
The proxy replaces whichever of the Basic auth username and password is a placeholder with the real value.
A username placeholder sent to a destination other than the rule's destinations is blocked too.
Basic auth separates the username and password with `:`, so the username field's `placeholder` cannot contain `:`.

### Where credentials live

| Credential | Where it lives |
| --- | --- |
| API keys and other registered secrets | Control plane (stored encrypted). The VM only has placeholders |
| GitHub token | Control plane. The VM only has placeholders |
| Logins made inside the VM, such as Codex's ChatGPT login | The VM's home directory |

To use Claude Code with a subscription, run `claude setup-token` on your local machine and register the resulting token with the definition in `deploy/secrets/claude_oauth_token.toml`.
To use Codex with a ChatGPT account, run `codex login` inside the VM (the credentials stay in the VM's home directory).

### Secrets in shared workspaces

Organization and team workspaces use that organization's or team's secrets.
Personal secrets do not enter shared workspaces.
A team workspace uses the organization's secret when the team has not registered one.

Specify the owner with `--owner` (`acme` or `acme/dev`).
Organization secrets can be registered by organization administrators; team secrets by organization administrators and the team's maintainers.
Members can see the list of names and the definitions, but cannot read the values.
When you change only the value of a secret that has a definition, the definition stored under the same owner is used (registering only a value for a team does not inherit the organization's definition).

```sh
ssh chloe@chloe.example.com chloe secret set --owner acme/dev anthropic_api_key --spec < anthropic_api_key.toml
```

## Use GitHub

If the server has a GitHub App configured, you can use GitHub as follows.

- **Personal workspaces**: Use the token obtained through Device Flow at login. From then on, `git` and tools that read `GH_TOKEN` (scripts that call the GitHub API, a `gh` you installed yourself, and so on) can use GitHub with your permissions. The control plane refreshes the token. To get a new one, run `ssh chloe@chloe.example.com chloe github login`.
- **Shared workspaces**: Use the GitHub App's installation token (actions are recorded under the app's name). Organization administrators can restrict the repositories and permissions.

Without a GitHub App, register a personal access token as `github_token`.

## Outbound traffic

The VM can connect out only to destinations allowed by the rules.
HTTP(S) to destinations the rules do not allow is refused by the proxy with 403. The response body explains that you can ask a person for permission with `chloe-ask network` (see [Asks from agents](#asks-from-agents)).
Connections to ports other than 80 and 443 are refused immediately (`Connection refused`).
To add a destination, ask the server operator or an organization administrator (organization and team settings are changed with `chloe policy`; workspace rules with `chloe ws policy` or Policy in the workspace settings of the [web UI](#web-ui)).
A team's maintainers can change the rules of that team's workspaces.
If the server enables it, you can add destinations for your own personal workspaces yourself (`chloe policy set` changes your user settings, and `chloe ws policy set NAME` changes a workspace's rules).
You can only add destinations that are not in the rules of the layers above (gateway or organization) (see [configuration layering](configuration.md#configuration-layers)).

HTTPS traffic is decrypted at the proxy, so the chloe CA certificate is installed inside the VM.
The content (body), header values, and query values are not logged.
For each workspace, chloe logs the destination, method, path, status, verdict (allowed, or blocked with the reason), injected rule, response size, and duration.

View the log with `chloe ws log` or with `l` on the workspaces tab of the admin TUI.
Anyone who can enter the workspace can view it, even while the VM is stopped.
If a destination is refused with 403, you can check the block reason here.

```sh
ssh chloe@chloe.example.com chloe ws log api              # last 200 entries
ssh chloe@chloe.example.com chloe ws log api --blocked    # blocked entries among the last 200
ssh chloe@chloe.example.com chloe ws log api --lines 1000 # last 1000 entries (up to 2000)
```

Each entry is one line, with these tab-separated columns: time (UTC), verdict, status, method, host, path (query keys only), response size, duration, the injected or blocking rule, and error.
Verdicts include `allowed`, `blocked:<reason>`, and `passthrough` (TLS relayed without decryption).
Streaming responses (such as SSE) do not appear in the log until the response finishes.
The log of a shared workspace does not distinguish whose request it was.

## Asks from agents

An agent inside the VM can send an ask to a person with `chloe-ask` and wait for the answer.
There are two kinds of asks: permission to reach a destination not on the allowlist, and questions.
Even with no one at the terminal, the agent does not get stuck when it is blocked or unsure; it waits for the answer and then continues working.

```sh
chloe-ask network docs.example.com --reason "read the API spec"
chloe-ask network api.example.com --method GET --path '/v1/*' --reason "..."
chloe-ask question "Which branch should this go into?" --option main --option dev
```

`chloe-ask` waits until an answer arrives, and exits with 0 if approved or answered, 1 if denied, and 2 if it expires.
The answer to a question is printed on standard output.
With `--no-wait`, it prints the ask ID and exits immediately; you can wait later with `chloe-ask wait ID`.
An ask expires after 24 hours by default (`--timeout` can shorten this).
While an ask is waiting for an answer, the VM is not stopped even with no connections.

The same operations are registered with Claude Code and Codex as MCP tools (`request_network_access`, `ask_user`, `wait_for_answer`).
At login, a `chloe` server is added once to the user's settings (`~/.claude.json` and `~/.codex/config.toml`).
If you remove it from the settings, it is not added again.
A single tool call waits up to 55 minutes; if no answer arrives by then, it returns the ID, and the agent waits again with `wait_for_answer`.
The 403 body returned when the proxy blocks a request also explains that permission can be requested with `chloe-ask network`.

Anyone who can enter the workspace can see its asks, and answers them through the [web UI](#web-ui), the Inbox tab of the admin TUI, `chloe inbox`, or the [admin API](api.md).

```sh
ssh chloe@chloe.example.com chloe inbox                 # asks waiting for an answer (--all includes answered ones)
ssh chloe@chloe.example.com chloe inbox show 12
ssh chloe@chloe.example.com chloe inbox allow 12        # allow the network access
ssh chloe@chloe.example.com chloe inbox deny 12 not needed now  # deny (the message is passed to the agent)
ssh chloe@chloe.example.com chloe inbox answer 13 2     # an option number or answer text
```

Approving a network ask appends a rule allowing that destination (named `ask-<ID>`) to the end of the workspace's rules, and it takes effect immediately in the running VM.
To revoke it, remove it from the workspace's rules.
A destination blocked by the rules of a layer above (gateway, organization, team) stays blocked even if approved.
Only people who can change the workspace's rules can approve, and if the gateway settings do not allow personal rules, network asks from personal workspaces cannot be approved (they can still be denied).
Anyone who can enter the workspace can answer questions.
An ask can be answered only once, and both sending and answering an ask are recorded in the audit log.

## Port forwarding

You can connect from your local machine to servers running inside the VM.

```sh
ssh -N -L 8080:127.0.0.1:3000 chloe@chloe.example.com
# http://localhost:8080 on your machine connects to 127.0.0.1:3000 inside the VM.
```

The target of `-L` is a destination as seen from inside the VM.
It is not a path out of the VM from your machine (it only reaches destinations the VM itself can reach).
Remote forwarding (`-R`) is not supported yet.

## Open servers in the VM from a browser

If the gateway has it enabled ([Installation](install.md#open-servers-running-in-the-vm)), you can open HTTP(S) servers running inside the VM without SSH port forwarding, even from a smartphone browser.
Only ports published inside the VM can be opened.

```sh
chloe-ask port publish 5173            # prints the URL (https://k3x7q2m5vbtd.chloe-preview.example.net/)
chloe-ask port publish 8443 --https    # when the server speaks HTTPS (its certificate is not verified)
chloe-ask port list
chloe-ask port unpublish 5173
```

Opening the URL takes you through the web UI login and then to the server inside the VM.
Only people who can enter the workspace can open it.
With an admin API token in `Authorization: Bearer`, you can also open it from curl and similar tools.
A published port stays published until you unpublish it, and if the VM is stopped, it starts when the URL is opened.
If the server is not running, you get 502.

- **Listening only on `localhost` is fine**: The gateway connects to `localhost:<port>` from inside the VM. You do not need to listen on `0.0.0.0`.
- **`Host` becomes `localhost:<port>`**: So that development servers do not reject an unknown `Host`, it matches what you would see opening the page inside the VM. The original host arrives in `X-Forwarded-Host`. `X-Forwarded-Proto` also reflects how the gateway talks to the server inside the VM (`https` if published with `--https`, otherwise `http`), not the public URL. If the app builds its own URL from `Host`, it will be a `localhost` URL.
- **The user's IP address arrives in `X-Forwarded-For`**: The address of the peer that connected to the gateway is appended at the end. If you put a reverse proxy in front, it follows the value that proxy added. The leading entries can be written by the user, so only count from the end when deciding what to trust.
- **The page's own `Origin` also becomes `localhost:<port>`**: Servers that check `Origin`, such as Vite's HMR WebSocket, work without configuration changes. The `Origin` of requests from other origins arrives unchanged.
- **chloe's authentication does not reach the VM**: The proxy host's cookie (`chloe_proxy`) and an `Authorization` header carrying a chloe token are removed before the request is passed to the server inside the VM. Cookies the app sets itself arrive unchanged.
- **A separate origin per port**: The hostname is a random name (12 characters) per published port and contains neither the workspace nor the port. A page on one port cannot read cookies for other ports or for the web UI. Republishing with a different `--https` keeps the name; unpublishing and then publishing gives a new name (the old URL stops working).
- **Not usable from other sites' pages**: If a script on another port's or another site's page opens a WebSocket with the user's cookie, or sends a non-GET request via a form or `fetch`, it is refused (403). Requests from the page itself and opening the page from a link go through.
- **WebSockets work too**: Development server HMR and the like work.
- **`/__chloe/` never reaches the app**: The proxy host uses `/__chloe/auth` for login.

The same operation is registered with Claude Code and Codex as an MCP tool (`publish_port`).
After having an agent start a server, you can ask it to publish the port and give you the URL.

From outside the VM, you can list and unpublish published ports (links also appear on the workspace card in the web UI).

```sh
ssh chloe@chloe.example.com chloe ws ports acme/web
ssh chloe@chloe.example.com chloe ws unpublish acme/web 5173
```

## Web UI

If the gateway has the web UI enabled ([Installation](install.md#enable-the-web-ui)), you can answer asks from agents, check workspace status, and use workspace terminals from a browser.
It works from a smartphone, without an SSH terminal.
Add it to your home screen (PWA) to open it like an app.

Log in with the GitHub login button or with a one-time URL issued over SSH.

```sh
ssh chloe@chloe.example.com chloe web login     # a URL and QR code usable once within 5 minutes
ssh chloe@chloe.example.com chloe web sessions  # list logged-in sessions
ssh chloe@chloe.example.com chloe web revoke 3  # delete a session
```

- **Inbox**: When an ask arrives that is waiting for an answer, it appears immediately on any open page and notifies you. Network asks are approved or denied; questions are answered with an option or text.
- **Workspaces**: For each workspace, shows whether it is running, whether an agent is busy, and the number of asks waiting for an answer. Busy status is queried from the VM when you open the page and when you reload. Clicking the number of waiting asks opens that workspace's asks.
- **Create a workspace**: In Workspaces, under New workspace, choose the owner (yourself, or an organization or team you belong to), enter a name, and create it. The name to use over SSH (`acme/dev/web`) appears below the field. Setting root to Default follows the owner's settings default, and leaving the Advanced fields empty follows the gateway defaults. Creating a workspace does not start it; it starts when you open a terminal.
- **Secrets**: Choose an owner (yourself, an organization, or a team) to view its list of secrets (environment variables, destinations, placed files). When you choose a team, organization secrets whose names the team lacks are also listed as "from acme" (the team's workspaces use them). People who can make changes use New secret to register a secret from scratch, from a template (the same as in the admin TUI), or the built-in `github_token`, and use Change to update the value, edit the definition, or delete it. Editing a definition lets you change the fields, header injection rules, signing rules, and placed files, but you must re-enter all the values. Values are never shown on screen. Registration takes effect immediately in running workspaces, and new environment variables go into shells opened afterward.
- **Policies**: Lists the configuration layers you can view (gateway, user, organization, team), from the top layer down. Choosing a layer shows the rules of the layers above, and lets you change that layer's rules, the default root mode for new workspaces, and the GitHub token scope (organizations and teams only) (only if you are allowed to change them). Clicking a workspace under a layer opens Policy in that workspace's settings.
- **Published ports**: The workspace card shows links to ports published inside the VM (see [Open servers in the VM from a browser](#open-servers-in-the-vm-from-a-browser)). × unpublishes a port.
- **Terminal**: Terminal on the workspace card opens a shell in that workspace. It is the same shell you get over SSH, and a stopped workspace starts. On a smartphone, the row below the terminal lets you type Esc, Tab, arrows, and Ctrl (which applies to the next character you type).
- **Settings**: Open from Settings on the workspace card, or Settings in the terminal menu (⋮). Machine changes the VM size, root, disks (grow only), and idle timeout, and stops the workspace. Ports opens and unpublishes published ports. Delete removes the workspace after you type its name (this cannot be undone; a running workspace is stopped first, so open terminals close; unless you select "Also delete the disks", the disks remain on the runner host). Policy shows the rules of the layers above (gateway, organization, team) and lets you add, remove, and reorder the workspace's rules (only if you are allowed to change them; otherwise it is shown read-only). If someone else saved or a network ask was approved while you were editing, saving asks whether to load the latest version or overwrite it. Size, root, and disks take effect from the next start, so if the workspace is running when you save, you are asked whether to stop it. A stopped workspace starts when you open a terminal or a port.
- **Working with the terminal open**: Settings and asks open as overlays on top of the terminal, so the shell keeps running when you close them. A button in the terminal header opens and lets you answer that workspace's asks waiting for an answer. Clicking a new-ask notification also opens it on top of the current screen. Restart workspace in the terminal menu stops the workspace and then opens a new shell.

As with SSH, a web terminal's shell ends when you close the page or the connection drops (on a smartphone, switching to another app may also drop it).
Run work you want to keep going in tmux or similar inside the VM.

Sessions are valid for 14 days by default and have the same permissions as SSH.
The web UI cannot create API tokens (so that a malicious script slipping into the page cannot exfiltrate a non-expiring token).
Create tokens with the `chloe` command or the admin TUI.
Secrets can be registered from the web UI, but changing a definition also requires re-entering the values (so that a registered value cannot be redirected to other destinations). To change only the definition while keeping the value, use the admin TUI.

## Admin TUI

Running just `chloe` with a terminal allocated opens the admin TUI.

```sh
ssh -t chloe@chloe.example.com chloe
```

Its tabs cover workspaces, secrets, rules, user, organization, and team settings, admin API tokens, bound SSH keys, and GitHub sign-in.
Secrets and settings are edited in per-field forms, without writing TOML.
Secrets can be created from the examples in `deploy/secrets/` as templates.
When changing the definition of a registered secret, leaving the value field empty keeps the current value.
On the workspaces tab, `g` changes the disk sizes (like `chloe ws disk`, this takes effect from the next start).
`m` changes the VM's vCPU count and memory (from the next start) and the idle timeout.
`u` opens the selected workspace in the Rules tab.
`l` opens the selected workspace's HTTP request log.
Newer entries appear at the bottom, and blocked ones are shown in red.
`Enter` shows all fields of an entry, `f` cycles the filter (all, blocked, injected, passthrough), and `r` reloads.

The Inbox tab shows asks from agents.
Unlike the other tabs, it reloads automatically when asks arrive or are answered, and notes new asks at the bottom of the screen.
The tab name shows the number of asks waiting for an answer.
`Enter` shows all fields of an ask, `a` allows the network access or answers the question, `d` denies, and `f` toggles whether answered asks are shown.

In the Rules tab, choose a scope (gateway, user, organization, team, workspace) from the list on the left.
The right side shows the rules in effect for that scope in evaluation order, along with each layer's name (`▶` marks the scope's own rules).
`Enter` edits the scope's own rules, and `d` deletes them.
Scopes marked `view` in the list are read-only.
The Settings tab edits everything other than rules (the default root mode and the token scope).

| Key | Action |
| --- | --- |
| `←` `→`, `1`–`8` | Switch tabs |
| `↑` `↓` | Select in a list |
| `n`, `Enter`, `d` | Create, edit, delete (the keys available in each tab are shown below it) |
| `Tab`, `↑` `↓` | Move between form fields |
| `←` `→` | Change a choice field in a form |
| `Enter` | Open a form's rule list (in the list, `a` adds, `d` deletes, `Esc` returns) |
| `Ctrl+S`, `Esc` | Save, close the form |
| `q`, `Ctrl+C` | Quit |

With the following in `~/.ssh/config`, `ssh chloe-admin` opens it.

```text
Host chloe-admin
    HostName chloe.example.com
    User chloe
    RequestTTY yes
    RemoteCommand chloe
```

Without a terminal allocated (`ssh chloe@chloe.example.com chloe`), it prints the list of commands.

## The `chloe` command

Commands starting with `chloe` are handled by the gateway, not the VM.

```text
chloe whoami                                    the connected user's GitHub login
chloe ws list                                   list workspaces
chloe ws create NAME [--root persistent|ephemeral] [--workspace SIZE] [--docker SIZE]
                     [--cpus N] [--memory MEM] [--idle TIME]
chloe ws root NAME persistent|ephemeral          root mode (from the next start)
chloe ws disk NAME [--workspace SIZE] [--docker SIZE]   grow the disks (from the next start)
chloe ws machine NAME [--cpus N] [--memory MEM]  resize the VM (from the next start)
chloe ws idle NAME [TIME]                        time from the last connection closing to stop
chloe ws policy show NAME                        the workspace's rules
chloe ws policy set NAME                         reads TOML from stdin
chloe ws policy rm NAME
chloe ws log NAME [--lines N] [--blocked]        end of the HTTP request log
chloe ws ports NAME                              ports published in the VM and their URLs
chloe ws unpublish NAME PORT                     unpublish a port
chloe ws stop NAME
chloe ws rm NAME [--disks]                      --disks also deletes the disks
chloe secret list [--owner OWNER]
chloe secret set [--owner OWNER] KIND            reads the value from stdin
chloe secret rm [--owner OWNER] KIND
chloe policy show [OWNER]                        user, organization, or team settings
chloe policy set [OWNER]                         reads TOML from stdin
chloe policy rm [OWNER]
chloe token create NAME [--expires 30d]          admin API token
chloe token list
chloe token revoke ID
chloe key list                                  SSH public keys bound via Device Flow
chloe key rm FINGERPRINT
chloe github login|status|logout
chloe inbox [--all]                             asks from agents
chloe inbox show ID
chloe inbox allow ID                            allow network access (adds to the workspace's rules)
chloe inbox deny ID [MESSAGE]
chloe inbox answer ID [TEXT]                    an option number or answer; reads stdin without TEXT
chloe web login                                 a one-time login URL and QR code for the web UI
chloe web sessions                              list web UI sessions
chloe web revoke ID
```

NAME is written the same way as the part after `+` in the SSH username; OWNER is an organization (`acme`) or a team (`acme/dev`).
Without OWNER, `secret` and `policy` act on your personal ones.
Without `--root`, the default from the owner's (user, organization, or team) settings is used (`ephemeral` if there is no setting).
SIZE is in GiB (`50G`). Omitted disks are created at the default size from the gateway settings.
MEM is written like `8G` or `512M`, and TIME like `30m`, `2h`, `1d`, `never`, or `default`. If omitted, the gateway settings default is used.
`chloe ws list` prints the name, root mode, whether it is running, the workspace and Docker disk sizes, vCPU count, memory, and idle timeout, separated by tabs.
Without `--disks`, `chloe ws rm` leaves the workspace's disks (`/workspace`, the home directory, Docker data) on the runner host.
When deleting from the admin TUI, you choose whether to keep the disks (`k`) or delete them too (`d`).
To do the same over HTTP(S), see the [admin API](api.md).

## Caveats for shared workspaces

Inside a shared workspace's VM, every member can use `sudo` and Docker.
Inside the VM, every member can do anything root can, including to each other.
Use shared workspaces only when you trust everyone who can enter them.
Who connected and when is recorded in the gateway's audit log.

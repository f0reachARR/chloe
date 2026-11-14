# chloe

[日本語](README.ja.md)

chloe is an SSH server that runs coding agents such as Claude Code and Codex inside disposable, per-workspace Firecracker microVMs.
Users sign in with their GitHub account and land in their workspace's VM with a plain `ssh`.
API keys and GitHub tokens never enter the VM: a transparent proxy in front of each VM replaces placeholders with the real values, per destination.

```console
$ ssh alice@chloe.example.com
user@chloe:/workspace$ mise use -g <tool>   # install Claude Code, Codex, etc. with mise
user@chloe:/workspace$ claude
```

## Status

chloe is under active development and has not reached 1.0; expect breaking changes between minor versions.

- Runners (a workspace VM plus its proxy) can run either in a privileged container that hosts Firecracker (for development) or as systemd units.
  With systemd, only network setup runs as root; Firecracker and the proxy run as a dedicated unprivileged user.
- Firecracker is not yet isolated with the jailer, and there is no Kubernetes deployment.
- The example host configuration and systemd units in `deploy/` have not yet been verified on a real host.
- GitHub sign-in for the web UI has only been tested against a stub that stands in for GitHub.

The order of implementation and the outcome of each stage are recorded in the [design notes](docs/design/) (Japanese).

## Features

- **GitHub authentication**: With a GitHub App configured, SSH login uses the GitHub Device Flow. Everyone connects as the same user name (`ssh chloe@host`); chloe remembers the public key used when you approved the login and accepts it next time. Organization and team membership grants access to shared workspaces. Without a GitHub App, users log in with the public keys registered on their GitHub account.
- **Workspaces**: A personal workspace per user, plus shared workspaces per organization or team. Docker works inside the VM. A VM stops a while after the last connection closes and starts again on the next connection, but never while an agent is busy.
- **Secret injection**: The VM only ever holds placeholders. HTTPS traffic from the VM is decrypted by a transparent proxy, and a placeholder is replaced with the real value only when the destination and headers match the secret's rules.
- **Egress control**: Traffic to destinations not on the allowlist is blocked, and DNS names are not resolved upstream. Rules on destination, method, and path can narrow down how an API may be used.
- **Administration**: Manage workspaces, secrets, and egress rules (per user, organization, team, and workspace) through the `chloe` command over SSH, an admin TUI (`ssh -t <host> chloe`), or an HTTP(S) admin API.
- **Multiple hosts**: Runners can be spread over several physical servers (`chloe-node`). The control plane and the hosts talk over mutual TLS.
- **Web UI**: From a browser (including phones, and as a PWA) you can answer asks from agents, see whether an agent is busy in a workspace, and open a terminal in the workspace (the same shell as SSH). Sign in with GitHub or with a URL issued by `chloe web login` over SSH.
- **Open servers running in the VM**: An HTTP(S) server published inside the VM with `chloe-ask port publish 5173` is reachable from a browser at a random per-port host name (`k3x7q2m5vbtd.preview.example.com`). Only workspace members can open it; login goes through the web UI, and chloe's credentials are never passed to the VM.
- **Asks from agents**: Agents in the VM use `chloe-ask` (a CLI and an MCP server) to ask a human for permission to reach a blocked destination, or to ask a question, and then wait for the answer. Humans answer from the web UI, the admin TUI, `chloe inbox`, or the admin API. Even when an agent runs unattended, it keeps running while it waits.
- **Audit**: Logins, sessions, port forwarding, admin operations, and secret lookups are logged as JSON Lines. For HTTP traffic, only metadata is recorded, per workspace; users can view it with `chloe ws log` or in the admin TUI.

## Architecture

```text
             SSH                        mutual TLS (with multiple hosts)
user ──────────────> chloe-gateway ──────────────────────> chloe-node
                     (control plane)                       (runner host)
                     - GitHub authentication                │
                     - workspaces and secrets               │ one runner
                     - admin commands and admin API         ▼ per workspace
                     - answers real-value lookups   ┌──────────────────────┐
                               ▲                    │ Firecracker VM       │
                               │ real-value lookup  │  guest-agent         │
                               │                    │  mise, agents        │
                               └─────────────────── │  Docker              │
                                                    ├──────────────────────┤
                                                    │ chloe-egress         │──> allowed destinations
                                                    │ (transparent proxy,  │
                                                    │  secret injection)   │
                                                    └──────────────────────┘
```

On a single machine, the gateway starts runners with Docker on the same host.

## Known limitations

- Firecracker runs without jailer isolation (chroot, namespaces, cgroups). Even with systemd runners, it is only confined by running as a dedicated user.
- Members of a shared workspace can effectively act as root toward each other inside the VM.
- The proxy's body inspection (which checks that a placeholder is not being sent to a different destination) does not look at compressed bodies, chunked bodies, bodies larger than 1 MiB, or WebSocket payloads.
- Destinations with a passthrough rule (`action = "passthrough"`) are not subject to any other rule or injection. Configurations in which another rule overlaps a passthrough destination are rejected.
- In the VM's DNS allowlist, `*.example.com` also allows `example.com` itself.
- Stopping a VM ends the processes running in it. (Snapshots were only tried as a [proof of concept](docs/design/poc/stage7-snapshot.md) (Japanese) and are not used.)
- Removing a workspace with `chloe ws rm` leaves its disks behind.
- The admin API listens on plain HTTP unless a certificate is configured.
- With multiple hosts, workspaces cannot move between hosts, and placement ignores how much capacity each host has left. There is no way to revoke a leaked control-plane certificate.
- If the GitHub App is made installable by any account, anyone can install it and use chloe, so restrict it with `organizations` in the App settings.

## Getting started

On an x86_64 Linux machine with KVM, run the following from the repository root.
This uses the development configuration and the files in the repository as they are.
To deploy chloe on a host, follow the [installation guide](docs/install.md) instead.

```sh
images/guest/kernel/build.sh
images/guest/rootfs/build.sh
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest
cargo build -p chloe-egress -p chloe-gateway
# Put your GitHub login in allowed_users before starting.
target/debug/chloe-gateway --config images/guest/dev/gateway.example.toml
```

From another terminal, connect with a key registered on your GitHub account.

```sh
ssh -p 2222 <github-login>@127.0.0.1
```

## Documentation

| Document | Contents |
| --- | --- |
| [Installation](docs/install.md) | Building and installing the deb packages, single-host setup, multiple hosts, upgrades, backups, manual installation |
| [Usage](docs/usage.md) | Connecting, workspaces, secrets, port forwarding, opening servers running in the VM, the `chloe` command |
| [Configuration](docs/configuration.md) | The gateway, `chloe-node`, the GitHub App, user/organization/team settings, writing workspace rules |
| [Admin API](docs/api.md) | The HTTP(S) API and tokens |
| [GitHub App setup](docs/github-app.md) | Registering the App and its permissions |
| [Development](docs/development.md) | Crate layout, building, and running the tests |
| [Web UI design](docs/web.md) | Technology, structure, dependency direction, and security rules for the web UI |
| [Design notes](docs/design/) (Japanese) | Research, design rationale, and the stage-by-stage implementation log |

## Repository layout

```text
crates/
  chloe-gateway      SSH entry point and control plane (authentication, admin commands, admin API, internal API)
  chloe-controller   Workspaces, runners, secret lookups, database
  chloe-node         Daemon on each runner host
  chloe-mtls         Mutual TLS between the control plane and the hosts
  chloe-egress       Transparent proxy (TLS interception, allowlist, injection, HTTP request log)
  chloe-policy       Allowlist, path rules, injection rules
  chloe-secrets      Secret encryption and storage, 1Password Connect
  chloe-github       GitHub App (installation tokens, membership checks, Device Flow)
  chloe-audit        Audit records and log reading
  chloe-guest-agent  Daemon inside the VM (terminal and exec over vsock)
  chloe-agent-proto  Protocol between the host and guest-agent
  chloe-ask          CLI and MCP server that agents in the VM use to ask humans
  chloe-runner       Development CLI that connects directly to a VM
images/guest/
  kernel/            Guest kernel build
  rootfs/            Guest root filesystem build
  runner/            Runner startup scripts (shared by the dev container and the systemd units)
  dev/               Dev runner container, example dev configuration, VM-based tests
web/                 Web UI (React, Mantine, TanStack Router and Query; types generated from web/openapi.json)
deploy/              Example host configuration, systemd units, polkit rules
packaging/deb/       Debian package build and tests
migrations/          Database migrations
docs/                Documentation
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and the [development guide](docs/development.md).
To report a security issue, see [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

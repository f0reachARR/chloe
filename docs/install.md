# Installation

This page covers running chloe on a single host and spreading runners across several hosts.
On Ubuntu, you install chloe from deb packages.
Installing the files by hand without a deb, and running the runner in a Docker container, are covered in later sections.

Runners (a workspace's VM and its proxy) run as systemd units.
Only the per-workspace network setup runs as root; the proxy, DNS, and Firecracker run as the dedicated user `chloe`.
Firecracker is not yet isolated with jailer, so dedicate the host to chloe and let only trusted users use it.

## Host requirements

- **CPU and OS**: Ubuntu 24.04 or 26.04 on x86_64. KVM must be available (`/dev/kvm` exists). Either a physical server or a VM with nested virtualization enabled.
- **Disk**: About 3GB for the guest image. Each workspace gets a workspace disk (20GiB by default) and a Docker disk (40GiB by default), created as sparse files. Only the space actually used is consumed. The default sizes and the limits can be changed in the `[disks]` section of the configuration.
- **Network**: The host must be able to reach the outside when a VM connects to a destination on the allowlist. The veth pairs that connect runners to the host use `10.250.0.0/16` (change it if it overlaps the host's network; see [Runner and host networking](#runner-and-host-networking)).

By default, a workspace VM boots with 4 vCPUs and 4GiB of memory. The defaults are set in `[machine]` in the configuration, and users can change the size of each workspace with `chloe ws machine`. There is no upper limit, so make sure the total size of the VMs running at the same time does not exceed the host's memory.

## Install from a deb

Release debs are published on GitHub [Releases](https://github.com/f0reachARR/chloe/releases) (including `chloe-guest`).
To build them yourself, follow [Build a deb](#build-a-deb) below.

### Build a deb

Build the debs on a build machine.
Docker is required (the build runs inside an Ubuntu 24.04 container, and the result runs on both 24.04 and 26.04).
Run the following from the repository root.

```sh
# The guest kernel (Linux 6.18 series) and root filesystem (Ubuntu 26.04, Docker, mise).
images/guest/kernel/build.sh
images/guest/rootfs/build.sh

# Build chloe-common, chloe-gateway, chloe-node, and chloe-guest into build/deb/.
packaging/deb/build.sh --guest
```

Without `--guest`, `chloe-guest` is not built.
The guest image build scripts print a number and the elapsed time for each step.
Compiling the kernel takes a few minutes, and the sources and intermediate build files are kept in `build/cache/` (about 2GB).
Subsequent builds only rebuild what changed.

The root filesystem includes mise, at the version pinned in `images/guest/rootfs/Dockerfile`.
Tools such as Claude Code and Codex are not included (users install them inside the VM with mise; see [Usage](usage.md#install-tools)).

The deb version is `<last release version>+git<commit date>.<hash>` (with `.dirty` appended if there are uncommitted changes).
It sorts after the release version, so you can install it over a release deb.
The Maintainer field in control can be changed with the `DEB_MAINTAINER` environment variable.

| Package | Contents |
| --- | --- |
| `chloe-common` | `chloe-egress`, Firecracker, the runner scripts and units, the polkit rule, `/etc/chloe/runner.env`. Creates the user `chloe` and the state directory (`/var/lib/chloe`) |
| `chloe-gateway` | `chloe-gateway`, its unit, `/etc/chloe/gateway.toml`, example secret definitions (`/usr/share/doc/chloe-gateway/examples/secrets/`) |
| `chloe-node` | `chloe-node`, its unit, `/etc/chloe/node.toml` (for runner hosts in a multi-host setup) |
| `chloe-guest` | The guest kernel and root filesystem (`/usr/lib/chloe/guest`) |

### Install the packages

Copy the debs to the host and install them with `apt` (dependencies are installed too).

```sh
sudo apt install ./chloe-common_*.deb ./chloe-gateway_*.deb ./chloe-guest_*.deb
```

Installing does not start the gateway.
With the example configuration as-is, it would listen on port 22 and clash with sshd, and the users allowed to log in are placeholder names.
Configure it as described in [Run on a single host](#run-on-a-single-host) before starting it.

The packages install the following.

| Location | Contents |
| --- | --- |
| `/usr/bin/` | `chloe-gateway`, `chloe-node` |
| `/usr/libexec/chloe/` | `chloe-egress`, Firecracker, the runner scripts |
| `/usr/lib/chloe/guest/` | The guest kernel and root filesystem |
| `/usr/lib/systemd/system/` | `chloe-gateway.service`, `chloe-node.service`, the runner units (`chloe-host-net.service`, `chloe-netns@.service`, `chloe-runner@.service`) |
| `/etc/chloe/` | Configuration (`gateway.toml`, `node.toml`, `runner.env`). The GitHub App settings and private key, and the admin API certificate and key, also go here |
| `/var/lib/chloe/` | Gateway state (host key, database, master key, CA) and workspace state (`workspaces/`) |
| `/var/lib/chloe-node/` | The `chloe-node` certificate and key |
| `/var/log/chloe/` | Audit log |

The configuration files in `/etc/chloe/` are conffiles, so your edits are not overwritten on upgrade.

## Run on a single host

### Configuration

Edit `/etc/chloe/gateway.toml`.
All settings are listed in [Configuration](configuration.md).
At a minimum, change the following.

- `listen`: The SSH listen address. The example is `0.0.0.0:22`, which clashes with the host's sshd. Either move sshd to another port or have the gateway listen on another port.
- `allowed_users` and `admins`: The GitHub logins allowed to log in, and the server's operators.
- `[[policy.rules]]`: Outbound traffic rules shared by all workspaces. Traffic that matches no rule is blocked.

The state locations (`host_key`, `database`, `master_key`, `audit_log`) are set under `/var/lib/chloe` and `/var/log/chloe`, which the unit's `StateDirectory=` and `LogsDirectory=` create.
If you move them elsewhere, use a directory the `chloe` user can write to.
The runner's locations and addresses go in `/etc/chloe/runner.env` (see [Configuration](configuration.md#runner)).

### Start

```sh
sudo systemctl enable --now chloe-gateway
journalctl -u chloe-gateway -f
```

The host key, database, and master key are created on first start.
If a GitHub App is configured, users log in with the user name `chloe` and, the first time, verify their GitHub account with the Device Flow.
Without a GitHub App, users log in with the public keys registered on GitHub.

```sh
ssh chloe@<host>                # with a GitHub App
ssh <github-login>@<host>       # without a GitHub App
```

The first connection boots the workspace VM (this takes a few seconds).
See [Usage](usage.md) for how to use it.

Restarting the gateway does not stop running workspaces.
The newly started gateway takes them over and keeps answering their requests for real values.

### Operational commands

When you run `chloe-gateway` subcommands such as `node add` by hand, run them as the `chloe` user.
If you run them as root, the files SQLite creates next to the database (such as `chloe.db-wal`) can end up owned by root, and the gateway may then fail to open them.

```sh
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node list
```

### GitHub App

With a GitHub App configured, members of an organization can log in, the Device Flow becomes mandatory for login, and GitHub can be used inside the VM.
See [Preparing a GitHub App](github-app.md) for how to register one.
Put the app settings and private key in `/etc/chloe/`, and set their paths in `github_app` in the gateway configuration.

## Runner and host networking

The gateway starts a workspace's runner with `systemctl start chloe-runner@ws-<number>`.
The polkit rule allows the `chloe` user only to start, stop, and reload this unit.
`chloe-runner@` pulls in `chloe-netns@` (root), which creates the per-workspace network namespace, and `chloe-host-net` (root), which installs the host-side rules.

You can list running runners with `systemctl list-units 'chloe-runner@*'`, and view the guest's serial console output with `journalctl -u chloe-runner@ws-<number>`.
The proxy and DNS logs are in `/var/lib/chloe/workspaces/<ws-number>/logs/`.

A runner's namespace is connected to the host by a veth pair (named `cw<number>` on the host side), and the host masquerades its traffic out.
Only the proxy and DNS inside the namespace go out; the VM's traffic is directed to the proxy and DNS inside the namespace.
veth addresses are allocated as one /30 per runner from `CHLOE_NET_POOL` in `/etc/chloe/runner.env` (default `10.250.0.0/16`).
`chloe-host-net` installs the following rules in the nftables table `inet chloe-host` and sets `net.ipv4.ip_forward` to 1.

- Drop traffic from namespaces to the host (protecting the control plane's internal API, sshd, and so on).
- Drop forwarding from namespaces to private addresses (RFC1918 and so on). The proxy does not check destination addresses, so even if a user's rule allows a name that resolves to an internal address, it is stopped here. DNS (port 53) is let through, however, because the upstream DNS server is often on the LAN.
- Drop forwarding between namespaces.

For upstream DNS, if the host uses systemd-resolved, the servers listed in `/run/systemd/resolve/resolv.conf` are used (`127.0.0.53` is not reachable from inside the namespace).
Otherwise, `/etc/resolv.conf` is used.

Watch out for the following.

- On hosts that use `nftables.service`, the `flush ruleset` in its configuration (`/etc/nftables.conf`) also removes `inet chloe-host`. After reloading `nftables.service`, reinstall the rules with `sudo systemctl reload chloe-host-net` (a restart would also stop running runners).
- If another component (Docker, ufw, and so on) installs rules that drop forwarded traffic, forwarding from `cw*` may be dropped too. In nftables, a packet passes a hook only if it is accepted by the rules for that hook in every table.

## State locations and backup

The locations in the example layout are shown in parentheses.

| Location | Contents | Backup |
| --- | --- | --- |
| `database` (`/var/lib/chloe/chloe.db`) | Workspaces, secrets (encrypted), settings, token hashes, the host list | Required, together with `master_key` |
| `master_key` (`/var/lib/chloe/master.key`) | The key that encrypts secrets | Required. If lost, stored secrets can no longer be read |
| `host_key` (`/var/lib/chloe/host_key`) | The SSH host key | Required. If it changes, it no longer matches users' `known_hosts` |
| `pki_dir` in `[cluster]` (`/var/lib/chloe/pki`) | The CA certificate and key | Required. If lost, every host has to join again |
| `audit_log` (`/var/log/chloe/audit.jsonl`) | The audit log (JSON Lines) | Depends on your operational policy |
| `CHLOE_STATE_ROOT` in `runner.env` (`/var/lib/chloe/workspaces`; on each host in a multi-host setup) | Per-workspace disks, the proxy CA, the HTTP log | Required if you want to keep workspace contents |

`database` is SQLite, used in WAL mode.
To copy it while chloe is running, use `sqlite3`'s `.backup` as the `chloe` user.

```sh
sudo -u chloe sqlite3 /var/lib/chloe/chloe.db ".backup /var/lib/chloe/chloe.db.bak"
```

Deleting a workspace with `chloe ws rm` leaves its disks in place.
To delete them as well, use `chloe ws rm NAME --disks`.

## Run across multiple hosts

Put the gateway (the control plane) in one place and run `chloe-node` on each host.
The control plane holds a CA and issues host certificates.
The control plane and the hosts connect over mutual TLS (mTLS).

### Ports

| Direction | Port (example) | Purpose |
| --- | --- | --- |
| User → control plane | 22 | SSH |
| Host → control plane | 8444 | Internal API (join, relaying requests for real values) |
| Control plane → host | 8445 | The `chloe-node` API (runner operations, terminal relay) |

### Control plane

On the control plane, install `chloe-common` and `chloe-gateway` (no runners run there, so `chloe-guest` is not needed).
Remove `[runner]` from the gateway configuration and add `[cluster]`.

```toml
[cluster]
listen = "0.0.0.0:8444"
# The name or IP address hosts connect to. Included in the internal API certificate.
server_names = ["cp.example.com"]
pki_dir = "/var/lib/chloe/pki"
```

On start, the gateway creates the CA if it does not exist.
Register a host and create a join token.
The URL is the one the control plane uses to call that host's API, and its host name goes into the host's certificate.

```sh
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node add n1 --url https://n1.example.com:8445
```

The printed token can be used only once and expires after one hour (configurable with `join_ttl_secs`).
The token contains the fingerprint of the CA certificate, which the host uses to verify the control plane on its first connection.

### Hosts

On each host, install `chloe-common`, `chloe-node`, and `chloe-guest`.

```sh
sudo apt install ./chloe-common_*.deb ./chloe-node_*.deb ./chloe-guest_*.deb
```

Join and receive a certificate.
The `chloe` user cannot write to `/var/lib`, so create the state directory first (the unit's `StateDirectory=` only creates it when the unit starts).

```sh
sudo install -d -o chloe -g chloe -m 700 /var/lib/chloe-node
sudo -u chloe chloe-node join --server https://cp.example.com:8444 --token <token> --dir /var/lib/chloe-node/tls
```

Set `control_plane` in `/etc/chloe/node.toml` and start the service.

```sh
sudo systemctl enable --now chloe-node
```

Run `sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node list` on the control plane to check which hosts have joined.

### Placement

A workspace is bound to the host it was first started on (because its disks are on that host).
A new workspace is placed on the host with the fewest workspaces.
There is no way yet to move a workspace between hosts.

While a host is unreachable, its workspaces cannot be entered, but workspaces on other hosts remain usable.
The control plane does not treat the runners on an unreachable host as stopped, and handles them as before once the host reconnects.

To remove a host's registration, first delete the workspaces placed on it.

```sh
sudo -u chloe chloe-gateway --config /etc/chloe/gateway.toml node rm n1
```

## Enable the admin API

Add `[api]` to the gateway configuration.
If you set a certificate and key, it listens over HTTPS; otherwise over HTTP.
When listening over HTTP, use it only locally or put it behind a reverse proxy that terminates TLS.

```toml
[api]
listen = "127.0.0.1:8080"
# tls_cert = "/etc/chloe/api.crt"
# tls_key = "/etc/chloe/api.key"
```

Make the key readable by the `chloe` user (`sudo install -m 640 -g chloe api.key /etc/chloe/api.key`).

See [Admin API](api.md) for usage.

## Enable the web UI

The web UI is served on the same listener as the admin API.
In `[api]`, set the URL opened in the browser (`public_url`) and the location of the UI files (`web_root`).
The UI files are included in the chloe-gateway deb.

```toml
[api]
listen = "127.0.0.1:8080"
public_url = "https://chloe.example.com"
web_root = "/usr/share/chloe/web"
```

When it sits behind a reverse proxy that terminates TLS, set `public_url` to the reverse proxy's URL.
The UI is served from `/`, so `public_url` cannot contain a path (something like `https://example.com/chloe` does not work).
Ask events are streamed over SSE from `/api/v1/events`, so disable response buffering in the reverse proxy and use a long timeout (with nginx, `proxy_buffering off;` and `proxy_read_timeout 1h;`; the gateway also returns `X-Accel-Buffering: no`).
The web terminal upgrades `/api/v1/workspace-terminal/` to a WebSocket, so the reverse proxy must pass the `Upgrade` and `Connection` headers.
Even while nothing is typed, the UI sends a `ping` frame every 30 seconds and the gateway replies with `pong`; make the timeout longer than that (nginx's default of 60 seconds is fine).

```nginx
# inside http
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

# inside server
location /api/ {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_buffering off;
    proxy_read_timeout 1h;
}
```

## Open servers running in the VM

Ports published inside the VM with `chloe-ask port publish` can be opened from a browser at a host with a random per-port name (`<name>.<domain>`).
Enable the web UI, then set `proxy_url` in `[api]`.

```toml
[api]
listen = "127.0.0.1:8080"
public_url = "https://chloe.example.com"
web_root = "/usr/share/chloe/web"
proxy_url = "https://chloe-preview.example.net"
```

Point every name under the domain (`*.chloe-preview.example.net`) at the gateway or the reverse proxy.
This requires a wildcard DNS record and a wildcard certificate (with Let's Encrypt, obtained via the DNS-01 challenge).
The gateway identifies the published port's host by the request's `Host`, so the reverse proxy must pass `Host` through unchanged.
The gateway appends the address of the peer that connected to it (the reverse proxy) to the end of `X-Forwarded-For` before passing the request to the server in the VM, so to deliver the user's IP address, the reverse proxy must also set `X-Forwarded-For`.
Development servers in the VM use WebSockets for HMR and the like, so pass `Upgrade` and `Connection` too.

```nginx
server {
    listen 443 ssl;
    server_name *.chloe-preview.example.net;
    ssl_certificate     /etc/letsencrypt/live/chloe-preview.example.net/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/chloe-preview.example.net/privkey.pem;
    client_max_body_size 0;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        # The gateway passes the user's IP address on to the server in the VM.
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

Agents can rewrite pages served from the VM, so we recommend using a separate registrable domain from the web UI's (see [Configuration](configuration.md#api)).
The VM cannot reach the proxy host unless it is added to the allowlist.

There are two ways to log in.

- **GitHub**: Set the GitHub App's `client_secret`, and set the Callback URL in the App's settings to `<public_url>/api/auth/github/callback` (see [Preparing a GitHub App](github-app.md)).
- **From SSH**: `ssh chloe@host chloe web login` prints a single-use URL and QR code valid for 5 minutes. This works without a GitHub App.

See [Usage](usage.md#web-ui) for how to use it.

## Upgrade

Install the newly built debs with `apt install`, the same way you installed them.

- **The gateway and `chloe-node`**: If they are running, the upgrade restarts them. Database migrations are applied on start. Running workspaces are not stopped.
- **Runners (`chloe-common`)**: Running runners are not stopped. New runners use the new scripts and `chloe-egress` from their next start. The host-side rules are reinstalled.
- **Guest image (`chloe-guest`)**: Workspaces use the new image from their next start. dpkg writes the new file under a different name and then swaps it in, so running VMs can keep reading the old file they have open. A running workspace keeps running on the old image until you stop it with `chloe ws stop`. A workspace with a persistent root (`persistent`) boots with its saved changes layered on top of the new image, and records in `/etc/chloe/root-outdated` inside the VM that the image version differs from the one the layer was created on (how to handle the layer when the versions differ is not decided yet).

In a multi-host setup, upgrade `chloe-node`, `chloe-common`, and `chloe-guest` on every host.

## Uninstall

```sh
sudo apt remove chloe-gateway chloe-node chloe-common chloe-guest
```

Removing the packages stops and disables the gateway and `chloe-node`, and also stops running runners.
`apt purge` also deletes the configuration in `/etc/chloe/`.
Either way, the state (`/var/lib/chloe`, `/var/lib/chloe-node`, `/var/log/chloe`) and the user `chloe` are kept.
This is because losing the master key would make stored secrets unreadable.
Delete them by hand if you no longer need them.

## Install without a deb

On distributions other than Ubuntu, or to try out work in progress, install the build output by hand.
The example configuration and units in `deploy/` are written for the layout in this section (`/usr/local`).

### Build requirements

- **Rust**: Install [rustup](https://rustup.rs/). The repository's `rust-toolchain.toml` selects stable and the `x86_64-unknown-linux-musl` target used for the guest binaries.
- **BoringSSL build dependencies**: The proxy (`chloe-egress`) builds BoringSSL from source. It needs C and C++ compilers, cmake, libclang, and git. On Debian and Ubuntu:

  ```sh
  sudo apt-get install build-essential cmake clang libclang-dev git
  ```

- **Docker**: The kernel and root filesystem are built inside Docker containers.
- **Node.js and pnpm**: Needed only to serve the web UI (the version is in `packageManager` in `web/package.json`).

### Build

```sh
images/guest/kernel/build.sh
images/guest/rootfs/build.sh
cargo build --release -p chloe-gateway -p chloe-egress -p chloe-node
(cd web && pnpm install --frozen-lockfile && pnpm build)   # only to serve the web UI
```

### Host packages

```sh
sudo apt-get install dnsmasq-base e2fsprogs iproute2 jq nftables polkitd
```

Firecracker is not in the Ubuntu archive, so install the binary from its [releases](https://github.com/firecracker-microvm/firecracker/releases).
Use the same version as the runner container (`images/guest/dev/runner.Dockerfile`).

```sh
v=v1.17.0
curl -fsSLO "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz"
curl -fsSLO "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz.sha256.txt"
sha256sum -c "firecracker-$v-x86_64.tgz.sha256.txt"
tar -xzf "firecracker-$v-x86_64.tgz"
sudo install -m 755 "release-$v-x86_64/firecracker-$v-x86_64" /usr/local/bin/firecracker
```

### Install the files

Run the following from the repository root.

```sh
sudo useradd --system --no-create-home --home-dir /var/lib/chloe --shell /usr/sbin/nologin --groups kvm chloe
sudo install -m 755 target/release/chloe-gateway target/release/chloe-node /usr/local/bin/
sudo install -D -m 755 target/release/chloe-egress /usr/local/lib/chloe/chloe-egress
sudo install -D -m 755 -t /usr/local/lib/chloe/runner images/guest/runner/*
sudo install -D -m 644 build/guest/kernel/vmlinux /usr/local/lib/chloe/guest/kernel/vmlinux
sudo install -D -m 644 build/guest/rootfs.ext4 /usr/local/lib/chloe/guest/rootfs.ext4
sudo install -m 644 deploy/systemd/*.service /etc/systemd/system/
sudo install -m 644 deploy/polkit/50-chloe.rules /etc/polkit-1/rules.d/
sudo install -m 644 deploy/tmpfiles/chloe.conf /etc/tmpfiles.d/
sudo install -d -m 755 /etc/chloe
sudo install -m 644 deploy/runner.env deploy/gateway.toml /etc/chloe/   # node.toml on chloe-node hosts
sudo systemd-tmpfiles --create /etc/tmpfiles.d/chloe.conf
sudo systemctl daemon-reload
# Only to serve the web UI (remove the old files first)
sudo rm -rf /usr/local/share/chloe/web && sudo install -d -m 755 /usr/local/share/chloe && sudo cp -r web/dist /usr/local/share/chloe/web
```

Add the user `chloe` to the `kvm` group (so Firecracker can open `/dev/kvm`).
Install the guest images with `install`, not `cp`.
`install` removes the old file before creating the new one, so running VMs can keep reading the old file they have open.
`cp` overwrites the contents of the same file, which corrupts the root filesystem of running VMs.

Then configure and start chloe as described in [Run on a single host](#run-on-a-single-host) or [Run across multiple hosts](#run-across-multiple-hosts).
To upgrade, reinstall with the same `install` commands and restart with `sudo systemctl restart chloe-gateway` (`chloe-node` on hosts).

## Run the runner in Docker

If you remove `kind` from `[runner]` in the configuration and set `guest_dir`, `egress_bin`, and `state_root`, runners run in a privileged Docker container (`chloe-dev-runner`) (see [Configuration](configuration.md#runner)).
Testing on development machines uses this mode.
The runner container runs with `--privileged`, and the user that runs the gateway (`chloe-node` on hosts) is added to the `docker` group.
Members of the `docker` group can do anything root can.

```sh
docker build -t chloe-dev-runner -f images/guest/dev/runner.Dockerfile images/guest
sudo usermod -aG docker chloe
```

After rebuilding `chloe-dev-runner`, stop running workspaces once.
Runners still on the old image cannot rebuild the VM's DNS allowlist when they receive a configuration change.

To switch from Docker to systemd, stop all workspaces before changing the configuration (check with `docker ps --filter name=chloe-` that no runner containers remain).
If you switch without stopping them, the new mode cannot recognize the running Docker containers as its own runners.
The layout of a workspace's state directory is the same, so disks and the proxy CA carry over as-is.
If root-owned files remain in the state directory, fix their ownership with `sudo chown -R chloe:chloe /var/lib/chloe/workspaces`.

## Troubleshooting

- **Logs**: The gateway and `chloe-node` log to standard error (read them with `journalctl -u chloe-gateway`; adjust verbosity with `RUST_LOG`). Each workspace's proxy log (`egress.log`), HTTP request log (`requests.jsonl`), and DNS log (`dnsmasq.log`) are in `/var/lib/chloe/workspaces/<ws-number>/logs/`. Users can also view the HTTP request log with `chloe ws log` and in the admin TUI. These logs are not rotated and keep growing until they are deleted along with the workspace's disks.
- **The VM does not boot**: `journalctl -u chloe-runner@ws-<number> -u chloe-netns@ws-<number>` shows the network setup, Firecracker, and the guest's serial console output. Check that `/dev/kvm` can be opened by the `kvm` group. When running the runner in Docker, the output is in `docker logs chloe-ws-<number>`.
- **The gateway cannot start runners**: If `systemctl start chloe-runner@...` fails with `Interactive authentication required`, check that the polkit rule (`/usr/share/polkit-1/rules.d/50-chloe.rules`) is loaded and that polkit is running (`systemctl status polkit`).
- **The VM cannot reach the outside**: The proxy rejects destinations not on the allowlist with 403 (names resolve to the proxy's address). `requests.jsonl` and `dnsmasq.log` show which destinations were blocked. If allowed destinations are also unreachable, check with `sudo nft list table inet chloe-host` that the host-side rules are in place (and have not been removed by `nftables.service`).

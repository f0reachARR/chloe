# Development

The crate layout is in the [README](../README.md#repository-layout), the design assumptions are in the [research and plan](design/research-and-plan.md) (Japanese), and the per-stage plans and results are in the [implementation log](design/implementation-steps.md) (Japanese).

## Build and static checks

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

`cargo test` does not use VMs and finishes in a few seconds.
The guest binary (`chloe-guest-agent`) is built with musl by `images/guest/rootfs/build.sh`, which puts it into the root filesystem.

## Web UI

The web UI lives in `web/`.
Its layout, dependency direction, where state lives, and security rules are in the [web UI guidelines](web.md).
Install Node.js and pnpm with mise or similar (the version is `packageManager` in `web/package.json`).

```sh
cd web
pnpm install
pnpm dev            # http://localhost:5173. /api is proxied to the gateway (CHLOE_API, default http://127.0.0.1:8080)
pnpm typecheck
pnpm lint           # oxlint
pnpm format         # oxfmt (pnpm format:check to only check)
pnpm test           # Vitest
pnpm build          # writes to web/dist
```

For development, set the gateway's `public_url` under `[api]` to the Vite URL (`http://localhost:5173`) and leave `web_root` unset.
The GitHub login callback also reaches the gateway through Vite, and the cookie and `Origin` checks work unchanged.
Without a GitHub App, log in with the URL that `chloe web login` prints.

### Adding a feature

1. Create `web/src/features/<feature>/` and write `index.ts` and, if needed, `feature.ts` (navigation items, event handlers).
2. Add it to the list in `web/src/app/features.ts`.
3. Write the route in `web/src/routes/_app/<path>.tsx` (screens that require login go under `_app`).

### API types and tests

When you change the admin API, re-export the spec and regenerate the types.
`cargo test` fails if `web/openapi.json` is out of sync with the code.

```sh
cargo run -p chloe-gateway -- openapi > web/openapi.json
(cd web && pnpm api)
```

Fake APIs for tests are written with `http` from `web/src/test/msw.ts`.
Paths not in the spec and responses with mismatched types fail `pnpm typecheck`.

### E2E

UI tests against a real gateway and VM are written with Playwright in `web/e2e` and run with `images/guest/dev/test-all.sh web` (see [VM-based tests](#vm-based-tests) below).
The script starts the gateway, passes the `chloe web login` URL (`CHLOE_E2E_LOGIN`) and the UI URL (`CHLOE_E2E_URL`), and runs `pnpm e2e`.
The first time, install the browser with `pnpm exec playwright install chromium`.
Tests fail if errors (such as CSP violations) appear in the console.
With `CHLOE_E2E_SCREENSHOTS=1`, the final screen of every test is captured to `build/dev/web/playwright`, so you can check how things look.

## Two layers of tests

- **Rust tests**: check the decision logic. Controller stop scheduling, stopping that waits for busy agents, reconcile, answers to queries, policy layering, mutual TLS role checks, and so on are tested against a fake runner, a fake guest-agent, and a fake GitHub.
- **VM-based tests**: check that the parts are wired together. They run a real gateway, `chloe-node`, runner, and VM, and check only representative items for each feature.

Add fine-grained decision cases to the Rust tests, not to the VM-based tests.

## VM-based tests

These need KVM and Docker.
The `systemd` test also runs in a privileged Docker container, so root privileges are not needed.
Build the guest image first ([Install](install.md#build-a-deb)).
No real API keys are used: fake values are registered as secrets, and the tests confirm that injected requests get a 401 at the destination.

```sh
images/guest/dev/test-all.sh            # run everything in parallel
images/guest/dev/test-all.sh mvp node   # run the named tests
```

| Test | Covers |
| --- | --- |
| `smoke` | VM boot, Docker, mise, and that the workspace and Docker data survive a restart |
| `net` | Transparent proxy, allowlist, DNS, injection, passthrough destinations, and that mise can install tools through the proxy |
| `root` | Environment variables passed by the runner, ephemeral and persistent root |
| `disk` | Creating the disk at the specified size, growing it on the next boot while keeping its contents, and never shrinking it |
| `mvp` | Two users, secrets, live updates, HTTP request log, asks from agents, workspaces, stop, admin API |
| `web` | Web UI E2E (`web/e2e`). Against a gateway serving the built UI, using Playwright's Chromium at both smartphone width and on a wide screen, checks the terminal, the key row, and answering in the Inbox |
| `github` | GitHub App (with a server impersonating GitHub), login with Device Flow required, linking a shared username to keys, two organizations, permissions for settings and secrets, admin TUI |
| `lifecycle` | Stopping that waits for busy agents, gateway restart, port forwarding, audit log |
| `machine` | Per-workspace VM vCPU count and memory, idle timeout (including never stopping) |
| `node` | Two `chloe-node`s, join, HTTP request log from another host, growing the disk, mutual TLS, unreachable hosts |
| `systemd` | Runner run by systemd (`kind = "systemd"`). Uses a privileged container with systemd as PID 1 in place of the host and, with the `deploy/` units, polkit rules, and host network rules, checks startup, users, outbound traffic and DNS, blocking of the host and private addresses, policy replacement, gateway restart, and stop and cleanup |

Each test uses its own VM names, state directories, and ports, so they can run in parallel (as of 2026-10-17, 320 checks in 79 seconds).
Each script's output goes to `build/dev/test-logs/<name>.log`.
Each check is tagged with the seconds since the previous one, and tests that go through the gateway print per-section durations at the end.

When writing test scripts:

- Instead of a fixed-length `sleep`, wait for a condition (a log line, a port, the runner's state) with `wait_until` (`images/guest/dev/gateway-lib.sh`).
- Shorten the idle timeout and polling interval, but not so far that the runner stops between tests.
- Never print real values (even fake ones) or tokens in test output.

## Testing the deb

The deb built by `packaging/deb/build.sh` is installed with `apt` into a privileged Ubuntu container with systemd as PID 1 and tested.
The tests run in order: fresh install, running a runner, upgrading to a newer version (running workspaces do not stop), remove, and purge.
The guest image comes from `build/guest`.

```sh
packaging/deb/build.sh
packaging/deb/test.sh 24.04
packaging/deb/test.sh 26.04
```

`build.sh` also prints lintian results.
Intended warnings go in `packaging/deb/lintian-suppress` along with the reason.

## Entering a VM directly

You can boot a VM in the development container and get into it without going through the gateway.

```sh
images/guest/dev/run.sh                                     # connect the terminal to the serial console
CHLOE_DEV_DETACH=1 images/guest/dev/run.sh                  # start in the background
target/debug/chloe-runner shell --vsock build/dev/vsock.sock
```

Running `sudo reboot` inside the VM terminates Firecracker.

## CI

GitHub Actions runs the tests that do not need VMs and builds the debs.
The VM-based tests, which need KVM, and the deb tests (`packaging/deb/test.sh`) are not run in CI; run them locally.

- **CI** (`.github/workflows/ci.yml`): on pushes to main and pull requests, runs [Build and static checks](#build-and-static-checks) above (clippy with `-D warnings`) plus the web UI's lint, formatting, tests, and build. It fails if `web/src/shared/api/schema.d.ts` or `web/src/routeTree.gen.ts` differ from their regenerated versions.
- **release** (`.github/workflows/release.yml`): on pushes to main and manual runs, runs `packaging/deb/build.sh` and uploads `chloe-common`, `chloe-gateway`, and `chloe-node` as artifacts. Manual runs also build the guest kernel and root filesystem to produce `chloe-guest` (this can be turned off). The kernel comes from the cache unless `images/guest/kernel/` has changed.
  On pushes to main, release-please (configured in `release-please-config.json` and `.release-please-manifest.json`) determines the next version from the Conventional Commits types of the commits since the previous release, and recreates a release pull request that updates `CHANGELOG.md` and the versions (`Cargo.toml`, `Cargo.lock`, `web/package.json`). Until 1.0.0, even breaking changes bump minor rather than major (`feat` bumps minor, `fix` bumps patch).
  Merging that pull request creates the tag `v<version>` and a GitHub release, and in the same run builds the debs including `chloe-guest` with `DEB_RELEASE=1` (no `+git` in the version) and attaches them to the release.
  Enable "Allow GitHub Actions to create and approve pull requests" in the repository settings. Pull requests created with GITHUB_TOKEN do not trigger CI, so CI does not run on the release pull request (it only contains versions and `CHANGELOG.md`).

## Commits

Use Conventional Commits, with descriptions in English and the crate or area as the scope (`feat(gateway): ...`, `fix(controller): ...`, `docs: ...`).
release-please builds `CHANGELOG.md` from these messages.

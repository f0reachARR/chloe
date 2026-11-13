# AGENTS.md

Guidance for coding agents (and humans) working in this repository.
For what chloe is, read [README.md](README.md); for the full developer workflow, read [docs/development.md](docs/development.md).

## Project in one paragraph

chloe is an SSH server (Rust) that runs coding agents in per-workspace Firecracker microVMs.
`chloe-gateway` is the SSH entry point and control plane; `chloe-controller` owns workspaces, runners, and the database; each runner is a VM plus a transparent egress proxy (`chloe-egress`) that injects secrets and enforces the allowlist; `chloe-node` runs runners on other hosts over mutual TLS.
The web UI lives in `web/` (React, Mantine, TanStack Router/Query) and talks to the gateway's admin API.
See the repository layout in the README for every crate.

## Commands

```sh
# Rust: what CI runs
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked        # fast, no VM needed

# Web UI (Node.js and pnpm via mise; pnpm version is in web/package.json)
cd web
pnpm install
pnpm lint && pnpm format:check && pnpm typecheck && pnpm test && pnpm build
```

If you change the admin API, regenerate the OpenAPI spec and the TypeScript types; `cargo test` fails when `web/openapi.json` is out of date, and CI fails when `web/src/shared/api/schema.d.ts` or `web/src/routeTree.gen.ts` differ from what the tools generate.

```sh
cargo run -p chloe-gateway -- openapi > web/openapi.json
(cd web && pnpm api)
```

## Tests

There are two layers; put each check in the right one.

- **Rust tests** (`cargo test`) verify decision logic against fake runners, a fake guest-agent, and a fake GitHub. Add edge cases here.
- **VM-based tests** (`images/guest/dev/test-all.sh [name...]`) verify that real components connect: gateway, `chloe-node`, runners, and VMs. They need KVM and Docker, take minutes, and do not run in CI. Keep them to one representative check per feature. Run them in the background and read `build/dev/test-logs/<name>.log`.
- Browser checks for the web UI go in `web/e2e` (Playwright) and run with `images/guest/dev/test-all.sh web`, not as ad-hoc scripts.
- In test scripts, wait on conditions with `wait_until` (`images/guest/dev/gateway-lib.sh`) rather than fixed `sleep`s, and never print real or fake secret values or tokens.

The guest images must be built first (`images/guest/kernel/build.sh`, `images/guest/rootfs/build.sh`).

## Conventions

- **Branches**: work on a branch (`feat/...`, `fix/...`, `docs/...`), not on `main`. Make small, focused commits.
- **Commits**: Conventional Commits in English, scoped by crate or area: `feat(gateway): ...`, `fix(controller): ...`, `docs: ...`. release-please generates `CHANGELOG.md` and the version from these, so the type matters (`feat` → minor, `fix` → patch; before 1.0, breaking changes bump minor).
- **Dependencies**: prefer a well-maintained crate over hand-rolled protocol code (for example, MCP uses `rmcp`).
- **Web UI**: the stack and structure are decided in [docs/web.md](docs/web.md); follow its dependency rules and discuss before changing the stack. To add a feature, follow "Adding a feature" in docs/development.md.
- **Migrations**: add a new numbered file in `migrations/`; never edit an existing one.
- **Security-sensitive code**: the VM is untrusted. Real secret values and chloe credentials must never reach the VM; keep that invariant when touching `chloe-egress`, `chloe-policy`, `chloe-secrets`, or anything that passes data into the guest.

## Documentation

- User documentation is in English under `docs/`. Update it in the same change as the behavior it describes.
- `README.ja.md` is a Japanese translation of `README.md`; when you change the README, update both.
- `docs/design/` holds historical design notes in Japanese. They are records, not user docs; do not rewrite them to match current behavior.
- Code comments and some CLI/UI messages are still in Japanese; new comments and messages may be written in English.

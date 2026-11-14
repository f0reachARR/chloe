# Contributing to chloe

Thanks for your interest in chloe.
The project is pre-1.0 and moving quickly, so for anything larger than a small fix, please open an issue first to discuss the approach.

## Before you start

- Read the [development guide](docs/development.md) for building, testing, and the layout of the code.
- [AGENTS.md](AGENTS.md) summarizes the conventions in one place (it is written for coding agents, but applies to everyone).
- To report a vulnerability, do not open a public issue; see [SECURITY.md](SECURITY.md).

## Pull requests

1. Branch from `main`.
2. Keep each change focused, and update the documentation in `docs/` along with behavior changes.
3. Make sure the checks CI runs pass locally: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and, if you touched `web/`, `pnpm lint`, `pnpm format:check`, `pnpm test`, and `pnpm build`.
4. If your change affects how runners, VMs, or the proxy behave, run the relevant VM-based tests (`images/guest/dev/test-all.sh <name>`); they need KVM and are not run in CI. Mention in the pull request which ones you ran.
5. Write commit messages and the pull request title as [Conventional Commits](https://www.conventionalcommits.org/) in English (`feat(gateway): ...`, `fix(egress): ...`). The changelog is generated from them.

## License

By contributing, you agree that your contributions are dual licensed under the [Apache License, Version 2.0](LICENSE-APACHE) and the [MIT license](LICENSE-MIT), as described in the [README](README.md#license).

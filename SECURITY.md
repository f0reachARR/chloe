# Security policy

chloe runs untrusted code (coding agents) and holds credentials on its behalf, so security reports are very welcome.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: on the repository page, open **Security** → **Report a vulnerability**.
Do not open a public issue or pull request for a vulnerability.

Include what you can of: the affected component (gateway, egress proxy, runner, guest, web UI, ...), the version or commit, steps to reproduce, and the impact you expect.
We will acknowledge the report, keep you informed while we work on a fix, and credit you in the release notes unless you prefer otherwise.

## Supported versions

chloe is pre-1.0. Fixes are made on `main` and shipped in the next release; older releases are not patched.

## Scope and known limitations

Issues that are already listed under [Known limitations](README.md#known-limitations) in the README (for example, the lack of jailer isolation, or the proxy not inspecting compressed bodies) are known.
Reports that show a concrete way to exploit them beyond what is described there are still welcome.

Of particular interest:

- Real secret values or chloe credentials reaching the VM, or being sent to a destination their rules do not allow.
- Bypassing the egress allowlist or DNS restrictions from inside the VM.
- Escaping the VM or the runner's network namespace.
- Accessing another user's workspace, secrets, or published ports.
- Authentication or authorization flaws in SSH login, the admin API, or the web UI.

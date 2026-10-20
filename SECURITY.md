# Security Policy

## Reporting a vulnerability

Please report security issues privately, not through public issues. Open a private advisory at https://github.com/philippe-desplats/procly/security/advisories/new (the "Report a vulnerability" button under this repository's Security tab).

Include a description, reproduction steps, and the affected version (`procly --version`). Expect an acknowledgement within 72 hours and a remediation plan once the report is triaged.

## Supported versions

Only the latest released version receives security fixes.

## Threat model

Procly is a local, per-user tool. The Daemon listens only on a Unix socket, `procly.sock`, never on a TCP port (ADR 0003). The state directory holding it is created with mode `0700` and the socket is set to `0600` right after it is bound, so only your user, and root, can reach the Daemon. The state documents it writes there, such as `leases.json` and `services.json`, are `0600` as well.

Procly runs the `run` commands of `procly.toml` and `procly.local.toml`, the `[worktree] setup` commands and command Seeds with your rights, through `/bin/sh -c`, exactly as if you had typed them. The commands of a Driver run inside the container of their Compose service, through `docker compose exec`. Treat a `procly.toml` like a `Makefile` or a `package.json` script: review it before running `procly up` or `procly worktree init` in a repository you do not trust. The Guard and the Rules block are written only when you run `procly agent install`, and the Guard lets through every command it does not recognize, it is a steering aid for agents and not a sandbox.

## Supply chain posture

- The CI (`ci.yml`) builds, lints and tests every push to `main` and every pull request on macOS and Linux, with `Cargo.lock` enforced through `--locked`. It references its actions by major version tag.
- Releases (`release.yml`) are built from a `v<version>` tag on GitHub-hosted runners, one native runner per target, after checking that the tag matches the crate version. The SHA-256 of every archive is published in a `SHA256SUMS` file on the release, and the Homebrew formula is rendered from those checksums. The workflow runs with a read-only default token, and only the publishing job gets `contents: write`. Releases are not signed and ship no SBOM or build provenance attestation yet.
- Dependencies are checked against the RustSec advisory database with `cargo audit` on every push to `main`, on pull requests that touch `Cargo.lock`, and weekly (`security.yml`).
- The code and the workflows are analysed by CodeQL (`codeql.yml`), and the project is tracked by OpenSSF Scorecard (`scorecard.yml`).
- These 3 security workflows run with a read-only default token, pin their actions to full commit SHAs, and run `step-security/harden-runner` in audit mode.
- Crates and actions are kept current by Dependabot, grouped weekly.

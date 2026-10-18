# Inject Derived variables into the process, never rewrite the repository's .env

Ports appear inside many URLs of a typical `.env` (API host, CORS origin, OAuth callbacks, WebAuthn origin). Procly computes those as Derived variables from templates in `procly.toml` and injects them into the environment of the processes it starts, mirrored in a git-excluded Env file for tools it does not launch. The repository's own `.env` stays untouched, unlike setup scripts that rewrite it per worktree.

## Consequences

- This relies on AdonisJS, Next.js and `dotenv-cli` not overriding a variable already present in the environment. That assumption is unverified and is tested in the first prototype.

## Verification

The assumption of the Consequences above was checked on 2026-10-18 with `scripts/check-env-precedence.mjs`, which installs the tools at pinned versions in a temporary directory, sets `FOO=from_procly` in the environment, writes `FOO=from_file` in every env file the tool loads in development, and reads `FOO` back through the loader of the tool. No server is started.

| Tool | Version | What ran | Files loaded | Result |
| --- | --- | --- | --- | --- |
| dotenv-cli | 11.0.0 | the `dotenv` command, plain and with `-c development` | `.env`, then with the cascade `.env.local`, `.env.development`, `.env.development.local` | `FOO=from_procly` |
| Next.js | 16.4.2 | `loadEnvConfig` of `@next/env` 16.4.2, the loader `next dev` calls, in development mode | `.env.development.local`, `.env.local`, `.env.development`, `.env` | `FOO=from_procly` |
| AdonisJS | `@adonisjs/core` 7.6.0 | `Env.create` of `@adonisjs/env` 7.1.0, the class `start/env.ts` calls, with and without `NODE_ENV=development` | `.env.development.local`, `.env.local`, `.env.development`, `.env` | `FOO=from_procly` |

All 3 tools keep a variable already present in the environment over every file they load, so the assumption holds and this decision stands. It replaces the word "unverified" in the Consequences above. The check ran the loaders and not a booted application, so rerun the script when one of these versions changes.

A variable set to the empty string stays empty in dotenv-cli and Next.js, and reads as unset in AdonisJS, which does not fall back to the file value either. A Derived variable that renders to an empty string therefore still wins over the `.env`.

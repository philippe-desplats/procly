# Inject Derived variables into the process, never rewrite the repository's .env

Ports appear inside many URLs of a typical `.env` (API host, CORS origin, OAuth callbacks, WebAuthn origin). Procly computes those as Derived variables from templates in `procly.toml` and injects them into the environment of the processes it starts, mirrored in a git-excluded Env file for tools it does not launch. The repository's own `.env` stays untouched, unlike setup scripts that rewrite it per worktree.

## Consequences

- This relies on AdonisJS, Next.js and `dotenv-cli` not overriding a variable already present in the environment. That assumption is unverified and is tested in the first prototype.

# Procly demo playground.
#
# SOURCE this file (do not execute it), from bash or zsh, so the environment it sets stays in your shell:
#
#     source sample/demo.sh
#
# It builds a throwaway HOME under /tmp holding a copy of sample/project as a git repository with a second
# Worktree, and points Procly at a state directory and a settings directory inside it, so a separate Daemon
# serves the demo and your real ~/.local/state/procly and ~/.config/procly are never touched.
# Run `procly_demo_cleanup` when you are done: it stops that Daemon, with the Services it runs, restores your
# HOME and PATH and removes the temp directory.
#
# No `set -euo pipefail` here on purpose, this file is sourced into your interactive shell and must not change
# its options or exit it on error.

# Resolve the sample/ directory whether sourced from bash or zsh.
if [ -n "${BASH_SOURCE:-}" ]; then
  _procly_src="${BASH_SOURCE[0]}"
elif [ -n "${ZSH_VERSION:-}" ]; then
  _procly_src="${(%):-%x}"
else
  _procly_src="$0"
fi
_PROCLY_SAMPLE_DIR="$(cd "$(dirname "$_procly_src")" && pwd)"
_PROCLY_REPO_DIR="$(dirname "$_PROCLY_SAMPLE_DIR")"
unset _procly_src

# Locate procly: $PROCLY_BIN, else a build of this checkout (built now when cargo is available), else the
# procly on your PATH.
_procly_demo_bin() {
  if [ -n "${PROCLY_BIN:-}" ] && [ -x "$PROCLY_BIN" ]; then
    printf '%s\n' "$PROCLY_BIN"
  elif command -v cargo >/dev/null 2>&1 \
    && (cd "$_PROCLY_REPO_DIR" && cargo build --release --locked --quiet --bin procly) >&2; then
    printf '%s\n' "$_PROCLY_REPO_DIR/target/release/procly"
  elif command -v procly >/dev/null 2>&1; then
    command -v procly
  else
    return 1
  fi
}

if ! _PROCLY_DEMO_BIN="$(_procly_demo_bin)"; then
  printf 'procly demo: no procly binary found, install procly or cargo first\n' >&2
  return 1 2>/dev/null || exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  printf 'procly demo: the sample Services need python3 on the PATH\n' >&2
  return 1 2>/dev/null || exit 1
fi

# A short path under /tmp keeps the Daemon socket below the 104-byte limit of macOS.
_PROCLY_DEMO_ROOT="$(mktemp -d /tmp/procly-demo.XXXXXX)" || return 1 2>/dev/null || exit 1
_PROCLY_DEMO_OLD_HOME="$HOME"
_PROCLY_DEMO_OLD_PATH="$PATH"
_PROCLY_DEMO_OLD_PWD="$PWD"

export HOME="$_PROCLY_DEMO_ROOT"
export PATH="$(dirname "$_PROCLY_DEMO_BIN"):$PATH"
export PROCLY_STATE_DIR="$HOME/.local/state/procly"
export PROCLY_CONFIG_DIR="$HOME/.config/procly"
mkdir -p "$PROCLY_CONFIG_DIR"

# The Main worktree, ~/acme-shop, and a second Worktree on the branch `checkout`, ~/acme-shop-checkout.
cp -R "$_PROCLY_SAMPLE_DIR/project" "$HOME/acme-shop"
(
  cd "$HOME/acme-shop" || exit 1
  git init -q -b main
  git add -A
  git -c user.name=demo -c user.email=demo@example.com commit -q -m "init"
  git worktree add -q -b checkout "$HOME/acme-shop-checkout"
)

# The tool call Claude Code pipes to `procly guard` when an agent runs `npm run dev`, once
# `procly agent install` registered the Guard as a PreToolUse hook.
printf '%s\n' '{"tool_name":"Bash","tool_input":{"command":"npm run dev"}}' > "$HOME/pretooluse.json"

procly_demo_cleanup() {
  cd "$_PROCLY_DEMO_OLD_PWD" 2>/dev/null || cd /
  # There is no stop command: SIGTERM makes the Daemon stop its Services and exit.
  if [ -f "$PROCLY_STATE_DIR/daemon.pid" ]; then
    _procly_pid="$(cat "$PROCLY_STATE_DIR/daemon.pid")"
    case "$(ps -p "$_procly_pid" -o command= 2>/dev/null)" in
      *"procly daemon"*)
        kill -TERM "$_procly_pid" 2>/dev/null
        _procly_tries=0
        while kill -0 "$_procly_pid" 2>/dev/null && [ "$_procly_tries" -lt 100 ]; do
          sleep 0.1
          _procly_tries=$((_procly_tries + 1))
        done
        ;;
    esac
    unset _procly_pid _procly_tries
  fi
  export HOME="$_PROCLY_DEMO_OLD_HOME"
  export PATH="$_PROCLY_DEMO_OLD_PATH"
  unset PROCLY_STATE_DIR PROCLY_CONFIG_DIR
  case "$_PROCLY_DEMO_ROOT" in
    /tmp/procly-demo.*) rm -rf "$_PROCLY_DEMO_ROOT" ;;
  esac
  unset _PROCLY_DEMO_ROOT _PROCLY_DEMO_OLD_HOME _PROCLY_DEMO_OLD_PATH _PROCLY_DEMO_OLD_PWD _PROCLY_DEMO_BIN
  unset _PROCLY_SAMPLE_DIR _PROCLY_REPO_DIR
  unset -f _procly_demo_bin procly_demo_cleanup
}

cd "$HOME/acme-shop" || return 1 2>/dev/null || exit 1

printf '\n  procly demo ready in %s, with its own Daemon\n' "$HOME"
printf '  try:  procly up web  .  cd ../acme-shop-checkout  .  procly status --all  .  procly down\n'
printf '  done: procly_demo_cleanup\n\n'

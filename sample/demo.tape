# Procly demo tape.
#
# Regenerate the README GIF from the repository root with:
#     vhs sample/demo.tape
#
# Requires: vhs (brew install vhs), git, python3, and cargo or procly on your PATH.
# The hidden setup sources sample/demo.sh, which runs a separate Daemon on a throwaway HOME under /tmp, so
# recording never touches your real Procly state. The hidden cleanup stops that Daemon and removes the temp dir.

Output docs/demo.gif

Require git
Require python3

Set Shell "bash"
Set FontSize 18
Set Width 1100
Set Height 720
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 65ms

# --- Hidden setup: isolated demo environment ---
Hide
Type "source sample/demo.sh" Enter
Wait+Screen@5m /procly demo ready/
# Recording-only prompt: show the working directory (cyan) so the move between Worktrees is visible.
Type `PS1="\[\e[36m\]\w\[\e[0m\] \$ "` Enter
Type "clear" Enter
Sleep 500ms
Show

# 1. Start web in the Main worktree: api comes first since web needs it, both on their declared ports.
Type "procly up web" Sleep 600ms Enter
Sleep 2800ms

# 2. Move to a second Worktree of the same repository.
Type "cd ../acme-shop-checkout" Sleep 500ms Enter
Sleep 700ms

# 3. The same command there: its own Leases, other ports from the Range.
Type "procly up web" Sleep 600ms Enter
Sleep 2800ms

# 4. Again: reused, no second process.
Type "procly up web" Sleep 600ms Enter
Sleep 2500ms

# 5. One Daemon sees every Worktree.
Type "procly status --all" Sleep 600ms Enter
Sleep 4500ms

Hide
Type "clear" Enter
Sleep 300ms
Show

# 6. The Guard: what Claude Code's PreToolUse hook hands to `procly guard` when an agent runs `npm run dev`.
Type "# an agent runs npm run dev, its PreToolUse hook asks Procly" Sleep 400ms Enter
Sleep 400ms
Type "cat ~/pretooluse.json" Sleep 500ms Enter
Sleep 1800ms
Type "procly guard < ~/pretooluse.json | python3 -m json.tool" Sleep 600ms Enter
Sleep 5500ms

# 7. Stop this Worktree's Services.
Type "procly down" Sleep 500ms Enter
Sleep 2500ms

# --- Hidden cleanup: stop the demo Daemon and remove the temp dir ---
Hide
Type "procly_demo_cleanup" Enter
Sleep 2500ms

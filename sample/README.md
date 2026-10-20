# Procly demo playground

A self-contained playground to try Procly without touching your real Procly state, and the source of the README demo GIF.

`demo.sh` builds a throwaway `HOME` under `/tmp/procly-demo.XXXXXX` and copies [`project/`](project) there as `~/acme-shop`, a git repository with a second Worktree, `~/acme-shop-checkout`, on the branch `checkout`. It exports `PROCLY_STATE_DIR` and `PROCLY_CONFIG_DIR` inside that directory, so the first command starts a separate Daemon for the demo, and your real `~/.local/state/procly` and `~/.config/procly` are never read or written.

## Try it live

From the repository root, in bash or zsh:

```sh
source sample/demo.sh            # builds procly with cargo, or uses the one on your PATH
procly up web                    # api then web, on the ports procly.toml declares
cd ../acme-shop-checkout
procly up web                    # the same Services on Leases of their own
procly up web                    # reused, no second process
procly status --all              # every Worktree the Daemon knows
procly guard < ~/pretooluse.json | python3 -m json.tool
procly down
procly_demo_cleanup              # stop the demo Daemon, restore HOME and PATH, remove the temp dir
```

`procly_demo_cleanup` matters: Procly v0.1 has no command to stop the Daemon, so the function sends it `SIGTERM`, which stops the Services it still runs before it exits. Set `PROCLY_BIN` to a `procly` binary before sourcing to skip the build.

`~/pretooluse.json` is the tool call Claude Code pipes to `procly guard` when an agent runs `npm run dev`, once `procly agent install` registered the Guard as a `PreToolUse` hook. The Guard denies it and answers with the Procly command to run instead.

## The sample project

```
project/procly.toml    2 Process services, web and api, each a python3 -m http.server on its Lease
project/web/           the page web serves
project/api/           health.json, which the HTTP health check of api requests
project/package.json   the npm run dev an agent would reach for, which the Guard blocks
```

`web` needs `api` and reads its URL through the Derived variable `API_URL`. The Main worktree gets the declared ports, 24080 and 24081, and the second Worktree gets ports from the Range, 24000 and up. The services need nothing but `python3`, no Docker and no Node.js. The names are fictitious.

## Regenerate the README GIF

```sh
brew install vhs            # needs ttyd and ffmpeg too
vhs sample/demo.tape        # writes docs/demo.gif
```

The tape sources `demo.sh` in a hidden setup, records the story, and runs `procly_demo_cleanup` in a hidden step at the end, so no Daemon and no Service outlive the recording.

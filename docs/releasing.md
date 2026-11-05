# Releasing Procly

A release is a tag. The workflow in `.github/workflows/release.yml` does the rest, and nothing else publishes anything.

## Cutting a release

1. Set `version` under `[workspace.package]` in the root `Cargo.toml`, run `cargo build --workspace` so `Cargo.lock` follows, and merge both on `main` once `make check` and the CI are green.
2. Tag that commit `v<version>` and push the tag:

   ```console
   $ git tag v0.1.0
   $ git push origin v0.1.0
   ```

The workflow then runs 4 jobs in order:

| Job | What it does |
| --- | --- |
| `version` | Fails unless the tag is `v` followed by the version of `crates/procly/Cargo.toml`. |
| `build` | Builds `procly` in release mode on a native runner for each of the 4 targets, fails unless `procly --version` prints `procly <version>`, then packs the binary in `procly-<version>-<target>.tar.gz` with its SHA-256. |
| `release` | Gathers the 4 checksums in `SHA256SUMS` and creates the GitHub release with the 4 archives, the checksum file and generated notes. |
| `homebrew` | Renders `Formula/procly.rb` with `scripts/render-formula.sh` and pushes it to the tap. |

A version with a hyphen, such as `0.2.0-rc.1`, is published as a prerelease and never reaches the tap.

The Linux arm64 build runs on a hosted arm runner, which GitHub provides for free to public repositories only.

A failed `build` publishes nothing. A failed `homebrew` leaves the GitHub release in place: fix the cause and run the job again from the Actions page, it only rewrites the formula. A failed `release` after the GitHub release was created cannot be run again: delete the release and its tag on GitHub, then push the tag again.

## One-time setup of the tap

The tap is the repository `philippe-desplats/homebrew-procly`, with the formula at `Formula/procly.rb`. Homebrew resolves `brew install philippe-desplats/procly/procly` to it by name.

1. Create the repository, public and initialised with a README, since the workflow cannot check out a repository that has no commit.
2. Generate an SSH key pair without a passphrase, and add the public key to the tap as a deploy key with write access. A deploy key reaches that one repository only and does not expire.
3. Store the private key as the secret `HOMEBREW_TAP_DEPLOY_KEY` of the Procly repository, then delete the local copy.

   ```console
   $ ssh-keygen -t ed25519 -N "" -C "procly release" -f tap-key
   $ gh repo deploy-key add tap-key.pub --repo philippe-desplats/homebrew-procly --allow-write --title "procly release"
   $ gh secret set HOMEBREW_TAP_DEPLOY_KEY --repo philippe-desplats/procly < tap-key
   $ rm tap-key tap-key.pub
   ```

To rotate the key, run the same commands and delete the old deploy key from the settings of the tap.

Until the secret exists, the `homebrew` job fails with a message saying so, and the rest of the release is unaffected.

## Previewing the formula

`scripts/render-formula.sh <version> <SHA256SUMS>` prints the formula a release would publish and writes nothing. Download `SHA256SUMS` from a release to see the real one.

## Manual checklist

The native surfaces of the desktop app are not automated: a test cannot read the menu bar, the Dock or a login item. Run these steps by hand on a Mac before a release, from a bundled app and not from `tauri dev`, since `LSUIElement` only applies to a bundle. Use a temporary `HOME` or switch Open at login back off, so that no LaunchAgent is left behind.

### Menu bar and Dock

1. Launch the app. The window opens, the Dock icon shows, and the menu bar holds the mark.
2. Stop the Daemon from outside the app, with `procly daemon stop`. Within one poll the glyph becomes the outline of the mark. A single slow answer alone must not change it.
3. Start the Daemon again, and make a Service fail or leave a Foreign process on a Worktree. The glyph becomes the mark with a round bite and a dot. Mark the failure as Seen in the window, and it returns to the mark.
4. Check the 3 glyphs on a light and a dark menu bar, and on a menu bar tinted by a wallpaper. Each follows the system colour like any other template icon, and none shows a count.
5. Open the menu. It lists what needs you with one line per item, a Copy Port submenu, Open Procly, Settings and Quit, and its first line gives the state with the time of the last answer. With no Daemon, the first line says so and gives the time of the last state it knew.
6. Choose a port in Copy Port, and paste it anywhere. Choose Open Procly with the window closed, then Settings, which opens the window on the Settings screen.
7. Close the window with the red button. The Dock icon goes, the app keeps running, and the glyph stays. Click the glyph and choose Open Procly: the icon comes back, then the window shows and takes focus.
8. Close the window again and open the app from the Finder or the Launchpad. This is a reopen: the window shows. Do the same by clicking the Dock icon while another app has the focus.
9. Turn off "Show in Dock while the window is open" in Settings. The Dock icon goes at once, the window stays open, and it takes focus every time it opens with no Dock icon. Turn it on again and close the window, then open it: the icon follows the window.
10. Quit from the menu, then with `⌘Q` on another launch. Both end the app, and the Daemon and its Services keep running.

### Open at login

1. In Settings, "Open at login" is off on a Mac that never had it. Turn it on and check `~/Library/LaunchAgents` for a `com.pdesplats.procly-app` file, and System Settings under Login Items.
2. Log out and in. The app starts with no window and no Dock icon, only the glyph, and it has not started the Daemon.
3. Remove the app from Login Items in System Settings. Open Settings again: the switch reads off, since its state comes from the system and is never stored.
4. Start the app by hand with the switch on: it opens its window, since only the login launch passes `--hidden`.
5. Turn the switch off, and check that the file in `~/Library/LaunchAgents` is gone.

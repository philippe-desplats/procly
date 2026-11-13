# Releasing Procly

A release is a tag. The workflow in `.github/workflows/release.yml` does the rest, and nothing else publishes anything.

## Cutting a release

1. Set `version` under `[workspace.package]` in the root `Cargo.toml`, run `cargo build --workspace` so `Cargo.lock` follows, and merge both on `main` once `make check` and the CI are green.
2. Run the [manual checklist](#manual-checklist) on the signed `.dmg` of the release candidate. Nothing automated proves the native surfaces of the app.
3. Tag that commit `v<version>` and push the tag:

   ```console
   $ git tag v0.1.0
   $ git push origin v0.1.0
   ```

The workflow then runs 5 jobs. `app` runs beside `build`, and `release` waits for both:

| Job | What it does |
| --- | --- |
| `version` | Fails unless the tag is `v` followed by the version of `crates/procly/Cargo.toml`. |
| `build` | Builds `procly` in release mode on a native runner for each of the 4 targets, fails unless `procly --version` prints `procly <version>`, then packs the binary in `procly-<version>-<target>.tar.gz` with its SHA-256. |
| `app` | Runs on the 2 macOS runners. Fails at once, naming every missing secret, unless the [Apple secrets](#one-time-setup-of-the-apple-signing) are set. Otherwise builds the bundle with `make bundle`, signs and notarizes it, checks it and packs `Procly-<version>-<target>.dmg` with its SHA-256. |
| `release` | Gathers the checksums in `SHA256SUMS` and creates the GitHub release with the 4 archives, the 2 disk images when the app was built, the checksum file and generated notes. |
| `homebrew` | Renders `Formula/procly.rb` with `scripts/render-formula.sh` and `Casks/procly-app.rb` with `scripts/render-cask.sh` and pushes both to the tap. |

A version with a hyphen, such as `0.2.0-rc.1`, is published as a prerelease with its archives and its disk images, and never reaches the tap: the `homebrew` job does not run, so neither the formula nor the cask moves. `scripts/render-cask.sh` refuses a prerelease version as well.

The Linux arm64 build runs on a hosted arm runner, which GitHub provides for free to public repositories only.

A failed `build` publishes nothing. A failed `app` job does not stop the command line: `release` publishes the 4 archives without the disk images, the cask stays on the version before, and the run ends red with the cause in the log. A release never carries one disk image without the other, nor a bundle that is not signed and notarized, since the build does not start without the credentials. A failed `homebrew` leaves the GitHub release in place: fix the cause and run the job again from the Actions page, it only rewrites the formula and the cask. A failed `release` after the GitHub release was created cannot be run again: delete the release and its tag on GitHub, then push the tag again.

To publish the disk images after the secrets were fixed, delete the release and its tag on GitHub and push the tag again, the `release` job cannot add files to a release that exists.

## The app bundle

`make bundle PROFILE=release TAURI_ARGS="--bundles app,dmg"` builds the bundle, once per architecture, each on the native runner of the command line. The command line is embedded as the sidecar of the app, `Procly.app/Contents/MacOS/procly`, so the bundle must come from `make bundle`: the sidecar is declared in `apps/desktop/src-tauri/tauri.bundle.conf.json` and a plain `tauri build` yields a bundle without it. There is no universal bundle, `bundle.macOS.minimumSystemVersion` is `13.0`, and the hardened runtime is the one Tauri enables, with no entitlements file until notarization or the first signed launch asks for one.

Before the disk image is published, the `app` job checks the bundle:

- `scripts/check-bundle.sh` fails when the executable carries the end-to-end test driver. The `e2e` feature of the app is off by default, the build never asks for it, and the job also fails if a manifest ever makes it a default feature. A release must never open a WebDriver channel on the window of every user.
- The sidecar reports `procly <version>`.
- `codesign --verify --deep --strict`, `xcrun stapler validate` and `spctl --assess` accept the bundle, so it is signed with the Developer ID certificate, notarized and stapled.

The disk image is named `Procly-<version>-<target>.dmg` and its SHA-256 goes in `SHA256SUMS` beside the 4 archives. There is no updater: no updater key, no `latest.json`, and updates go through `brew upgrade`.

## One-time setup of the Apple signing

The app jobs need an Apple Developer account, individual or organization, and its Developer ID Application certificate. Until the secrets exist, the `app` jobs fail on their first step with a message naming every secret they lack, and the rest of the release is unaffected.

The signing needs 3 secrets:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | The Developer ID Application certificate exported from Keychain Access as a `.p12` file, base64 encoded. |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when the `.p12` file was exported. |
| `KEYCHAIN_PASSWORD` | Any password, it protects the temporary keychain the job creates and deletes. |

The job imports the certificate into that temporary keychain and finds the identity by its `Developer ID Application` prefix, so the secrets do not carry the name of the team.

Notarization needs one of 2 complete sets. The API key set is the first choice, and the Apple ID set is used only when the API key set is incomplete:

| Set | Secrets |
| --- | --- |
| App Store Connect Team Key | `APPLE_API_ISSUER`, the issuer id shown above the keys table. `APPLE_API_KEY`, the Key ID of the key. `APPLE_API_KEY_CONTENT`, the text of the downloaded `AuthKey_<id>.p8` file. |
| Apple ID | `APPLE_ID`, the email of the account. `APPLE_PASSWORD`, an app-specific password of that account. `APPLE_TEAM_ID`, the Team ID of the membership page. |

Tauri reads the private key of the first set from a file named by `APPLE_API_KEY_PATH`. A secret cannot be a path, so the job writes `APPLE_API_KEY_CONTENT` to a file in the runner temporary directory, sets `APPLE_API_KEY_PATH` to it and deletes it at the end. Only the variables of the chosen set reach the build.

```console
$ base64 -i certificate.p12 | gh secret set APPLE_CERTIFICATE --repo philippe-desplats/procly
$ gh secret set APPLE_CERTIFICATE_PASSWORD --repo philippe-desplats/procly
$ openssl rand -base64 24 | gh secret set KEYCHAIN_PASSWORD --repo philippe-desplats/procly
$ gh secret set APPLE_API_ISSUER --repo philippe-desplats/procly
$ gh secret set APPLE_API_KEY --repo philippe-desplats/procly
$ gh secret set APPLE_API_KEY_CONTENT --repo philippe-desplats/procly < AuthKey_ABC123DEFG.p8
```

For the Apple ID set, set `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` the same way. Delete the local `.p12` and `.p8` files once the secrets are stored. To check the secrets without a release, run the `app` job logic by hand: `scripts/check-apple-secrets.sh` prints the missing names when run with the secrets in the environment.

## One-time setup of the tap

The tap is the repository `philippe-desplats/homebrew-procly`, with the formula at `Formula/procly.rb` and the cask of the app at `Casks/procly-app.rb`. Homebrew resolves `brew install philippe-desplats/procly/procly` to the formula by name, and `brew install --cask philippe-desplats/procly/procly-app` to the cask. The cask has a token of its own because how Homebrew resolves `procly` when a formula and a cask share a tap is not confirmed.

The formula stays for Linux and for the command line alone on macOS. The cask installs `Procly.app` and links the command line it carries, so install one or the other. The cask declares no `conflicts_with`: Homebrew 7 accepts only `cask:` keys there and refuses to load a cask that names a formula, with `Unknown key: :formula. Valid keys are: :cask`. Homebrew links the `binary` of a cask through `Cask::Artifact::Symlinked#link_action` in `Library/Homebrew/cask/artifact/symlinked.rb`: when the target already exists as a symlink into the Cellar, `conflicting_formula` names its formula and the link is skipped with a warning, `It seems there is already a Binary at '<prefix>/bin/procly' from formula procly; skipping link.`, while the app itself is installed. `--force` does not change it, since it only replaces a link that points into the Caskroom, and uninstalling the cask leaves that link to the formula in place. This is read from the source of Homebrew 7.0.13, not measured.

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

## Previewing the formula and the cask

`scripts/render-formula.sh <version> <SHA256SUMS>` prints the formula a release would publish and `scripts/render-cask.sh <version> <SHA256SUMS>` prints the cask, and neither writes anything. Download `SHA256SUMS` from a release to see the real ones. The cask needs the 2 `.dmg` lines in the file and refuses a prerelease version.

`scripts/test-release-scripts.sh` tests the scripts the release workflow runs, the cask rendering, the check of the bundle and the check of the Apple secrets. It needs nothing but bash, and `make test-release-scripts` runs it.

## Manual checklist

Nothing automated drives the native surfaces of the app: the menu bar, the notifications, the Dock and the native sheets. Run this checklist on the signed `.dmg` of the release candidate, on the architecture of the machine, before each tag. Mount the image, copy the app to `/Applications` and open it from there, the way a user does.

1. Gatekeeper opens the app with no warning and no right click, and `spctl --assess --type execute --verbose=2 /Applications/Procly.app` says `accepted` with the source `Notarized Developer ID`.
2. The menu bar icon shows the mark when the Daemon answers and nothing needs you, the mark with a bite and a dot when a Service needs you, and the outline when the Daemon cannot be reached. Each glyph reads well in the light and the dark menu bar. The icon height is the one the design asks for on the first run.
3. The native menu lists only what needs you, a Copy Port submenu, Open Procly, Settings and Quit, one line per item, with the last known state and its absolute time.
4. A notification reaches the screen end to end with banners allowed in the macOS notification settings: turning on "Notify on failure" in Settings asks for the permission, then with the window in the background a Service that fails makes a banner, and clicking it opens the window on the item. The first answer after launch makes no banner. With the setting off nothing is asked at launch and no banner appears.
5. The Dock icon is absent at launch, shown while the window is open, hidden when the last window closes, and never shown when "Show in Dock while the window is open" is off. Clicking the app again, with the window closed, reopens the window (Reopen).
6. Open at login adds the login item. After a login, or a launch with `--hidden`, the app opens no window, shows only the menu bar glyph and does not start the Daemon. Turning the setting off removes the item.
7. Open with opens a Worktree in the editor, the terminal and the Finder, and `open -a <terminal> <Worktree path>` opens a shell in that directory for Terminal, iTerm and Ghostty.
8. The window opens again where it was left after a quit and a launch.
9. The accent colour follows the system accent colour in the window, which proves `-apple-system-control-accent` in the web view.
10. The confirmed Daemon restart: with a Daemon of another version running, the app names both versions, a click on Restart asks for confirmation, Cancel restarts nothing, and a confirmation stops the Daemon through the sidecar and starts the one of the bundle. The window recovers on its own.
11. The launch from Homebrew: on a machine without the app, after `brew trust philippe-desplats/procly`, `brew install --cask philippe-desplats/procly/procly-app` installs `Procly.app` and links the command line, `procly --version` prints `procly <version>`, the app opens from Launchpad, and `brew uninstall --cask procly-app` removes both. Installing the cask while the formula `procly` is linked installs the app, prints the warning that ends in `skipping link`, and leaves `procly` pointing at the formula, as read from the Homebrew source in "One-time setup of the tap" above.
12. The upgrade from the previous release through `brew upgrade --cask procly-app`, with a Daemon started from the previous bundle and one running Service: the app shows the Daemon of another version, `procly logs` of a Service started after the upgrade still shows its output, and the restart brings the window back, see the next section.

Items 2, 3, 4, 5 and 6 are played step by step below for the menu bar, the glyphs, the notifications, the Dock, Reopen and Open at login. Run them from a bundled app and not from `tauri dev`, since `LSUIElement` only applies to a bundle. Use a temporary `HOME` or switch Open at login back off, so that no LaunchAgent is left behind.

### Menu bar and Dock

1. Launch the app. The window opens, the Dock icon shows, and the menu bar holds the mark.
2. Stop the Daemon from outside the app, with `procly daemon stop`. Within one poll the glyph becomes the outline of the mark. A single slow answer alone must not change it.
3. Start the Daemon again, and make a Service fail or leave a Foreign process on a Worktree. The glyph becomes the mark with a round bite and a dot. Mark the failure as Seen in the window, and it returns to the mark.
4. Check the 3 glyphs on a light and a dark menu bar, and on a menu bar tinted by a wallpaper. Each follows the system colour like any other template icon, and none shows a count.
5. Open the menu. It lists what needs you with one line per item, a Copy Port submenu, Open Procly, Settings and Quit, and its first line gives the state with the time of the last answer. With no Daemon, the first line says so and gives the time of the last state it knew.
6. Choose a port in Copy Port, and paste it anywhere. Choose Open Procly with the window closed, then Settings, which opens the window on the Settings screen.
7. Close the window with the red button. The Dock icon goes, the app keeps running, and the glyph stays. Click the glyph and choose Open Procly: the icon comes back, then the window shows and takes focus.
8. Close the window again and open the app from the Finder or the Launchpad. This is a reopen: the window shows. Do the same by clicking the Dock icon while another app has the focus.
9. Turn off "Show in Dock while the window is open" in Settings. The Dock icon goes at once, the window stays open and keeps the focus, and it takes focus every time it opens with no Dock icon. Do it a first time within a second of the window opening, on a fresh launch: the icon goes a moment later, since the system ignores a hide that comes too soon after a show. Turn it on again and close the window, then open it: the icon follows the window.
10. Quit from the menu, then with `⌘Q` on another launch. Both end the app, and the Daemon and its Services keep running.

### Notifications

Allow banners for Procly in System Settings > Notifications first, since macOS decides whether a banner shows and the app cannot read that choice. The permission calls of the app always answer granted on desktop.

1. Launch the app with the Daemon running, a Service failed in a Worktree and "Notify on failure" off. No prompt appears at launch and no banner shows, whatever the state.
2. Put another app in front, then make a second Service fail. No banner shows, since the setting is off.
3. In Settings, the "Notifications" section holds the "Notify on failure" switch, off, a line saying that macOS decides whether the banner shows, and an "Open notification settings" button. The button opens the Notifications pane of System Settings.
4. Turn the switch on. It stays on. If macOS asks to allow notifications for Procly at the first banner, allow them.
5. With another app in front, make a Service fail. A banner names the Service, how it ended and the branch of its Worktree, and the 2 failures that were already there make none. Click the banner: the window opens on that Worktree.
6. Make 4 Services fail within one poll, for example from one Worktree. One banner says that 4 failures need you, and a click on it opens the window on the Overview. Fewer than 4 failures make a banner each.
7. Leave a Foreign process running on a Worktree. It makes no banner.
8. Stop the Daemon from outside the app, then start it again with a failed Service. The first answer of the restarted Daemon makes no banner, and a failure after it does. Quit and launch the app again with the setting on: the failures already there make no banner.
9. Mark a failure as Seen in the window, then make another Service fail. Only the new failure makes a banner.
10. Turn the switch off, and make a Service fail. No banner shows.

### Open at login

1. In Settings, "Open at login" is off on a Mac that never had it. Turn it on and check `~/Library/LaunchAgents` for a `com.pdesplats.procly-app` file, and System Settings under Login Items.
2. Log out and in. The app starts with no window and no Dock icon, only the glyph, and it has not started the Daemon.
3. Remove the app from Login Items in System Settings. Open Settings again: the switch reads off, since its state comes from the system and is never stored.
4. Start the app by hand with the switch on: it opens its window, since only the login launch passes `--hidden`.
5. Turn the switch off, and check that the file in `~/Library/LaunchAgents` is gone.

Tick the list in the release pull request or the ticket of the release, and fix before tagging whatever fails. A tag whose bundle fails an item is deleted and cut again.

## A Daemon that outlives an upgrade

The Daemon runs its output writer, `procly output-file <path>`, each time a Service starts (`OutputFiles::start_writer` in `crates/procly/src/daemon/output_file.rs`). Inside the app, the Daemon is the sidecar `Procly.app/Contents/MacOS/procly`, so what the writer runs from matters once a cask upgrade replaces the bundle.

The finding, measured on macOS 26 with a probe program: on macOS `std::env::current_exe()` answers the path a process was started from and not the file it runs. A process started from `app/probe`, whose file was then removed and replaced by a different executable the way a cask upgrade replaces `Procly.app`, answered the same path afterwards, and the file at that path was the replacement. A Daemon that spawned `current_exe()` for each writer would therefore run the executable of version N+1 as the writer of a Service started after `brew upgrade --cask procly-app`. The formula is the other case: it installs under a versioned Cellar path that `brew upgrade` removes, so the path may disappear and not be replaced. The probe was not run on Linux, where `current_exe()` reads `/proc/self/exe`, which names a removed file with a `(deleted)` suffix.

The rule:

1. The Daemon copies its own executable to `daemon-executable` in its state directory when it starts, before it serves anything, and starts every helper from that copy, never from `current_exe()` or from a path on the `PATH`. It runs the copy once with `--version` first, since the first run of a new file takes about half a second on macOS, and the writer of the first Service would otherwise start late. A link is not used: on macOS, running a file while another name of it is linked or unlinked killed the process with `SIGKILL` in 11 runs of 3202. The copy is a new file for each Daemon and is removed when the Daemon exits cleanly, so a helper that outlives its Daemon keeps running from its own file. The copy is taken at start, a few milliseconds after the launch, from the file then at the path the Daemon was started from, and not from the image it started from. Replacing or removing that file after the copy changes nothing for the Daemon, and `tests/daemon_executable.rs` holds both cases. The only case left is an upgrade that replaces the file inside that window, which gives the Daemon the copy of the new release while the Daemon itself runs the old one. A restart of the Daemon fixes it.
2. The argument of `procly output-file` and what the Service writes on its standard input are no longer a contract between 2 releases: a release may change them without a bump of `PROTOCOL_VERSION`, since a writer is started by a Daemon of its own release, the window of point 1 apart.
3. The layout of the output file and its `.prev` file stays a contract between 2 releases. A Daemon that crashes or restarts re-attaches the Services of the previous one, with the writers of that release still appending to their files, and the new Daemon reads them. A change to that layout ships with a bump of `PROTOCOL_VERSION` and a line in the release notes saying that the Daemon must be restarted, with the Services stopped first. The bump makes the app show the Daemon of another version, and `procly doctor` report it with the command that fixes it.
4. Restart the Daemon after an upgrade to run the new release, which the app offers from the confirmed sheet and `procly daemon stop` does from a shell. The cask does not stop the Daemon itself, since that would stop every Service of every Worktree on a package upgrade, which nobody asked for. The restart is no longer what keeps the helpers matching the Daemon.
5. When the copy is gone, for example because something cleaned the state directory under the Daemon, the Service still starts and appends to its output file directly. The Daemon log names the path it could not run and tells to restart the Daemon with `procly daemon stop`.
6. Checklist item 12 proves the path of an upgrade with a Daemon of the previous release.

Not measured here: the behaviour after a real `brew upgrade` of the formula, which needs a published release. It is recorded on the ticket #136 during the installation of 0.5 on a real project. The removal of the executable is held by the test above.

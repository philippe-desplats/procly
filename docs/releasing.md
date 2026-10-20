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

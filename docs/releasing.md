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

A failed `build` publishes nothing. A failed `homebrew` leaves the GitHub release in place: fix the cause and run the job again from the Actions page, it only rewrites the formula.

## One-time setup of the tap

The tap is the repository `philippe-desplats/homebrew-procly`, with the formula at `Formula/procly.rb`. Homebrew resolves `brew install philippe-desplats/procly/procly` to it by name.

1. Create the repository, empty and public.
2. Create a fine-grained personal access token limited to that repository, with the `Contents` permission set to read and write.
3. Store it as the secret `HOMEBREW_TAP_TOKEN` in the settings of the Procly repository.

Until the secret exists, the `homebrew` job fails with a message saying so, and the rest of the release is unaffected.

## Previewing the formula

`scripts/render-formula.sh <version> <SHA256SUMS>` prints the formula a release would publish and writes nothing. Download `SHA256SUMS` from a release to see the real one.

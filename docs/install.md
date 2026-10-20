# Installing Procly

Procly is one binary, `procly`, for macOS (Apple silicon and Intel) and Linux (x86_64 and arm64). It needs no runtime. The Daemon starts on demand, from the first command that needs it, see [The Daemon across crashes and sessions](daemon.md).

## Homebrew

```console
$ brew install philippe-desplats/procly/procly
$ procly --version
procly 0.2.0
```

The formula lives in the tap `philippe-desplats/homebrew-procly` and installs the binary of the matching GitHub release. `brew upgrade procly` moves to a newer release.

## GitHub Releases

Each release carries one archive per platform and a `SHA256SUMS` file:

| Platform | Archive |
| --- | --- |
| macOS, Apple silicon | `procly-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `procly-<version>-x86_64-apple-darwin.tar.gz` |
| Linux, arm64 | `procly-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Linux, x86_64 | `procly-<version>-x86_64-unknown-linux-gnu.tar.gz` |

The Linux archives are built on Ubuntu 22.04 and link the system glibc, so they run on distributions that ship glibc 2.35 or newer.

```console
$ version=0.2.0
$ target=aarch64-apple-darwin
$ base=https://github.com/philippe-desplats/procly/releases/download/v$version
$ curl -fsSLO $base/procly-$version-$target.tar.gz
$ curl -fsSLO $base/SHA256SUMS
$ grep " procly-$version-$target.tar.gz\$" SHA256SUMS | shasum -a 256 --check
procly-0.2.0-aarch64-apple-darwin.tar.gz: OK
$ tar -xzf procly-$version-$target.tar.gz
$ install procly-$version-$target/procly /usr/local/bin/procly
```

On Linux, `sha256sum --check` replaces `shasum -a 256 --check`.

## cargo

```console
$ cargo install --locked --git https://github.com/philippe-desplats/procly --tag v0.2.0 --bin procly
```

`--tag` installs a release, leave it out to install the head of the default branch. `--bin procly` leaves out `procly-test-server`, a fixture of the tests that the crate also builds. From a clone, `cargo install --locked --path crates/procly --bin procly` does the same. A compiler older than the `rust-version` of the crate is refused.

## Checking the installation

`procly --version` prints `procly` and the version of the release, the same string as the tag without its `v`. A binary installed through Homebrew, from a release archive, or by `cargo install --tag` reports the same version.

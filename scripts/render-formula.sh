#!/usr/bin/env bash
# Prints the Homebrew formula of a release on stdout.
#
# Usage: scripts/render-formula.sh <version> <SHA256SUMS>
#
# <SHA256SUMS> is the checksum file of the release, one `<sha256>  procly-<version>-<target>.tar.gz` line per
# archive. The release workflow runs this script and pushes its output to the tap, see docs/releasing.md.
# It reads and prints only, so it can be run anywhere to see the formula a release would publish.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <version> <SHA256SUMS>" >&2
  exit 2
fi

version="$1"
sums="$2"
repository="philippe-desplats/procly"

# The checksum of the archive built for a target, failing when the release does not carry it.
checksum() {
  local archive="procly-${version}-$1.tar.gz"
  local sha
  sha="$(awk -v archive="${archive}" '$2 == archive { print $1 }' "${sums}")"
  if ! [[ "${sha}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "no sha256 for ${archive} in ${sums}" >&2
    exit 1
  fi
  printf '%s' "${sha}"
}

# The download URL of the archive built for a target.
url() {
  printf 'https://github.com/%s/releases/download/v%s/procly-%s-%s.tar.gz' "${repository}" "${version}" "${version}" "$1"
}

macos_arm="aarch64-apple-darwin"
macos_intel="x86_64-apple-darwin"
linux_arm="aarch64-unknown-linux-gnu"
linux_intel="x86_64-unknown-linux-gnu"

# Resolved before the heredoc, since a failure inside a command substitution there would not stop the script.
sha_macos_arm="$(checksum "${macos_arm}")"
sha_macos_intel="$(checksum "${macos_intel}")"
sha_linux_arm="$(checksum "${linux_arm}")"
sha_linux_intel="$(checksum "${linux_intel}")"

cat <<FORMULA
class Procly < Formula
  desc "Supervisor so coding agents never duplicate processes, containers or ports"
  homepage "https://github.com/${repository}"
  version "${version}"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "$(url "${macos_arm}")"
      sha256 "${sha_macos_arm}"
    end
    on_intel do
      url "$(url "${macos_intel}")"
      sha256 "${sha_macos_intel}"
    end
  end

  on_linux do
    on_arm do
      url "$(url "${linux_arm}")"
      sha256 "${sha_linux_arm}"
    end
    on_intel do
      url "$(url "${linux_intel}")"
      sha256 "${sha_linux_intel}"
    end
  end

  def install
    bin.install "procly"
  end

  test do
    assert_match "procly #{version}", shell_output("#{bin}/procly --version")
  end
end
FORMULA

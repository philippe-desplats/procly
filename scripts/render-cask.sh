#!/usr/bin/env bash
# Prints the Homebrew cask of the desktop app for a release on stdout.
#
# Usage: scripts/render-cask.sh <version> <SHA256SUMS>
#
# <SHA256SUMS> is the checksum file of the release, one `<sha256>  Procly-<version>-<target>.dmg` line per bundle.
# The release workflow runs this script and pushes its output to the tap, see docs/releasing.md. It reads and prints
# only, so it can be run anywhere to see the cask a release would publish. A prerelease is refused, since a
# prerelease never reaches `brew install`.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <version> <SHA256SUMS>" >&2
  exit 2
fi

version="$1"
sums="$2"
repository="philippe-desplats/procly"

case "${version}" in
  *-*)
    echo "${version} is a prerelease, the cask only follows a release" >&2
    exit 1
    ;;
esac

# The checksum of the bundle built for a target, failing when the release does not carry it.
checksum() {
  local bundle="Procly-${version}-$1.dmg"
  local sha
  sha="$(awk -v bundle="${bundle}" '$2 == bundle { print $1 }' "${sums}")"
  if ! [[ "${sha}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "no sha256 for ${bundle} in ${sums}" >&2
    exit 1
  fi
  printf '%s' "${sha}"
}

arm="aarch64-apple-darwin"
intel="x86_64-apple-darwin"

# Resolved before the heredoc, since a failure inside a command substitution there would not stop the script.
sha_arm="$(checksum "${arm}")"
sha_intel="$(checksum "${intel}")"

# The URL keeps `#{version}` for Homebrew to expand, so the cask reads like the ones people write by hand.
cat <<CASK
cask "procly-app" do
  version "${version}"

  on_arm do
    sha256 "${sha_arm}"
    url "https://github.com/${repository}/releases/download/v#{version}/Procly-#{version}-${arm}.dmg"
  end
  on_intel do
    sha256 "${sha_intel}"
    url "https://github.com/${repository}/releases/download/v#{version}/Procly-#{version}-${intel}.dmg"
  end

  name "Procly"
  desc "Desktop app for the Procly supervisor, with the procly command line"
  homepage "https://github.com/${repository}"

  depends_on macos: :ventura

  app "Procly.app"
  # The command line is the sidecar of the app, the one the app itself starts the Daemon with.
  binary "#{appdir}/Procly.app/Contents/MacOS/procly"
end
CASK

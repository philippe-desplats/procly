#!/usr/bin/env bash
# Tests the scripts the release workflow runs: scripts/render-cask.sh and scripts/check-bundle.sh.
#
# Usage: scripts/test-release-scripts.sh
#
# Runs on macOS and Linux with nothing but bash, awk and grep, and writes only under a temporary directory.

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

failures=0

# Reports a check and counts it as failed when its condition does not hold.
check() {
  local what="$1"
  shift
  if "$@"; then
    echo "ok   ${what}"
  else
    echo "FAIL ${what}"
    failures=$((failures + 1))
  fi
}

arm_sha="$(printf 'a%.0s' {1..64})"
intel_sha="$(printf 'b%.0s' {1..64})"
cat > "${work}/SHA256SUMS" <<SUMS
${arm_sha}  Procly-1.2.3-aarch64-apple-darwin.dmg
${intel_sha}  Procly-1.2.3-x86_64-apple-darwin.dmg
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  procly-1.2.3-aarch64-apple-darwin.tar.gz
SUMS

cask="$(bash "${here}/render-cask.sh" 1.2.3 "${work}/SHA256SUMS")"

check "the cask is named procly-app" grep -q '^cask "procly-app" do$' <<< "${cask}"
# The lines of a `do` block of the cask, from its opening line to its `end`.
block() {
  awk -v opening="  $1 do" '$0 == opening { inside = 1; next } inside && $0 == "  end" { exit } inside { print }' <<< "${cask}"
}
check "the cask has an on_arm block with its own sha256" grep -q "sha256 \"${arm_sha}\"" <<< "$(block on_arm)"
check "the cask has an on_arm block with its own url" \
  grep -q 'url "[^"]*Procly-#{version}-aarch64-apple-darwin\.dmg"' <<< "$(block on_arm)"
check "the cask has an on_intel block with its own sha256" grep -q "sha256 \"${intel_sha}\"" <<< "$(block on_intel)"
check "the cask has an on_intel block with its own url" \
  grep -q 'url "[^"]*Procly-#{version}-x86_64-apple-darwin\.dmg"' <<< "$(block on_intel)"
check "the cask installs the app" grep -q '^  app "Procly.app"$' <<< "${cask}"
check "the cask links the command line from the bundle" \
  grep -q '^  binary "#{appdir}/Procly.app/Contents/MacOS/procly"$' <<< "${cask}"
check "the cask conflicts with the formula" grep -q '^  conflicts_with formula: "procly"$' <<< "${cask}"
check "the cask needs macOS 13" grep -q '^  depends_on macos: :ventura$' <<< "${cask}"
check "the cask names the version once" test "$(grep -c '^  version "1.2.3"$' <<< "${cask}")" -eq 1

check "a prerelease is refused" bash -c '! bash "$0" 1.2.3-rc.1 "$1" 2> /dev/null' "${here}/render-cask.sh" "${work}/SHA256SUMS"
check "a release without the bundle of an architecture is refused" bash -c '
  grep -v x86_64 "$1" > "$2"
  ! bash "$0" 1.2.3 "$2" 2> /dev/null' "${here}/render-cask.sh" "${work}/SHA256SUMS" "${work}/short"
check "a bad usage exits 2" bash -c 'bash "$0" > /dev/null 2>&1; test "$?" -eq 2' "${here}/render-cask.sh"

# A bundle laid out like the one Tauri builds, with an executable that may carry the plugin of the e2e feature.
bundle() {
  mkdir -p "${work}/$1/Contents/MacOS"
  printf 'plain executable\0panic in tauri-plugin-opener-2.5.0/src/lib.rs\n' > "${work}/$1/Contents/MacOS/procly-desktop"
}

bundle clean.app
bundle driven.app
printf 'panic in tauri-plugin-wdio-webdriver-0.2.0/src/lib.rs\n' >> "${work}/driven.app/Contents/MacOS/procly-desktop"

check "a bundle without the test driver passes" bash -c 'bash "$0" "$1" > /dev/null' "${here}/check-bundle.sh" "${work}/clean.app"
check "a bundle with the test driver is refused" bash -c '! bash "$0" "$1" > /dev/null 2>&1' "${here}/check-bundle.sh" "${work}/driven.app"
check "a missing bundle is refused" bash -c '! bash "$0" "$1" > /dev/null 2>&1' "${here}/check-bundle.sh" "${work}/absent.app"

# Runs scripts/check-apple-secrets.sh with exactly the secrets named in the arguments set, standard output in
# ${work}/secrets.out and the exit status as the status of the call.
with_secrets() {
  local environment=(PATH="${PATH}" GITHUB_OUTPUT="${work}/secrets.out")
  local name
  for name in "$@"; do
    environment+=("${name}=value")
  done
  : > "${work}/secrets.out"
  env -i "${environment[@]}" bash "${here}/check-apple-secrets.sh" > "${work}/secrets.log" 2>&1
}

signing=(APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD KEYCHAIN_PASSWORD)
api=(APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_CONTENT)
apple_id=(APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID)

# Each of these succeeds when the check behaves as the workflow relies on.
fails_naming() {
  ! with_secrets "${@:2}" && grep -q "$1" "${work}/secrets.log"
}
passes_with() {
  with_secrets "${@:2}" && grep -q "^method=$1\$" "${work}/secrets.out"
}

check "no secret at all fails" fails_naming "::error::"
check "no secret at all names the signing secrets" fails_naming "APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD and KEYCHAIN_PASSWORD"
check "no secret at all names both notarization sets" fails_naming "either APPLE_API_ISSUER, APPLE_API_KEY and APPLE_API_KEY_CONTENT (missing APPLE_API_ISSUER, APPLE_API_KEY and APPLE_API_KEY_CONTENT) or APPLE_ID"
check "a missing certificate password is the only one named" fails_naming "missing the signing secrets APPLE_CERTIFICATE_PASSWORD, see" \
  APPLE_CERTIFICATE KEYCHAIN_PASSWORD "${api[@]}"
check "half of a notarization set is not enough" fails_naming "(missing APPLE_API_KEY_CONTENT)" \
  "${signing[@]}" APPLE_API_ISSUER APPLE_API_KEY APPLE_ID
check "the signing secrets alone do not notarize" fails_naming "one complete notarization set" "${signing[@]}"
check "the API set notarizes" passes_with api "${signing[@]}" "${api[@]}"
check "the Apple ID set notarizes" passes_with apple-id "${signing[@]}" "${apple_id[@]}"
check "the API set wins when both are there" passes_with api "${signing[@]}" "${api[@]}" "${apple_id[@]}"
check "an empty secret counts as missing" bash -c '! env -i PATH="$PATH" GITHUB_OUTPUT=/dev/null APPLE_CERTIFICATE= bash "$0" > /dev/null' "${here}/check-apple-secrets.sh"

if [ "${failures}" -ne 0 ]; then
  echo "${failures} check(s) failed" >&2
  exit 1
fi

#!/usr/bin/env bash
# Checks that the Apple secrets the app jobs of the release workflow need are all there, and names the ones that are not.
#
# Usage: scripts/check-apple-secrets.sh
#
# Reads the secrets from the environment, prints nothing about their values. On success it writes `method=api` or
# `method=apple-id` to $GITHUB_OUTPUT (standard output when unset), the notarization method the credentials allow. On
# failure it prints a `::error::` line naming every missing secret and exits 1, so the app jobs fail before they build
# anything while the jobs that release the command line go through, see docs/releasing.md.

set -euo pipefail

output="${GITHUB_OUTPUT:-/dev/stdout}"

signing=(APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD KEYCHAIN_PASSWORD)
api=(APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_CONTENT)
apple_id=(APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID)

# The names among its arguments whose secret is empty or unset, one per line.
absent() {
  local name
  for name in "$@"; do
    if [ -z "${!name:-}" ]; then
      printf '%s\n' "${name}"
    fi
  done
}

# A list of names as `A, B and C`.
join() {
  local names=("$@")
  local count="${#names[@]}"
  local list="" index
  for ((index = 0; index < count; index++)); do
    if [ "${index}" -eq 0 ]; then
      list="${names[index]}"
    elif [ "${index}" -eq $((count - 1)) ]; then
      list="${list} and ${names[index]}"
    else
      list="${list}, ${names[index]}"
    fi
  done
  printf '%s' "${list}"
}

# Splits the lines of `absent` into an array, an empty array when there are none.
lines() {
  local line
  while IFS= read -r line; do
    if [ -n "${line}" ]; then
      printf '%s\n' "${line}"
    fi
  done
}

missing_signing=()
while IFS= read -r name; do missing_signing+=("${name}"); done < <(absent "${signing[@]}" | lines)
missing_api=()
while IFS= read -r name; do missing_api+=("${name}"); done < <(absent "${api[@]}" | lines)
missing_apple_id=()
while IFS= read -r name; do missing_apple_id+=("${name}"); done < <(absent "${apple_id[@]}" | lines)

problems=()
if [ "${#missing_signing[@]}" -gt 0 ]; then
  problems+=("the signing secrets $(join "${missing_signing[@]}")")
fi

method=""
if [ "${#missing_api[@]}" -eq 0 ]; then
  method="api"
elif [ "${#missing_apple_id[@]}" -eq 0 ]; then
  method="apple-id"
else
  problems+=("one complete notarization set, either $(join "${api[@]}") (missing $(join "${missing_api[@]}")) or $(join "${apple_id[@]}") (missing $(join "${missing_apple_id[@]}"))")
fi

if [ "${#problems[@]}" -gt 0 ]; then
  echo "::error::the app is not built and not published, missing $(join "${problems[@]}"), see docs/releasing.md"
  exit 1
fi

echo "method=${method}" >> "${output}"

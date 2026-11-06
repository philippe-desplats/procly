#!/usr/bin/env bash
# Fails when a macOS app bundle carries the end-to-end test driver.
#
# Usage: scripts/check-bundle.sh <Procly.app>
#
# The `e2e` feature of the desktop app (ADR 0016) links the WebDriver plugin that lets a test drive the window. A
# release must never ship it, since it would open a control channel on the window of every user. The plugin names
# itself in the strings of the executable (its crate path in a panic message, its registered name), so the scan looks
# for those. The release workflow runs it on each bundle before it is signed and published, see docs/releasing.md.

set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <Procly.app>" >&2
  exit 2
fi

app="$1"
executable="${app}/Contents/MacOS/procly-desktop"

if [ ! -f "${executable}" ]; then
  echo "no app executable at ${executable}" >&2
  exit 1
fi

# Markers of the plugin `tauri-plugin-wdio-webdriver`, in the spellings a crate name takes in a binary.
if grep -a -q -i -E 'tauri[-_]plugin[-_]wdio|wdio[-_]webdriver' "${executable}"; then
  echo "::error::${app} carries the end-to-end test driver, the e2e feature must not reach a release" >&2
  exit 1
fi

echo "${app} carries no end-to-end test driver"

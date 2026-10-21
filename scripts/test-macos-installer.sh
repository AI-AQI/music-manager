#!/bin/bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/music-manager-installer-tests.XXXXXX")"
trap 'rm -rf "$work"' EXIT
xcrun swiftc -swift-version 5 -module-cache-path "${TMPDIR:-/tmp}/music-manager-swift-cache" \
  "$root/scripts/macos-installer/InstallerCore.swift" "$root/scripts/macos-installer/tests/main.swift" -o "$work/tests"
"$work/tests"

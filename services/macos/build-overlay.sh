#!/bin/bash
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cat "$ROOT/services/macos/user-settings.swift" "$ROOT/services/macos/menu-panel.swift" "$ROOT/services/macos/sunoto-overlay.swift" > "$WORK/main.swift"
# swiftc ignores MACOSX_DEPLOYMENT_TARGET and targets the build machine's
# macOS, so a release build passes it on (cargo and cmake read it).
TARGET=()
if [ -n "${MACOSX_DEPLOYMENT_TARGET:-}" ]; then
    TARGET=(-target "$(uname -m)-apple-macos$MACOSX_DEPLOYMENT_TARGET")
fi
swiftc -O ${TARGET[@]+"${TARGET[@]}"} "$WORK/main.swift" -o "$1"

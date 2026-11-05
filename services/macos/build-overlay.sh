#!/bin/bash
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cat "$ROOT/services/macos/user-settings.swift" "$ROOT/services/macos/menu-panel.swift" "$ROOT/services/macos/sunoto-overlay.swift" > "$WORK/main.swift"
swiftc -O "$WORK/main.swift" -o "$1"

#!/bin/bash
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$1"
cat "$ROOT/services/macos/user-settings.swift" "$ROOT/services/macos/menu-panel.swift" "$ROOT/tools/ui/settings-evidence.swift" > "$WORK/main.swift"
swiftc -O "$WORK/main.swift" -o "$WORK/settings-evidence"
"$WORK/settings-evidence" "$@"

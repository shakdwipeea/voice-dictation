#!/usr/bin/env bash
# Render the macOS overlay (the floating pill and the notch island) to PNG
# frames, without a screen or any permission. The overlay's views are
# file-private, so this cuts services/macos/sunoto-overlay.swift at its
# "// MARK: App launch" line, appends tools/ui/overlay_render_main.swift, and
# compiles the two as one file.
#
#   tools/ui/render-overlay-frames.sh OUT_DIR [GROUP...]
#
# Writes OUT_DIR/<group>/<frame>.png and a contact sheet OUT_DIR/<group>.png
# per group. With no GROUP, renders every group. RENDER_MAIN overrides the
# appended main (used to render a baseline from an older checkout).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC="${OVERLAY_SRC:-$ROOT/services/macos/sunoto-overlay.swift}"
MAIN="${RENDER_MAIN:-$ROOT/tools/ui/overlay_render_main.swift}"

if [ $# -lt 1 ]; then
    echo "usage: $0 OUT_DIR [GROUP...]" >&2
    exit 2
fi
OUT="$1"
shift

line="$(grep -n '^// MARK: App launch' "$SRC" | head -n 1 | cut -d: -f1)"
if [ -z "$line" ]; then
    echo "no '// MARK: App launch' line in $SRC" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
head -n "$((line - 1))" "$SRC" > "$work/render.swift"
cat "$MAIN" >> "$work/render.swift"
swiftc -O -o "$work/render" "$work/render.swift"
mkdir -p "$OUT"
"$work/render" "$OUT" "$@"

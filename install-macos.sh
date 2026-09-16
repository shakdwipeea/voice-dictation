#!/usr/bin/env bash
# Build Sunoto and install it as a macOS Login Item.
#
# The heavy lifting lives in `sunoto-daemon setup`: it assembles Sunoto.app
# with the daemon as the bundle's executable, registers it at login, starts
# it, and then watches the app's own health report until the hotkey is
# verified, the microphone is capturing, and the speech model is loaded.
# Idempotent: an existing config is preserved and the app is replaced.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON_BIN="$PROJECT_DIR/target/release/sunoto-daemon"
OVERLAY_BIN="$PROJECT_DIR/target/release/sunoto-overlay"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
fail() { printf '\033[31m  ✗\033[0m %s\n' "$*"; exit 1; }

bold "== preflight =="
command -v cargo >/dev/null || fail "cargo not found — install Rust from https://rustup.rs"
command -v python3 >/dev/null || fail "python3 not found"
command -v codesign >/dev/null || fail "codesign not found"
command -v osascript >/dev/null || fail "osascript not found"
ok "toolchain: cargo + python3 + codesign"

bold "== build =="
if [ "${SUNOTO_SKIP_BUILD:-0}" = "1" ]; then
    warn "SUNOTO_SKIP_BUILD=1: using existing daemon and overlay binaries"
else
    (cd "$PROJECT_DIR" && cargo build --release -p sunoto-daemon)
    command -v swiftc >/dev/null || fail "swiftc not found — install Xcode command line tools"
    swiftc -O "$PROJECT_DIR/services/macos/sunoto-overlay.swift" -o "$OVERLAY_BIN"
fi
[ -x "$DAEMON_BIN" ] || fail "missing $DAEMON_BIN"
[ -x "$OVERLAY_BIN" ] || fail "missing $OVERLAY_BIN"
ok "built daemon and overlay"

exec "$DAEMON_BIN" setup "$@"

#!/usr/bin/env bash
# Install Sunoto on macOS.
#
# Two ways to run it:
#
#   curl -fsSL https://raw.githubusercontent.com/shakdwipeea/voice-dictation/master/install-macos.sh | bash
#       Downloads the latest prebuilt Sunoto.app (daemon, overlay, and the
#       Python runtime inside), installs it in ~/Applications, and runs the
#       guided setup. Needs nothing but macOS on Apple silicon.
#
#   bash install-macos.sh          (inside a repository checkout)
#       Builds from source and installs the checkout-backed bundle. Needs
#       Rust, the Xcode command line tools, and the Python runtimes.
#
# Both paths end in `sunoto-daemon setup`, which registers the Login Item,
# starts the app, offers the polish model download, and watches the app's
# own health until the hotkey is verified. Nothing here needs sudo.
#
# Options are passed through to setup: --with-llm, --without-llm,
# --no-login-item, --timeout-secs N, --dry-run.

set -euo pipefail

REPO="shakdwipeea/voice-dictation"
ASSET="Sunoto-macos-arm64.zip"
RELEASE_URL="https://github.com/$REPO/releases/latest/download/$ASSET"
APP_NAME="Sunoto.app"
INSTALL_DIR="$HOME/Applications"

bold() { printf '\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
warn() { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
fail() { printf '\033[31m  ✗\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || fail "this installer is for macOS; on Linux run install.sh"
[ "$(uname -m)" = "arm64" ] || fail "the prebuilt app is Apple silicon only"

# --- from a checkout: build from source -------------------------------------
SCRIPT_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ] && [ "${SUNOTO_INSTALL_PREBUILT:-0}" != "1" ]; then
    DAEMON_BIN="$SCRIPT_DIR/target/release/sunoto-daemon"
    OVERLAY_BIN="$SCRIPT_DIR/target/release/sunoto-overlay"
    bold "build from checkout"
    command -v cargo >/dev/null || fail "cargo not found — install Rust from https://rustup.rs (or run with SUNOTO_INSTALL_PREBUILT=1)"
    command -v swiftc >/dev/null || fail "swiftc not found — xcode-select --install"
    if [ "${SUNOTO_SKIP_BUILD:-0}" = "1" ]; then
        warn "SUNOTO_SKIP_BUILD=1: using existing binaries"
    else
        (cd "$SCRIPT_DIR" && cargo build --release -p sunoto-daemon)
        swiftc -O "$SCRIPT_DIR/services/macos/sunoto-overlay.swift" -o "$OVERLAY_BIN"
    fi
    [ -x "$DAEMON_BIN" ] || fail "missing $DAEMON_BIN"
    ok "built daemon and overlay"
    exec "$DAEMON_BIN" setup "$@"
fi

# --- prebuilt: download the latest release ----------------------------------
bold "download"
TMP="$(mktemp -d /tmp/sunoto-install.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT
curl -fL --progress-bar -o "$TMP/$ASSET" "$RELEASE_URL" \
    || fail "download failed: $RELEASE_URL (is there a published release?)"
curl -fsSL -o "$TMP/$ASSET.sha256" "$RELEASE_URL.sha256" || fail "checksum download failed"
expected="$(cut -d' ' -f1 "$TMP/$ASSET.sha256")"
actual="$(shasum -a 256 "$TMP/$ASSET" | cut -d' ' -f1)"
[ "$expected" = "$actual" ] || fail "checksum mismatch; the download was discarded"
ok "verified $ASSET"

bold "install"
mkdir -p "$INSTALL_DIR"
rm -rf "$TMP/unpacked" && mkdir -p "$TMP/unpacked"
ditto -x -k "$TMP/$ASSET" "$TMP/unpacked"
[ -d "$TMP/unpacked/$APP_NAME" ] || fail "archive did not contain $APP_NAME"
# Stop a running copy before replacing the bundle it runs from.
pkill -f "$APP_NAME/Contents/MacOS/sunoto-daemon" 2>/dev/null || true
sleep 1
rm -rf "$INSTALL_DIR/$APP_NAME"
ditto "$TMP/unpacked/$APP_NAME" "$INSTALL_DIR/$APP_NAME"
# The bundle is ad-hoc signed, so macOS would refuse to open a quarantined
# copy. You chose to install it; lift the flag.
xattr -dr com.apple.quarantine "$INSTALL_DIR/$APP_NAME" 2>/dev/null || true
ok "installed $INSTALL_DIR/$APP_NAME"

exec "$INSTALL_DIR/$APP_NAME/Contents/MacOS/sunoto-daemon" setup "$@"

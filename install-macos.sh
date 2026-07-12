#!/usr/bin/env bash
# Build Sunoto and install its GUI-context macOS Login Item.
# Idempotent: existing config is preserved and the login item is replaced.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON_BIN="$PROJECT_DIR/target/release/sunoto-daemon"
OVERLAY_BIN="$PROJECT_DIR/target/release/sunoto-overlay"
ASR_PYTHON="$PROJECT_DIR/.venv-nemotron-mac/bin/python"
BUILD_APP="$PROJECT_DIR/target/release/Sunoto.app"
APP_BIN="$BUILD_APP/Contents/MacOS/sunoto-login"
INSTALLED_APP="$HOME/Applications/Sunoto Login.app"
CONFIG_DIR="$HOME/Library/Application Support/sunoto"
CONFIG_PATH="${SUNOTO_CONFIG:-$CONFIG_DIR/config.json}"
LOG_DIR="$HOME/Library/Logs/sunoto"
LOG_PATH="$LOG_DIR/daemon.log"
LEGACY_PLIST="$HOME/Library/LaunchAgents/com.earendil-works.sunoto.plist"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
note() { printf '  %s\n' "$*"; }
warn() { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
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

rm -rf "$BUILD_APP"
mkdir -p "$BUILD_APP/Contents/MacOS" "$BUILD_APP/Contents/Resources"
cp "$PROJECT_DIR/services/macos/sunoto-login" "$APP_BIN"
chmod +x "$APP_BIN"
python3 - "$BUILD_APP/Contents/Resources/sunoto-login.env" \
    "$DAEMON_BIN" "$PROJECT_DIR" "$LOG_PATH" "$ASR_PYTHON" <<'PY'
import shlex, sys
path, daemon, root, log, python = sys.argv[1:]
values = {
    "SUNOTO_DAEMON": daemon,
    "SUNOTO_ROOT": root,
    "SUNOTO_LOG": log,
    "SUNOTO_ASR_PYTHON": python,
}
with open(path, "w") as stream:
    for key, value in values.items():
        stream.write(f"{key}={shlex.quote(value)}\n")
PY
cat > "$BUILD_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>sunoto-login</string>
    <key>CFBundleIdentifier</key><string>com.earendil-works.sunoto-login</string>
    <key>CFBundleName</key><string>Sunoto Login</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSUIElement</key><true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sunoto records microphone audio while you hold the push-to-talk shortcut.</string>
    <key>NSInputMonitoringUsageDescription</key>
    <string>Sunoto listens for the global push-to-talk shortcut while you use other applications.</string>
</dict>
</plist>
PLIST
plutil -lint "$BUILD_APP/Contents/Info.plist" >/dev/null
# Cargo and swiftc outputs are already ad-hoc signed on macOS. Do not re-sign
# them here: changing their cdhash needlessly churns existing TCC grants.
codesign --force --sign - --identifier com.earendil-works.sunoto-login "$BUILD_APP" >/dev/null
ok "built and signed daemon, overlay, and GUI login launcher"

bold "== config and ASR runtime =="
mkdir -p "$CONFIG_DIR"
if [ -f "$CONFIG_PATH" ]; then
    ok "config exists: $CONFIG_PATH (left untouched)"
else
    "$DAEMON_BIN" config init >/dev/null
    ok "created $CONFIG_PATH"
fi
BACKEND="$(python3 - "$CONFIG_PATH" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("backend", "mock"))
PY
)"
case "$BACKEND" in
    parakeet_mlx_offline|parakeet_mlx_streaming)
        [ -x "$ASR_PYTHON" ] || fail "ASR Python missing: $ASR_PYTHON — run services/asr/setup_macos_runtime.sh"
        "$ASR_PYTHON" -c 'import mlx, parakeet_mlx' \
            || fail "MLX/Parakeet imports failed — repair .venv-nemotron-mac"
        ok "ASR runtime imports: mlx + parakeet_mlx"
        ;;
    *)
        warn "backend=$BACKEND; Parakeet runtime preflight skipped"
        ;;
esac

bold "== GUI Login Item =="
mkdir -p "$HOME/Applications" "$LOG_DIR"

# Remove the obsolete LaunchAgent. It lacks a responsible GUI process, so
# macOS repeatedly disables its CGEventTap.
launchctl bootout "gui/$(id -u)/com.earendil-works.sunoto" 2>/dev/null || true
rm -f "$LEGACY_PLIST"

rm -rf "$INSTALLED_APP"
cp -R "$BUILD_APP" "$INSTALLED_APP"
codesign --verify --deep --strict "$INSTALLED_APP"
ok "installed $INSTALLED_APP"

osascript - "$INSTALLED_APP" <<'APPLESCRIPT'
on run argv
    set loginPath to item 1 of argv
    tell application "System Events"
        try
            delete every login item whose name is "Sunoto Login"
        end try
        make login item at end with properties {name:"Sunoto Login", path:loginPath, hidden:true}
    end tell
end run
APPLESCRIPT
ok "registered Sunoto Login in macOS Login Items"

# Replace any manually started copy with the installed GUI-context launcher.
pkill -f 'target/release/sunoto-daemon run' 2>/dev/null || true
sleep 1
: > "$LOG_PATH"
open "$INSTALLED_APP"
ok "opened GUI login launcher"

bold "== permissions =="
note "Grant Accessibility and Input Monitoring to BOTH of these entries:"
note "  $INSTALLED_APP"
note "  $DAEMON_BIN"
note "Microphone permission is requested on first capture."
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" >/dev/null 2>&1 || true
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent" >/dev/null 2>&1 || true

bold "== done =="
note "Login item: System Settings -> General -> Login Items -> Sunoto Login"
note "Launch:     open \"$INSTALLED_APP\""
note "Logs:       tail -f \"$LOG_PATH\""
note "Verify:     bash \"$PROJECT_DIR/scripts/macos-port/verify-phase.sh\" 7"
note "Hotkey:     hold Ctrl+F1, speak, release"

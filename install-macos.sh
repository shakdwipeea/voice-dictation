#!/usr/bin/env bash
# Install the sunoto voice-dictation daemon as a macOS launchd LaunchAgent.
#
# This is the macOS counterpart to install.sh (systemd). It:
#   1. Verifies prerequisites (Rust, Python 3, the macOS NeMo venv optionally).
#   2. Builds the release daemon binary.
#   3. Writes a default config if none exists (~/Library/Application Support/sunoto).
#   4. Renders the launchd plist to ~/Library/LaunchAgents/.
#   5. Loads (or reloads) the LaunchAgent.
#   6. Prints the TCC permissions the user must grant once.
#
# Idempotent — safe to re-run.
#
# The daemon on macOS uses CGEventTap hotkeys, CoreAudio capture, and CGEvent
# text insertion; the preferred ASR backend is whole-utterance Nemotron on CPU
# (`backend=nemotron_offline`, `asr_device=cpu`). The cache-aware streaming
# sidecar remains available for experiments, but CPU streaming is too slow on
# this Mac.
# See docs/macos-port-plan.md.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON_BIN="$PROJECT_DIR/target/release/sunoto-daemon"
OVERLAY_BIN="$PROJECT_DIR/target/release/sunoto-overlay"
APP_BUNDLE="$PROJECT_DIR/target/release/Sunoto.app"
APP_BIN="$APP_BUNDLE/Contents/MacOS/sunoto-daemon"
APP_SIGN_ID="com.earendil-works.sunoto"
DAEMON_SIGN_ID="com.earendil-works.sunoto-daemon"
OVERLAY_SIGN_ID="com.earendil-works.sunoto-overlay"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
note()  { printf '  %s\n' "$*"; }
warn()  { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
ok()    { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
fail()  { printf '\033[31m  ✗\033[0m %s\n' "$*"; exit 1; }

# ---- preflight ----
bold "== preflight =="
command -v cargo >/dev/null || fail "cargo not found — install Rust (https://rustup.rs)"
command -v python3 >/dev/null || fail "python3 not found"
command -v swiftc >/dev/null || fail "swiftc not found — install Xcode command line tools"
command -v codesign >/dev/null || fail "codesign not found"
ok "toolchain: cargo + python3 + swiftc"

if [ -x "$PROJECT_DIR/.venv-nemotron-mac/bin/python" ]; then
    ok "Nemotron venv: $PROJECT_DIR/.venv-nemotron-mac"
else
    warn "no .venv-nemotron-mac — run services/asr/setup_macos_runtime.sh for real ASR"
    warn "the 'mock' backend works without it"
fi

# ---- build ----
bold "== build =="
( cd "$PROJECT_DIR" && cargo build --release -p sunoto-daemon )
[ -x "$DAEMON_BIN" ] || fail "build did not produce $DAEMON_BIN"
swiftc -O "$PROJECT_DIR/services/macos/sunoto-overlay.swift" -o "$OVERLAY_BIN"
[ -x "$OVERLAY_BIN" ] || fail "build did not produce $OVERLAY_BIN"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
cp "$DAEMON_BIN" "$APP_BIN"
cat > "$APP_BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>sunoto-daemon</string>
    <key>CFBundleIdentifier</key>
    <string>com.earendil-works.sunoto</string>
    <key>CFBundleName</key>
    <string>Sunoto</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sunoto records microphone audio while you hold the push-to-talk shortcut.</string>
    <key>NSInputMonitoringUsageDescription</key>
    <string>Sunoto listens for the global push-to-talk shortcut while you use other applications.</string>
</dict>
</plist>
PLIST
codesign --force --sign - --identifier "$DAEMON_SIGN_ID" "$DAEMON_BIN" >/dev/null
codesign --force --sign - --identifier "$DAEMON_SIGN_ID" "$APP_BIN" >/dev/null
codesign --force --sign - --identifier "$APP_SIGN_ID" "$APP_BUNDLE" >/dev/null
codesign --force --sign - --identifier "$OVERLAY_SIGN_ID" "$OVERLAY_BIN" >/dev/null
ok "built and signed $DAEMON_BIN ($DAEMON_SIGN_ID)"
ok "built and signed $APP_BUNDLE ($APP_SIGN_ID)"
ok "built and signed $OVERLAY_BIN ($OVERLAY_SIGN_ID)"

# ---- config ----
bold "== config =="
CONFIG_DIR="${HOME}/Library/Application Support/sunoto"
CONFIG_PATH="${SUNOTO_CONFIG:-$CONFIG_DIR/config.json}"
mkdir -p "$CONFIG_DIR"
if [ -f "$CONFIG_PATH" ]; then
    ok "config exists: $CONFIG_PATH (left untouched)"
else
    "$DAEMON_BIN" config init >/dev/null
    ok "config created: $CONFIG_PATH"
    # Default the macOS install to offline RNNT on CPU when the venv is present;
    # otherwise leave the safe default (mock).
    if [ -x "$PROJECT_DIR/.venv-nemotron-mac/bin/python" ]; then
        python3 - "$CONFIG_PATH" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["backend"] = "nemotron_offline"
d["overlay_backend"] = "macos"
d["asr_device"] = "cpu"
json.dump(d, open(p, "w"), indent=2)
PY
        ok "configured backend=nemotron_offline, asr_device=cpu, overlay_backend=macos"
    fi
fi
note 'key fields: backend (mock|nemotron|nemotron_offline), asr_device (cpu|mps|cuda), shortcut (default Ctrl+F1), overlay_enabled'

# ---- launchd LaunchAgent ----
bold "== launchd LaunchAgent =="
LAUNCH_DIR="${HOME}/Library/LaunchAgents"
LAUNCH_PLIST_SRC="$PROJECT_DIR/services/macos/com.earendil-works.sunoto.plist"
LAUNCH_PLIST_DST="$LAUNCH_DIR/com.earendil-works.sunoto.plist"
LOG_DIR="${HOME}/Library/Logs/sunoto"
mkdir -p "$LAUNCH_DIR" "$LOG_DIR"

# Render the plist template: daemon binary, repo root, home for log paths.
# Uses the BARE daemon binary (DAEMON_BIN), not the app bundle — the bare
# binary's path-based TCC grant survives rebuilds; the bundle-id grant does
# not. See docs/macos-recurring-issues.md § inert-tap.
sed \
    -e "s|__SUNOTO_DAEMON__|$DAEMON_BIN|g" \
    -e "s|__SUNOTO_ROOT__|$PROJECT_DIR|g" \
    -e "s|__HOME__|$HOME|g" \
    "$LAUNCH_PLIST_SRC" > "$LAUNCH_PLIST_DST"
ok "installed $LAUNCH_PLIST_DST"

# Reload: unload if present, then load.
if launchctl list com.earendil-works.sunoto >/dev/null 2>&1; then
    launchctl unload "$LAUNCH_PLIST_DST" 2>/dev/null || true
fi
launchctl load "$LAUNCH_PLIST_DST" && ok "launchd loaded"

sleep 2
if launchctl list com.earendil-works.sunoto >/dev/null 2>&1; then
    ok "LaunchAgent registered (com.earendil-works.sunoto)"
else
    warn "LaunchAgent not listed. Check: launchctl list | grep sunoto, and $LOG_DIR/daemon.log"
fi

# ---- permissions ----
bold "== macOS permissions (grant once) =="
note "The daemon needs three TCC permissions. Open System Settings -> Privacy & Security:"
note "  1. Accessibility        — CGEventTap hotkey + CGEvent text insertion"
note "  2. Input Monitoring      — event-tap key monitoring"
note "  3. Microphone            — CoreAudio capture (prompted on first capture)"
note "Grant Accessibility and Input Monitoring to the BARE daemon binary (not the app bundle):"
note "  $DAEMON_BIN"
note "The bare binary's path-based grant survives `cargo build` rebuilds; the"
note "app-bundle grant is cdhash-bound and breaks every rebuild. See"
note "docs/macos-recurring-issues.md § inert-tap."
note "Until these are granted, hotkey/capture/insertion will fail with permission errors."
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" >/dev/null 2>&1 || true

# ---- summary ----
bold "== done =="
note "Manage:   launchctl unload/load \"$LAUNCH_PLIST_DST\""
note "Logs:     tail -f \"$LOG_DIR/daemon.log\""
note "Verify:   bash \"$PROJECT_DIR/scripts/macos-port/verify-all.sh\""
note "Check:    \"$DAEMON_BIN\" check    # event tap + CoreAudio + sidecar protocol"
note "Hotkey:   hold Ctrl+F1, speak, release — text lands at the cursor (after granting permissions)"

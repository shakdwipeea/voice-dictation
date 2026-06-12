#!/usr/bin/env bash
# Install the sunoto voice-dictation daemon as a systemd --user service.
#
# This script:
#   1. Verifies build/runtime prerequisites (Rust, Python, GTK4, audio).
#   2. Builds the release daemon binary.
#   3. Writes a default config if none exists.
#   4. Renders the systemd template to ~/.config/systemd/user/.
#   5. Enables + starts the systemd --user service.
#
# Idempotent — safe to re-run.
#
# The daemon supports X11 directly. On Hyprland/Wayland, source the rendered
# hypr/voice-dictation.conf so the compositor forwards push-to-talk edges to
# the daemon control socket; insertion uses wtype/wl-copy.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON_BIN="$PROJECT_DIR/target/release/sunoto-daemon"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
note()  { printf '  %s\n' "$*"; }
warn()  { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
ok()    { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
fail()  { printf '\033[31m  ✗\033[0m %s\n' "$*"; exit 1; }

# ---- preflight ----
bold "== preflight =="
command -v cargo >/dev/null || fail "cargo not found — install Rust (https://rustup.rs)"
command -v python3 >/dev/null || fail "python3 not found"
ok "toolchain: cargo + python3"

if [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
    command -v hyprctl >/dev/null \
        && ok "Hyprland control available" \
        || warn "hyprctl missing — Wayland hotkey binding install will not be useful"
    command -v wtype >/dev/null \
        && ok "Wayland text insertion available (wtype)" \
        || warn "wtype missing — Wayland insertion will fall back to clipboard only"
    command -v wl-copy >/dev/null \
        && ok "Wayland clipboard available (wl-copy)" \
        || warn "wl-copy missing — Wayland clipboard fallback unavailable"
elif [ "${XDG_SESSION_TYPE:-}" != "x11" ]; then
    warn "session is '${XDG_SESSION_TYPE:-unknown}', not x11/wayland — hotkey support may need a compositor binding"
fi
python3 -c 'import gi; gi.require_version("Gtk", "4.0")' 2>/dev/null \
    && ok "GTK4 overlay available" \
    || warn "GTK4 typelib missing — overlay falls back to the native X11 bubble (apt install gir1.2-gtk-4.0)"
python3 -c 'import Xlib' 2>/dev/null \
    || warn "python-xlib missing — overlay X11 anchoring unavailable (apt install python3-xlib)"
command -v parec >/dev/null \
    && ok "PulseAudio capture (parec)" \
    || warn "parec missing — microphone capture won't work (apt install pulseaudio-utils)"
if [ -x "$PROJECT_DIR/.venv-nemotron/bin/python" ]; then
    ok "Nemotron venv: $PROJECT_DIR/.venv-nemotron"
else
    warn "no .venv-nemotron — the 'nemotron' backend needs it (see services/asr/); 'mock' works without"
fi

# ---- build ----
bold "== build =="
(cd "$PROJECT_DIR" && cargo build --release -p sunoto-daemon)
[ -x "$DAEMON_BIN" ] || fail "build did not produce $DAEMON_BIN"
ok "built $DAEMON_BIN"

# ---- config ----
bold "== config =="
CONFIG_PATH="${SUNOTO_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/sunoto/config.json}"
if [ -f "$CONFIG_PATH" ]; then
    ok "config exists: $CONFIG_PATH (left untouched)"
else
    "$DAEMON_BIN" config init >/dev/null
    ok "config created: $CONFIG_PATH"
fi
note "key fields: backend (mock|nemotron), shortcut (default Ctrl+F1), overlay_enabled"

# ---- systemd --user unit ----
bold "== systemd --user unit =="
SYSTEMD_DIR="$HOME/.config/systemd/user"
mkdir -p "$SYSTEMD_DIR"
UNIT_SRC="$PROJECT_DIR/systemd/voice-dictation.service"
UNIT_DST="$SYSTEMD_DIR/voice-dictation.service"
sed "s|__SUNOTO_ROOT__|$PROJECT_DIR|g" "$UNIT_SRC" > "$UNIT_DST"
ok "installed $UNIT_DST"

systemctl --user daemon-reload
ok "daemon-reload done"

# ---- Hyprland/Wayland binding ----
if command -v hyprctl >/dev/null; then
    bold "== Hyprland binding =="
    HYPR_DIR="$HOME/.config/hypr"
    HYPR_UNIT_SRC="$PROJECT_DIR/hypr/voice-dictation.conf"
    HYPR_UNIT_DST="$HYPR_DIR/voice-dictation.conf"
    mkdir -p "$HYPR_DIR"
    sed "s|__SUNOTO_ROOT__|$PROJECT_DIR|g" "$HYPR_UNIT_SRC" > "$HYPR_UNIT_DST"
    ok "installed $HYPR_UNIT_DST"
    if grep -q "source = ~/.config/hypr/voice-dictation.conf" "$HYPR_DIR/bindings.conf" 2>/dev/null; then
        ok "bindings.conf already sources voice-dictation.conf"
    else
        cat >> "$HYPR_DIR/bindings.conf" <<'EOF'

# voice-dictation
source = ~/.config/hypr/voice-dictation.conf
EOF
        ok "added source line to $HYPR_DIR/bindings.conf"
    fi
    hyprctl reload >/dev/null && ok "hyprctl reload done"
    if [ -n "$(hyprctl configerrors)" ]; then
        warn "Hyprland reported config errors. Check: hyprctl configerrors"
    fi
fi

# ---- enable + start ----
bold "== enable + start =="
systemctl --user enable voice-dictation.service >/dev/null && ok "enabled (will start at login)"
systemctl --user restart voice-dictation.service && ok "started"
sleep 2
if systemctl --user is-active --quiet voice-dictation.service; then
    ok "service is active"
else
    warn "service is NOT active. Check: journalctl --user -u voice-dictation.service -n 50"
fi

# ---- summary ----
bold "== done =="
note "Service:  systemctl --user [start|stop|restart|status] voice-dictation"
note "Logs:     journalctl --user -u voice-dictation.service -f"
note "Hotkey:   hold Ctrl+F1, speak, release — text lands at the cursor"
note "Backend:  edit $CONFIG_PATH (backend: \"nemotron\" for real ASR), then restart"
note "Check:    $DAEMON_BIN check     # X11/XTEST/shortcut/sidecar verification"
note "GPU:      bash $PROJECT_DIR/bin/gpu-status.sh   # health check"

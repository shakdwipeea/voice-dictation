#!/usr/bin/env bash
# Install voice-dictation as a user service + add Hyprland keybinding.
#
# This script:
#   1. Verifies the project's uv-managed venv exists and works.
#   2. Renders the systemd template to ~/.config/systemd/user/.
#   3. Renders the hypr binding snippet to ~/.config/hypr/.
#   4. Adds `source = ~/.config/hypr/voice-dictation.conf` to bindings.conf
#      if not already present.
#   5. Enables + starts the systemd --user service.
#   6. Reloads Hyprland to pick up the new bind.
#
# Idempotent — safe to re-run.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV_BIN="$PROJECT_DIR/.venv/bin"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
note()  { printf '  %s\n' "$*"; }
warn()  { printf '\033[33m  warn:\033[0m %s\n' "$*"; }
ok()    { printf '\033[32m  ✓\033[0m %s\n' "$*"; }
fail()  { printf '\033[31m  ✗\033[0m %s\n' "$*"; exit 1; }

# ---- preflight ----
bold "== preflight =="
[ -x "$VENV_BIN/vd-daemon" ] || fail "venv not built. Run: cd '$PROJECT_DIR' && uv sync"
ok "venv: $VENV_BIN"
command -v hyprctl >/dev/null || warn "hyprctl not found — Hyprland binding step will be skipped"
command -v wl-copy >/dev/null || warn "wl-copy missing — paste injection won't work (pacman -S wl-clipboard)"
command -v wtype >/dev/null || warn "wtype missing — direct Wayland typing fallback won't work (pacman -S wtype)"
command -v grim >/dev/null || warn "grim missing — verification screenshots won't work (pacman -S grim)"
[ -f /usr/lib/libgtk4-layer-shell.so ] || warn "gtk4-layer-shell missing — overlay won't render (pacman -S gtk4-layer-shell)"

# ---- systemd --user unit ----
bold "== systemd --user unit =="
SYSTEMD_DIR="$HOME/.config/systemd/user"
mkdir -p "$SYSTEMD_DIR"
UNIT_SRC="$PROJECT_DIR/systemd/voice-dictation.service"
UNIT_DST="$SYSTEMD_DIR/voice-dictation.service"
sed "s|__VENV_BIN__|$VENV_BIN|g" "$UNIT_SRC" > "$UNIT_DST"
ok "installed $UNIT_DST"

systemctl --user daemon-reload
ok "daemon-reload done"

# ---- hyprland binding ----
bold "== hyprland binding =="
HYPR_DIR="$HOME/.config/hypr"
if [ -d "$HYPR_DIR" ]; then
    BIND_SRC="$PROJECT_DIR/hypr/voice-dictation.conf"
    BIND_DST="$HYPR_DIR/voice-dictation.conf"
    sed "s|__VENV_BIN__|$VENV_BIN|g" "$BIND_SRC" > "$BIND_DST"
    ok "installed $BIND_DST"

    BINDINGS_FILE="$HYPR_DIR/bindings.conf"
    if [ -f "$BINDINGS_FILE" ]; then
        SOURCE_LINE='source = ~/.config/hypr/voice-dictation.conf'
        if grep -qF "$SOURCE_LINE" "$BINDINGS_FILE"; then
            ok "bindings.conf already sources voice-dictation.conf"
        else
            {
                echo
                echo "# voice-dictation"
                echo "$SOURCE_LINE"
            } >> "$BINDINGS_FILE"
            ok "appended source line to $BINDINGS_FILE"
        fi
    else
        warn "$BINDINGS_FILE not found — add this line manually somewhere your hypr config loads:"
        note "  source = ~/.config/hypr/voice-dictation.conf"
    fi

    if command -v hyprctl >/dev/null 2>&1 && [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]; then
        hyprctl reload >/dev/null && ok "reloaded hyprland"
    else
        warn "Not inside a live Hyprland session; run 'hyprctl reload' yourself to activate the bind."
    fi
else
    warn "$HYPR_DIR not found — skipping Hyprland binding"
fi

# ---- enable + start ----
bold "== enable + start =="
systemctl --user enable voice-dictation.service >/dev/null && ok "enabled (will start at login)"
systemctl --user restart voice-dictation.service && ok "started"
sleep 1
if systemctl --user is-active --quiet voice-dictation.service; then
    ok "service is active"
else
    warn "service is NOT active. Check: systemctl --user status voice-dictation.service"
fi

# ---- summary ----
bold "== done =="
note "Service:  systemctl --user [start|stop|restart|status|logs] voice-dictation"
note "Logs:     journalctl --user -u voice-dictation.service -f"
note "Hotkey:   SUPER + I  (press to start, press again to stop & paste)"
note "Test:     $VENV_BIN/vd status    # query the daemon"
note "Samples:  $VENV_BIN/vd-samples   # record + transcribe regression set"
note "GPU:      bash $PROJECT_DIR/bin/gpu-status.sh   # health check"

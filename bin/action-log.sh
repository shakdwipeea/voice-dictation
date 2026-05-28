#!/usr/bin/env bash
# Record what we're about to do, BEFORE doing it.
# If the system crashes mid-action, on reboot we can read last-action.txt
# to know exactly what triggered it.
set -euo pipefail

LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/voice-dictation"
mkdir -p "$LOG_DIR"

MSG="${*:-(no message)}"
TS="$(date '+%Y-%m-%d %H:%M:%S')"

# Overwrite last-action: only ever shows the MOST RECENT thing attempted.
printf '[%s] %s\n' "$TS" "$MSG" > "$LOG_DIR/last-action.txt"

# Append to rolling history.
printf '[%s] %s\n' "$TS" "$MSG" >> "$LOG_DIR/actions.log"

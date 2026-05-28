#!/usr/bin/env bash
# Continuously tail kernel log for NVIDIA Xid events, append to persistent log.
# Designed to be run in the background during any GPU work.
# Survives reboot via journald's persistent storage; restart this script after reboot.
set -euo pipefail

LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/voice-dictation"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/xid-events.log"

ts() { date '+%Y-%m-%d %H:%M:%S'; }

{
  echo "[$(ts)] === gpu-watch started (pid $$) ==="
  echo "[$(ts)] watching journalctl -k -f for Xid / 'fallen off the bus' / NVRM errors"
} >> "$LOG_FILE"

# -k = kernel only, -f = follow, --since=now to skip backfill
journalctl -k -f --since=now --no-pager 2>/dev/null \
  | grep --line-buffered -iE 'xid|fallen off the bus|nvrm:.*error|gpu reset' \
  | while IFS= read -r line; do
      printf '[%s] %s\n' "$(ts)" "$line" >> "$LOG_FILE"
    done

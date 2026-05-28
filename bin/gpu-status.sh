#!/usr/bin/env bash
# One-shot health summary. Read-only. Safe to run anytime, including after a crash/reboot.
# Used by the gpu-status skill.
set -euo pipefail

LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/voice-dictation"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
faint() { printf '\033[2m%s\033[0m\n' "$*"; }

bold "== Uptime & boot history (last 5 boots) =="
uptime
journalctl --list-boots --no-pager 2>/dev/null | tail -5
echo

bold "== Current GPU state (nvidia-smi, read-only) =="
if command -v nvidia-smi >/dev/null 2>&1; then
  nvidia-smi --query-gpu=name,driver_version,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw,pstate,pcie.link.gen.current,pcie.link.width.current --format=csv
else
  echo "(nvidia-smi not found)"
fi
echo

bold "== Xid events in CURRENT boot =="
journalctl -k -b 0 --no-pager 2>/dev/null \
  | grep -iE 'xid|fallen off the bus' \
  | tail -20 || echo "(none)"
echo

bold "== Xid events in PREVIOUS boot (boot -1) =="
journalctl -k -b -1 --no-pager 2>/dev/null \
  | grep -iE 'xid|fallen off the bus' \
  | tail -20 || echo "(none)"
echo

bold "== Last 5 entries from our Xid watcher log =="
if [[ -f "$LOG_DIR/xid-events.log" ]]; then
  tail -5 "$LOG_DIR/xid-events.log"
else
  faint "(no $LOG_DIR/xid-events.log yet — watcher not started)"
fi
echo

bold "== Last action attempted (might be the crash trigger if rebooted) =="
if [[ -f "$LOG_DIR/last-action.txt" ]]; then
  cat "$LOG_DIR/last-action.txt"
else
  faint "(no last-action.txt yet)"
fi
echo

bold "== Last 5 lines of action history =="
if [[ -f "$LOG_DIR/actions.log" ]]; then
  tail -5 "$LOG_DIR/actions.log"
else
  faint "(no actions.log yet)"
fi
echo

bold "== gpu-watch process status =="
if pgrep -af 'bin/gpu-watch.sh' >/dev/null; then
  pgrep -af 'bin/gpu-watch.sh'
else
  faint "(gpu-watch.sh NOT running — start with: bash bin/gpu-watch.sh &)"
fi

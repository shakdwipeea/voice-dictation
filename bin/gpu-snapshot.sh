#!/usr/bin/env bash
# Take a single nvidia-smi snapshot, append to rolling log.
# Read-only — does not initialize CUDA, does not load anything on GPU.
set -euo pipefail

LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/voice-dictation"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/gpu-snapshots.log"
LABEL="${1:-snapshot}"

{
  echo "=========================================="
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] LABEL=$LABEL"
  echo "=========================================="
  nvidia-smi --query-gpu=timestamp,name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw,pstate,pcie.link.gen.current,pcie.link.width.current --format=csv 2>&1
  echo
} >> "$LOG_FILE"

# Echo to stdout too for live feedback
tail -n 4 "$LOG_FILE"

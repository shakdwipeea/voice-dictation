#!/usr/bin/env bash
# Create/attach the project tmux layout for the parakeet-mlx macOS benchmark/migration.
# Safe by default: it does NOT start the daemon or load any ASR/MLX model.
set -euo pipefail

SESSION="${SESSION:-sunoto-parakeet}"
ATTACH=1
if [[ "${1:-}" == "--no-attach" ]]; then
  ATTACH=0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$ROOT/.venv-nemotron-mac/bin/activate"

if tmux has-session -t "$SESSION" 2>/dev/null; then
  echo "tmux session '$SESSION' already exists."
  if [[ "$ATTACH" == "1" ]]; then
    exec tmux attach -t "$SESSION"
  fi
  exit 0
fi

run_shell='exec "${SHELL:-/bin/zsh}"'

# Window 0: benchmark/control lane. This is the only window that should load ASR/MLX models.
tmux new-session -d -s "$SESSION" -c "$ROOT" -n bench

tmux set-option -t "$SESSION" mouse on >/dev/null
tmux set-option -t "$SESSION" history-limit 50000 >/dev/null
tmux set-option -t "$SESSION" status-left-length 40 >/dev/null
tmux set-option -t "$SESSION" status-left " #[bold]sunoto-parakeet #[default]" >/dev/null
tmux set-option -t "$SESSION" automatic-rename off >/dev/null

tmux send-keys -t "$SESSION:bench" "cd '$ROOT'" C-m
if [[ -f "$VENV" ]]; then
  tmux send-keys -t "$SESSION:bench" "source '$VENV'" C-m
fi
tmux send-keys -t "$SESSION:bench" "clear" C-m
tmux send-keys -t "$SESSION:bench" "cat <<'EOF'
BENCH / MODEL-LOAD LANE
=======================
Use this window for Phase 0 only:
  - install parakeet-mlx in the existing env
  - run tools/phase0/parakeet_mlx_measure.py
  - run exactly ONE real ASR/MLX model process at a time

Before benchmarking:
  pkill -f sunoto-daemon || true
  pkill -f nemotron || true
  pkill -f parakeet || true

Suggested next command:
  uv pip install parakeet-mlx

Do NOT run the daemon or a second sidecar while this benchmark is loading a model.
EOF" C-m

# Window 1: code/editing lane.
tmux new-window -t "$SESSION" -n code -c "$ROOT"
tmux send-keys -t "$SESSION:code" "cd '$ROOT'" C-m
tmux send-keys -t "$SESSION:code" "clear" C-m
tmux send-keys -t "$SESSION:code" "cat <<'EOF'
CODE LANE
=========
Safe parallel work here after/while benchmark downloads:
  - inspect sidecars/settings/tests
  - edit services/asr/parakeet_mlx_offline_sidecar.py
  - edit apps/daemon/src/settings.rs

No ASR model loads in this window.

Useful files:
  docs/parakeet-mlx-migration-plan.md
  services/asr/nemotron_offline_sidecar.py
  services/asr/nemotron_sidecar.py
  apps/daemon/src/settings.rs
EOF" C-m

# Window 2: tests lane. Unit tests only unless explicitly decided.
tmux new-window -t "$SESSION" -n tests -c "$ROOT"
tmux send-keys -t "$SESSION:tests" "cd '$ROOT'" C-m
tmux send-keys -t "$SESSION:tests" "clear" C-m
tmux send-keys -t "$SESSION:tests" "cat <<'EOF'
TEST LANE
=========
Safe here:
  cargo test --workspace --offline
  cargo clippy --workspace --offline --all-targets -- -D warnings
  python -m unittest tests.phase1.test_parakeet_mlx_offline_sidecar

Avoid tests that start real ASR models while the benchmark window is active.
EOF" C-m

# Window 3: logs/monitoring lane. Tailing a file is safe and does not start daemon.
tmux new-window -t "$SESSION" -n logs -c "$ROOT"
tmux send-keys -t "$SESSION:logs" "cd '$ROOT'" C-m
tmux send-keys -t "$SESSION:logs" "clear" C-m
tmux send-keys -t "$SESSION:logs" "touch /tmp/sunoto-bare.log /tmp/sunoto-parakeet-bench.log" C-m
tmux send-keys -t "$SESSION:logs" "echo 'LOG LANE: tailing /tmp/sunoto-bare.log and /tmp/sunoto-parakeet-bench.log (safe; no daemon started).'" C-m
tmux send-keys -t "$SESSION:logs" "tail -F /tmp/sunoto-bare.log /tmp/sunoto-parakeet-bench.log" C-m

# Window 4: plan/reference.
tmux new-window -t "$SESSION" -n plan -c "$ROOT"
tmux send-keys -t "$SESSION:plan" "cd '$ROOT'" C-m
tmux send-keys -t "$SESSION:plan" "clear" C-m
tmux send-keys -t "$SESSION:plan" "echo 'PLAN LANE: docs/parakeet-mlx-migration-plan.md'" C-m
tmux send-keys -t "$SESSION:plan" "echo 'Open with: less docs/parakeet-mlx-migration-plan.md'" C-m

# Focus benchmark lane.
tmux select-window -t "$SESSION:bench"

echo "Created tmux session '$SESSION' for $ROOT"
echo "Attach with: tmux attach -t $SESSION"
echo "Windows: bench, code, tests, logs, plan"

if [[ "$ATTACH" == "1" ]]; then
  exec tmux attach -t "$SESSION"
fi

#!/usr/bin/env bash
# Full integration gate for the macOS port. Runs every phase's objective
# checks plus the universal build/clippy/test gate, then reports a single
# pass/fail. This is the "does the whole port work" check the agent runs at
# the end. See docs/macos-port-plan.md.
#
# Usage: scripts/macos-port/verify-all.sh
set -uo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/verify-phase.sh" 0 1 2 3 4 5 6 7 8

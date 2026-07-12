#!/usr/bin/env bash
# Objective per-phase verifier for the macOS port (docs/macos-port-plan.md).
#
# Each phase has concrete, agent-independent acceptance checks. The script
# always runs the universal gate (build + clippy + tests on macOS, plus the
# Python suites), then per-phase objective checks, and reports which phases
# pass, fail, or need manual/interactive (TCC permission) verification.
#
# Usage:
#   scripts/macos-port/verify-phase.sh         # all phases
#   scripts/macos-port/verify-phase.sh 0       # one phase
#   scripts/macos-port/verify-phase.sh 1 2 5   # selected phases
#
# Exit code: 0 only if every requested phase passes its objective checks.
# A phase that requires a live TCC prompt is reported as MANUAL, not FAIL.
#
# This script is safe to run repeatedly and from a headless context. It does
# NOT trigger dictation, type into windows, or load the Nemotron model (Phase 0
# measurement is a separate, optional step).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# Make cargo available on macOS (rustup install location).
if [ -z "${CARGO_HOME:-}" ] && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

PYTHON="${PYTHON:-python3}"
PASS=0
FAIL=0
MANUAL=0
FAILED_PHASES=()

note() { printf '  %s\n' "$*"; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
fail() { printf '\033[31m  ✗\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); FAILED_PHASES+=("$1"); }
manual() { printf '\033[33m  ⚠\033[0m %s (manual)\n' "$*"; MANUAL=$((MANUAL+1)); }

run() { # label, command...
  local label="$1"; shift
  if "$@" >/tmp/sunoto-verify.out 2>&1; then
    ok "$label"
    return 0
  else
    fail "$label"
    tail -n 15 /tmp/sunoto-verify.out | sed 's/^/      /'
    return 1
  fi
}

universal_gate() {
  echo "== universal gate (macOS build/clippy/test + Python suites) =="
  command -v cargo >/dev/null || { fail "cargo on PATH (install rustup)"; return 1; }
  run "cargo build --workspace"            cargo build --workspace
  run "cargo clippy -D warnings"           cargo clippy --workspace --all-targets -- -D warnings
  run "cargo test --workspace"             cargo test --workspace
  run "python tests/phase0"                $PYTHON -m unittest discover -s tests/phase0
  run "python tests/phase1"                $PYTHON -m unittest discover -s tests/phase1
  run "python tests/phase2"                $PYTHON -m unittest discover -s tests/phase2
  run "python tests/ui"                    $PYTHON -m unittest discover -s tests/ui
}

phase0() {
  echo "== phase 0: ASR feasibility (Nemotron offline CPU + CPU/MPS benchmark) =="
  [ -f "services/asr/setup_macos_runtime.sh" ] && ok "setup_macos_runtime.sh exists" || fail "setup_macos_runtime.sh missing"
  [ -f "services/asr/phase0_macos_measure.py" ] && ok "phase0_macos_measure.py exists" || fail "phase0_macos_measure.py missing"
  [ -f "docs/macos-phase-0-results.md" ]        && ok "macos-phase-0-results.md exists" || fail "macos-phase-0-results.md missing"
  if grep -q 'asr_device = "cpu"' docs/macos-phase-0-results.md 2>/dev/null \
    && grep -q 'backend = "nemotron_offline"' docs/macos-phase-0-results.md 2>/dev/null; then
    ok "phase 0 docs record offline CPU as the macOS ASR path"
  else
    fail "phase 0 docs do not record offline CPU as the macOS ASR path"
  fi
  if grep -q "hf-internal-testing/librispeech_asr_dummy" services/asr/phase0_macos_measure.py 2>/dev/null; then
    ok "real-speech Hugging Face benchmark source configured"
  else
    fail "phase0 benchmark missing Hugging Face real-speech source"
  fi
  if [ -f "build/phase0/macos-real-speech-cpu-limit5.json" ]; then
    if grep -q '"actual_device": "cpu"' build/phase0/macos-real-speech-cpu-limit5.json 2>/dev/null; then
      ok "CPU real-speech measurement recorded (actual_device=cpu)"
    else
      fail "macos-real-speech-cpu-limit5.json present but no cpu result"
    fi
  else
    manual "run phase0_macos_measure.py --device cpu to record CPU real-speech latency"
  fi
}

phase1() {
  echo "== phase 1: build skeleton =="
  [ -d "crates/sunoto-desktop" ] && ok "crates/sunoto-desktop exists" || fail "crates/sunoto-desktop missing"
  [ -d "crates/sunoto-macos" ]   && ok "crates/sunoto-macos exists"   || fail "crates/sunoto-macos missing"
  [ -f "crates/sunoto-linux/src/x11/linux.rs" ]  && ok "x11/linux.rs split exists"  || fail "x11/linux.rs missing"
  [ -f "crates/sunoto-linux/src/x11/stub.rs" ]   && ok "x11/stub.rs exists"        || fail "x11/stub.rs missing"
  if grep -rq "sunoto_desktop" apps/daemon/src/*.rs; then ok "daemon imports sunoto-desktop"; else fail "daemon not using sunoto-desktop"; fi
  if grep -q "DesktopBackend::Macos" apps/daemon/src/daemon.rs; then ok "DesktopBackend::Macos present"; else fail "DesktopBackend::Macos missing"; fi
}

phase5() {
  echo "== phase 5: offline Nemotron sidecar + backend wiring =="
  [ -f "services/asr/nemotron_offline_sidecar.py" ] && ok "nemotron_offline_sidecar.py exists" || fail "nemotron_offline_sidecar.py missing"
  [ -f "tests/phase1/test_nemotron_offline_sidecar.py" ] && ok "offline sidecar tests exist" || fail "offline sidecar tests missing"
  if grep -q '"nemotron_offline"' apps/daemon/src/settings.rs; then ok "settings accepts nemotron_offline"; else fail "settings missing nemotron_offline"; fi
  if grep -q '"macos"' apps/daemon/src/settings.rs; then ok "settings accepts macos overlay_backend"; else fail "settings missing macos overlay_backend"; fi
  if grep -q 'default="cpu"' services/asr/nemotron_offline_sidecar.py; then ok "offline sidecar defaults to CPU"; else fail "offline sidecar does not default to CPU"; fi
  # CLI smoke: config round-trips nemotron_offline without needing X11.
  local cfg=/tmp/sunoto-verify-phase5.json
  rm -f "$cfg"
  if SUNOTO_CONFIG="$cfg" ./target/debug/sunoto-daemon config init >/dev/null 2>&1; then
    if $PYTHON -c "import json,sys;p='$cfg';d=json.load(open(p));d['backend']='nemotron_offline';d['overlay_backend']='macos';json.dump(d,open(p,'w'))" 2>/dev/null \
      && SUNOTO_CONFIG="$cfg" ./target/debug/sunoto-daemon config show >/dev/null 2>&1; then
      ok "config round-trips backend=nemotron_offline, overlay_backend=macos"
    else
      fail "config round-trip for nemotron_offline/macos"
    fi
  else
    fail "sunoto-daemon config init"
  fi
  rm -f "$cfg"
  # Offline sidecar imports without NeMo and CLI works.
  if PYTHONPATH=services/asr $PYTHON services/asr/nemotron_offline_sidecar.py --help >/dev/null 2>&1; then
    ok "offline sidecar --help (numpy/NeMo-free import)"
  else
    fail "offline sidecar import/--help"
  fi
}

phase2() {
  echo "== phase 2: CoreAudio capture =="
  if [ -f "crates/sunoto-audio/src/macos.rs" ] || { grep -rlI 'extern "C"' crates/sunoto-audio/src/*.rs >/dev/null 2>&1 && grep -rlI 'AudioObject\|AudioDevice\|AVAudioEngine' crates/sunoto-audio/src/*.rs >/dev/null 2>&1; }; then
    ok "sunoto-audio has CoreAudio capture implementation"
  else
    fail "sunoto-audio CoreAudio capture not implemented"
  fi
  manual "live CoreAudio capture needs Microphone TCC permission (run sunoto-daemon check on macOS)"
}

phase3() {
  echo "== phase 3: CGEventTap hotkey =="
  if [ -f "crates/sunoto-macos/src/hotkey.rs" ] || { grep -rlI 'extern "C"' crates/sunoto-macos/src/*.rs >/dev/null 2>&1 && grep -rlI 'CGEventTap' crates/sunoto-macos/src/*.rs >/dev/null 2>&1; }; then
    ok "sunoto-macos has CGEventTap hotkey implementation"
  else
    fail "sunoto-macos CGEventTap hotkey not implemented"
  fi
  manual "live event tap needs Accessibility/Input Monitoring TCC permission"
}

phase4() {
  echo "== phase 4: CGEvent insertion + clipboard + focus =="
  if [ -f "crates/sunoto-macos/src/insertion.rs" ] || { grep -rlI 'extern "C"' crates/sunoto-macos/src/*.rs >/dev/null 2>&1 && grep -rlI 'CGEventCreate\|CGEventSetUnicodeString' crates/sunoto-macos/src/*.rs >/dev/null 2>&1; }; then
    ok "sunoto-macos has CGEvent insertion implementation"
  else
    fail "sunoto-macos CGEvent insertion not implemented"
  fi
  if [ -f "crates/sunoto-macos/src/insertion.rs" ] || { grep -rlI 'extern "C"' crates/sunoto-macos/src/*.rs >/dev/null 2>&1 && grep -rlI 'NSPasteboard\|NSWorkspace\|AXUIElement' crates/sunoto-macos/src/*.rs >/dev/null 2>&1; }; then
    ok "sunoto-macos has clipboard/focus implementation"
  else
    fail "sunoto-macos clipboard/focus not implemented"
  fi
  manual "live insertion test: focus TextEdit, run sunoto-daemon insert 'hello' (needs Accessibility)"
}

phase6() {
  echo "== phase 6: overlay UI =="
  if [ -f "services/macos/sunoto-overlay.swift" ] && grep -q "NSPanel" services/macos/sunoto-overlay.swift; then
    ok "native Swift NSPanel overlay sidecar exists"
  else
    fail "native Swift NSPanel overlay sidecar missing"
  fi
  if grep -q 'overlay_backend == "macos"' apps/daemon/src/settings.rs && grep -q "sunoto-overlay" apps/daemon/src/settings.rs; then
    ok "daemon settings wire overlay_backend=macos to native sidecar"
  else
    fail "daemon does not wire overlay_backend=macos to native sidecar"
  fi
  manual "overlay pill requires a GUI session to verify visually"
}

phase7() {
  echo "== phase 7: GUI login item + install =="
  [ -f "install-macos.sh" ] && ok "install-macos.sh exists" || fail "install-macos.sh missing"
  [ -x "services/macos/sunoto-login" ] && ok "GUI login launcher exists" || fail "GUI login launcher missing"
  if grep -q 'make login item' install-macos.sh && grep -q 'Sunoto Login.app' install-macos.sh; then
    ok "installer registers Sunoto Login.app"
  else
    fail "installer does not register GUI login item"
  fi
  if grep -q 'launchctl bootout' install-macos.sh; then
    ok "installer removes obsolete LaunchAgent"
  else
    fail "installer does not remove obsolete LaunchAgent"
  fi
  manual "Login Item registration and live GUI/TCC event tap require an interactive session"
}

phase8() {
  echo "== phase 8: docs =="
  [ -f "docs/macos-port-plan.md" ]      && ok "macos-port-plan.md exists"      || fail "macos-port-plan.md missing"
  [ -f "docs/macos-phase-0-results.md" ] && ok "macos-phase-0-results.md exists" || fail "macos-phase-0-results.md missing"
  if grep -qi "macOS\|macos" README.md 2>/dev/null; then ok "README mentions macOS"; else fail "README missing macOS section"; fi
}

# ---- dispatch ----

ALL_PHASES="0 1 2 3 4 5 6 7 8"
if [ "$#" -gt 0 ]; then
  PHASES="$*"
else
  PHASES="$ALL_PHASES"
fi

# The universal gate underpins every code phase; run it once unless only
# doc/asset phases were requested.
CODE_PHASES="0 1 2 3 4 5 6 7"
NEEDS_GATE=0
for p in $PHASES; do
  for c in $CODE_PHASES; do [ "$p" = "$c" ] && NEEDS_GATE=1; done
done
if [ "$NEEDS_GATE" -eq 1 ]; then
  universal_gate
  echo
fi

for p in $PHASES; do
  "phase$p"
  echo
done

echo "== summary =="
note "passed: $PASS   failed: $FAIL   manual: $MANUAL"
if [ "$FAIL" -gt 0 ]; then
  printf '\033[31m  failed phases:\033[0m %s\n' "${FAILED_PHASES[*]}"
  exit 1
fi
exit 0

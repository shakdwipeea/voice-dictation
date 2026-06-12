# Phase 1 Progress

> **Superseded:** this snapshot describes the in-progress state mid-way
> through Phase 1. The completed state, the architecture as built, and the
> measured exit-gate results are in [phase-1-results.md](phase-1-results.md);
> the pre-completion code review that reshaped this work is in
> [code-review-2026-06-12.md](code-review-2026-06-12.md).

**Date:** June 12, 2026  
**Status:** Headless X11 foundation implemented but not committed. Phase 1 is
not complete.

## Phase 1 Goal

Build an X11 vertical slice that lets a user hold a global shortcut, speak, and
receive the final transcription at the cursor in any writable focused X11 text
field. The selected ASR model must remain loaded between dictations, and the
complete path must meet the release-to-insertion latency target.

## Implemented

### Rust Workspace

- Created a Cargo workspace with:
  - `apps/daemon`
  - `crates/sunoto-core`
  - `crates/sunoto-ipc`
  - `crates/sunoto-linux`
- Added offline Cargo build, test, check, self-test, and run commands to the
  `Makefile`.

### Session Core

- Push-to-talk session state machine covering idle, recording, transcribing,
  finalization, and error states.
- Idempotent press and release handling.
- Rejection of stale final results from an older session.
- Bounded audio pre-roll data structure that retains only the latest samples.

### ASR Sidecar Contract

- Typed newline-delimited JSON protocol for:
  - Health checks
  - Starting and finishing sessions
  - Audio chunks
  - Partial and final transcripts
  - Cancellation and errors
- Managed sidecar process lifecycle from the Rust daemon.
- Protocol-compatible Python mock sidecar for daemon development and tests.

### X11 Integration

- Native X11/XTEST Rust adapter.
- Global `Ctrl+F8` push-to-talk press and release events.
- Shortcut registration across normal, Caps Lock, Num Lock, and combined
  modifier states.
- Detectable auto-repeat enabled to avoid repeated press/release cycles while
  holding the shortcut.
- ASCII text insertion at the currently focused X11 cursor.
- Private X11 self-tests for focused-cursor insertion and global push-to-talk.

### Headless Daemon

- Starts and health-checks the managed mock ASR sidecar.
- Registers the global push-to-talk shortcut.
- Starts a session on shortcut press.
- Requests final text on shortcut release.
- Inserts the mock final text at the focused X11 cursor.

## Verification Completed

- Rust workspace unit tests: **7 passed**
- Rust `clippy` with warnings denied: **passed**
- Phase 0 Python tests: **13 passed**
- Managed mock-sidecar process test: **passed**
- Live X11/XTEST capability check: **passed**
- Brief daemon startup and global-hotkey registration: **passed**
- Private focused-cursor insertion self-test: **passed on one run**
- Combined X11 insertion and push-to-talk self-test: **intermittent failure**

## Known Issue

The private X11 self-test has an intermittent race:

- It maps a private test window and then assigns keyboard focus.
- A fixed delay does not guarantee that the X server/window manager considers
  the window viewable.
- When focus is assigned too early, X11 returns `BadMatch` from
  `X_SetInputFocus`.

The self-test must wait for the matching `MapNotify` event before assigning
focus. This affects the test harness, not the normal daemon path, but it should
be fixed before committing the Phase 1 foundation.

## Remaining Phase 1 Work

### Required For A Functional Dictation Slice

- Replace the mock sidecar with a persistent, always-warm Nemotron sidecar.
- Keep model weights and streaming caches alive between dictation sessions.
- Add persistent microphone capture.
- Feed pre-roll audio when push-to-talk begins.
- Stream live audio chunks to the sidecar while the shortcut is held.
- Flush residual audio immediately on shortcut release.
- Add VAD for diagnostics and optional hands-free behavior without delaying
  push-to-talk finalization.
- Handle sidecar crashes, timeouts, cancellation, and restart.

### Required For Reliable X11 Insertion

- Fix the private X11 self-test `MapNotify` race.
- Add clipboard-preserving paste fallback for Unicode and characters that
  cannot be injected directly.
- Preserve and restore clipboard contents safely.
- Detect focus changes between recording start and final insertion.
- Disable insertion into password fields where detection is available.
- Test generic insertion across the desktop compatibility matrix. Named
  applications are test coverage, not an allowlist.

### Required For Product Behavior

- Add settings persistence for shortcut, microphone, profile, and backend.
- Add a recording/transcribing/error status bubble.
- Install GTK4 development packages before implementing the planned GTK4
  status UI.
- Add clean shutdown and signal handling.
- Add structured logging and local diagnostics.

### Required For The Phase 1 Exit Gate

- Measure time to first partial result.
- Measure release-to-final-ASR latency.
- Measure release-to-cursor-insertion latency at p50, p95, and p99.
- Verify repeated dictation without restarting Sunoto or manually pasting.
- Confirm operation in at least five diverse X11 applications while retaining
  the requirement to work in any writable focused X11 text field.

## Current Commands

```bash
make phase1-test
make phase1-check
make phase1-selftest
make phase1-run
```

`make phase1-run` currently uses the mock sidecar. Hold `Ctrl+F8` and release it
to insert the configured mock final text at the focused cursor.

## Git State

- Phase 0 baseline is committed as `4bed7bd`.
- All Phase 1 files and changes described here are currently uncommitted.

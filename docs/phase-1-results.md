# Phase 1 Results: X11 Vertical Slice

**Date:** June 12, 2026
**Status:** Implementation complete and latency exit gate passed with the real
Nemotron sidecar. One manual validation remains: the five-application desktop
compatibility pass, which needs a human at the keyboard.

## What Phase 1 Delivers

A working push-to-talk dictation slice on X11:

1. The daemon keeps the microphone open, the Nemotron model warm on the GPU,
   and a global `Ctrl+F8` grab registered.
2. Holding the shortcut streams 20 ms audio frames (plus a 300 ms pre-roll)
   to the ASR sidecar; partial transcripts appear in a status bubble.
3. Releasing the shortcut flushes the residual audio, runs the deterministic
   cleanup pipeline (Phase 2), and types the final text at the focused cursor.
4. Errors, timeouts, sidecar crashes, and microphone disconnects all recover
   without restarting the daemon.

## Architecture As Built

```text
hotkey thread            audio thread             sidecar reader thread
HotkeyListener           sunoto-audio (parec)     sunoto-ipc event pump
(own X11 connection)     20ms i16 frames          NDJSON -> SidecarMessage
      \                       |                       /
       \                      v                      /
        +----------> main control loop <------------+
                     SessionMachine + pre-roll
                     watchdogs, sidecar respawn
                              |
                              v
                     UI thread (own X11 connection)
                     insertion + clipboard + status bubble
```

- **`crates/sunoto-core`** — push-to-talk state machine (`Idle -> Recording ->
  Transcribing -> Idle`). Failures always return to `Idle`; a stale final for
  an old session is rejected; partials are validated against the current
  session.
- **`crates/sunoto-ipc`** — sidecar process management with an event-pump
  reader thread. The protocol is the same newline-delimited JSON, but the
  client no longer assumes one reply per request: partials and errors arrive
  whenever the sidecar emits them. Non-JSON stdout lines are reported and
  skipped, never fatal.
- **`crates/sunoto-audio`** — persistent `parec` capture (16 kHz mono s16le,
  20 ms frames) with automatic respawn/backoff on process death, and
  monitor-source-rejecting device resolution ported from the Phase 0 probe.
  The OS default source on the target machine *is* the speaker monitor, so
  this guard is load-bearing.
- **`crates/sunoto-linux`** — raw Xlib/XTEST adapters:
  - `HotkeyListener`: configurable `Modifier+Key` grab across lock-modifier
    combinations, poll-based waiting (clean shutdown, ~0% idle CPU), and
    auto-repeat pair filtering when detectable auto-repeat is unsupported.
  - `UiAdapter`: direct XTEST typing, native clipboard save/serve/restore
    with paste fallback, focus-change guard, and an override-redirect status
    bubble that cannot steal focus.
- **`crates/sunoto-polish`** — deterministic cleanup pipeline (Phase 2 scope,
  see `phase-2-results.md`).
- **`apps/daemon`** — control loop, settings file, structured logging,
  latency bench, self-tests.
- **`services/asr/nemotron_sidecar.py`** — persistent always-warm cache-aware
  streaming engine. Loads the model once, replicates NeMo's official
  per-chunk streaming simulation for live incremental audio (stable-frame
  mel extraction, pre-encode feature cache, `conformer_stream_step` with
  cache tensors), pads and flushes on finish, and isolates the protocol
  layer so it is unit-testable without a GPU. Stdout is reserved for the
  protocol by redirecting fd 1 to stderr at startup.

## Review Fixes Landed In This Phase

A 44-finding adversarially-verified review ran before this work (see
`code-review-2026-06-12.md`). The high-severity fixes:

| Finding | Fix |
| --- | --- |
| Releasing Ctrl before F8 dropped the hotkey release; recording never stopped | Releases match on keycode only; a self-test now fakes exactly that ordering |
| Text injected while Ctrl physically held became Ctrl+chords | Physically held modifiers are released around injection (`XQueryKeymap`-precise) and restored after |
| Lockstep request/response IPC died on the first unsolicited partial | Event-pump reader thread; requests are fire-and-forget with session-id routing |
| One sidecar error / unexpected event wedged or killed the daemon | All sidecar events handled; failures return the machine to `Idle`; bubble shows the error |
| Hung sidecar blocked forever | `final_timeout_ms` watchdog cancels and recovers |
| Sidecar crash killed the daemon | `Closed` event triggers respawn with capped backoff |
| No Xlib error handler meant any async X error exited the process | Logging error handler installed |
| `MapNotify` race in the insertion self-test | Waits for `MapNotify` before `XSetInputFocus` (5/5 repeated runs pass) |
| Unsupported characters killed the daemon mid-insertion | Atomic keycode resolution first; clipboard-paste fallback for non-ASCII |
| No signal handling | SIGINT/SIGTERM set a flag; session cancelled, threads joined, exit 0 |

Injection safety (from the plan review): final text is sanitized before
insertion — control characters are dropped and Enter/Tab become spaces unless
`allow_enter_and_tab` is set, so a dictation or snippet can never execute a
command in a focused terminal. If focus changed between release and
insertion, the text goes to the clipboard instead of the wrong window.

## Measured Latency (Phase 1 Exit Gate)

Harness: `sunoto-daemon bench` paces the 13.69 s reference recording through
the real sidecar in 20 ms chunks as if spoken live, treats the last chunk as
the shortcut release, and types the final text into a real focused X11 window,
counting until every character has echoed back. Five sessions per profile on
the RTX 3060 (driver 570.153.02, CUDA 12.8 runtime).

| Metric (ms) | 160 ms profile p50 / p95 | 80 ms profile p50 / p95 | Target |
| --- | --- | --- | --- |
| Release to final ASR text | 92 / 125 | 78 / 88 | < 450 p95 |
| Insertion (type + echo) | 19 / 26 | 15 / 16 | < 50 p95 |
| **Release to insertion** | **110 / 151** | **93 / 104** | **< 600 p95** |
| Time to first partial (speech onset) | 827 / 848 | 708 / 714 | see note |

- **Exit gate: passed.** Release-to-insertion p95 is 151 ms (160 ms profile)
  and 104 ms (80 ms profile) against the 600 ms target.
- Transcripts were identical across all repeated sessions (no cross-session
  state leakage) and match the Phase 0 reference transcription.
- Warm start (spawn to ready, model load + warmup): 32.3 s first load,
  16.6 s with a warm page cache. This misses the plan's provisional
  "warm-model under 10 s after login" guess; it is startup cost, not
  dictation latency, and is recorded in the amended plan.
- Sidecar VRAM while streaming: ~3.6 GiB (4,931 MiB total minus the ~1,350
  MiB desktop baseline) on the 12 GiB card.
- **First-partial note:** time to first partial is dominated by the
  cache-aware encoder's first chunk (~720 ms of audio for `[70,1]`), not by
  inference speed. The original 250 ms target was written without an anchor;
  the amended plan defines the metric and resets the target from these
  measurements. Release-to-final latency — what push-to-talk users feel — is
  unaffected.

## Verification

- `cargo test --workspace --offline` — 54 tests across 6 crates, all passing.
- `cargo clippy --workspace --offline --all-targets -- -D warnings` — clean.
- `python3 -m unittest discover -s tests/phase0` — 13 tests.
- `python3 -m unittest discover -s tests/phase1` — 18 sidecar-protocol tests
  (no GPU needed; the engine is injectable).
- `sunoto-daemon check` — live X11 grab, XTEST, and mock-sidecar health: ok.
- `sunoto-daemon selftest` × 5 consecutive runs — insertion echo, clipboard
  round-trip through the real selection protocol, and push-to-talk with the
  modifier released first: all passed every run (the old intermittent
  failure is gone).
- Daemon startup/shutdown on the live desktop: microphone resolved to the
  physical source (not the monitor default), sidecar ready, SIGTERM exits 0
  within 50 ms.
- GPU bench runs: `build/phase1/bench-nemotron-{160,80}-paced.json`,
  `build/phase1/bench-mock.json`.

## Remaining Before Closing Phase 1

- **Manual desktop compatibility pass** (needs a human): `make
  phase1-run-nemotron`, then dictate into at least five applications from
  different toolkits — e.g. Firefox, VS Code, GNOME Terminal, xed (GTK),
  LibreOffice Writer — confirming repeated dictation without restarting and
  no first/last-word loss. The applications are test coverage, not an
  allowlist.
- Live-microphone spot check of accuracy with real speech (the bench uses
  the recorded corpus; `make phase0-audio` verifies capture quality).

## Commands

```bash
make phase1-test            # Rust tests + clippy + sidecar protocol tests
make phase1-check           # live X11/XTEST/sidecar health check
make phase1-selftest        # X11 insertion, clipboard, push-to-talk self-tests
make phase1-run             # daemon with the mock sidecar
make phase1-run-nemotron    # daemon with the always-warm Nemotron sidecar
make phase1-bench-mock      # insertion-path latency with the mock
make phase1-bench-nemotron  # full latency percentiles on the GPU
```

Settings live at `~/.config/sunoto/config.json` (`sunoto-daemon config init`
writes the defaults; `config show` prints the effective values).

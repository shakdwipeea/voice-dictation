# Code & Plan Review — June 12, 2026

A multi-agent review (five dimensions: Rust correctness, Python/C Phase 0
code, latency architecture, robustness/recovery, plan critique) with
adversarial verification of every finding ran against the Phase 0/1 codebase
before Phase 1 completion. 49 raw findings → **44 confirmed, 5 refuted**.
Full machine-readable findings: `build/review-findings.json` (local artifact).

The 44 confirmed findings collapse into the root causes below, each with its
resolution. "Fixed" means landed and tested in the Phase 1 completion work
(see `phase-1-results.md`).

## Code findings

| # | Root cause (severity) | Resolution |
| --- | --- | --- |
| 1 | Lockstep request/response IPC: first unsolicited partial kills the daemon; `AudioChunk` had no reply and would deadlock; stray stdout line fatal (high, 4 findings) | **Fixed** — event-pump reader thread in `sunoto-ipc`; fire-and-forget sends; `Garbage`/`Closed` messages |
| 2 | One sidecar error wedges the daemon forever (`Error`/`Transcribing` states had no exit) (high, 4 findings) | **Fixed** — `fail()` returns the machine to `Idle`; `final_timeout_ms` watchdog cancels hung sessions |
| 3 | Hotkey release dropped when Ctrl released before F8 (high, 2 findings) | **Fixed** — release matches keycode only; self-test fakes that exact ordering |
| 4 | Text injected while Ctrl physically held becomes Ctrl+chords (high, 2 findings; also Phase 0 probe, medium) | **Fixed** — held modifiers cleared around injection in Rust; probe releases Ctrl before typing |
| 5 | Sidecar crash/hang kills or blocks the daemon; no respawn (high, 2 findings) | **Fixed** — `Closed` triggers respawn with capped backoff; watchdog covers hangs |
| 6 | No `XSetErrorHandler`: any async X error exits via Xlib's default handler (high/medium, 2 findings) | **Fixed** — logging handler installed once |
| 7 | Single-threaded loop cannot host audio/partials; synchronous `StartSession` on the press path (high/medium, 2 findings) | **Fixed** — four-thread architecture; all sends asynchronous |
| 8 | Unsupported (non-ASCII) character exits the daemon mid-insertion (medium, 2 findings) | **Fixed** — atomic keycode resolution; clipboard-paste fallback |
| 9 | US-layout hardcoded shift pairs in `character_to_keysym` (medium) | **Mitigated** — clipboard path covers unmappable text; full keymap-aware resolution deferred and recorded in the plan |
| 10 | `nemotron_benchmark.py` parsed Python list repr with `json.loads`, silently dropping most transcripts (medium) | **Fixed** — `ast.literal_eval` first, JSON fallback |
| 11 | Phase 0 probes: `parec` leak on timeout, uncaught `CalledProcessError`/`OSError`, capture shorter than requested (low, 4 findings) | **Fixed** — kill+wait on timeout, broadened handlers, startup grace |
| 12 | Mock sidecar replied to stale audio chunks, desyncing the stream (low) | **Fixed** — stale chunks ignored silently |
| 13 | Detectable auto-repeat support unverified → press/release storms (low, 2 findings) | **Fixed** — support flag honored; release+press pairs with equal timestamps swallowed |
| 14 | Sidecar path baked in via `CARGO_MANIFEST_DIR` (low, 2 findings) | **Fixed** — settings overrides + `SUNOTO_ROOT`; dev fallback retained |
| 15 | No signal handling; grabs/children leak on SIGINT (low) | **Fixed** — SIGINT/SIGTERM → orderly shutdown, exit 0 |
| 16 | JSON `i16` arrays inflate the audio hot path ~5× (low) | **Accepted for Phase 1** — ~150 KB/s at dictation rates; binary/base64 framing noted as a later optimization |
| 17 | `SessionMachine` lacked partial/awaiting states (low) | **Fixed** — partial validation added; awaiting-start state judged unnecessary (ordered writes) |

## Plan findings (all addressed in `product-plan.md` amendments)

| # | Finding (severity) | Amendment |
| --- | --- | --- |
| P1 | No security treatment of synthetic text injection: Enter into a terminal executes commands; focus can change before insertion; clipboard fallback exposes dictated secrets to clipboard managers (high) | New "Injection safety" subsection in section 5; sanitizer + focus guard implemented |
| P2 | Phase 1 started with the Phase 0 latency gate unmet, plan never amended (medium) | Section 8 records the gate carry-over and its now-measured result |
| P3 | 250 ms first-partial target has no anchor, unfalsifiable (medium) | Section 9 defines the anchor (speech onset incl. pre-roll) and resets the target from measurements |
| P4 | XTEST keyboard-layout dependence omitted (non-US layouts, MappingNotify) (medium) | Section 5 records the limitation, mitigation, and matrix addition |
| P5 | Audio stack decision deferred; Phase 0 monitor-source/buffering hazards never folded back (medium) | Section 4 commits to PulseAudio-protocol capture with monitor rejection and 20 ms buffer-attr requirements |
| P6 | Phase 1 scope didn't require streaming-capable IPC; Unix-socket vs stdio divergence (medium) | Section 8 Phase 1 scope now requires async partial events; section 4 records the deliberate stdio-NDJSON choice |
| P7 | Targets that were to come from Phase 0 measurement were still guesses (warm start, VRAM); idle-CPU vs always-on VAD tension; orphaned test corpus (low, 3 findings) | Section 9 annotated measured vs. provisional; VAD clarified as session-only; corpus recording moved into Phase 2 scope |

## Refuted findings (for the record)

Five findings were rejected by adversarial verification, chiefly: mock
sidecar crash-on-malformed-stdin (unreachable — the only writer is the typed
Rust client), "stale Final wedges the daemon" as stated (impossible under the
then-synchronous IPC; the real issue is the timeout gap, finding #2), and
"adapter `!Send` blocks the multi-threaded redesign" (refuted — one
connection per thread, which is exactly the architecture now in place).

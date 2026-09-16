# Sunoto architecture

Written 2026-09-16 against commit `b5234d7b` plus the uncommitted System-mode
work in the working tree. Audience: a senior developer who has never opened
this repo. First half is the shape and the trade-offs; second half is the
reference detail.

## What it is in one paragraph

Sunoto is a local push-to-talk dictation daemon. You hold `Ctrl+F1`, speak,
release, and the transcript lands in whatever window has focus. One Rust
binary (`sunoto-daemon`) owns the hotkey, microphone, session state, text
cleanup, and insertion. Everything heavy or platform-flavoured runs in a child
process it talks to over newline-delimited JSON on stdin/stdout: the speech
model, the optional LLM clean-up pass, and the on-screen overlay. Nothing
leaves the machine.

## The big picture

```
   Ctrl+F1 held ───────────────────────────────────────────────────────┐
   (X11 XGrabKey | macOS CGEventTap | Wayland: compositor → unix socket) │
                                                                        ▼
 ┌──────────────────────── sunoto-daemon (Rust, apps/daemon) ───────────────────────┐
 │                                                                                  │
 │  mic ──► capture thread ──► 16 kHz i16, 20 ms frames ──► main event loop         │
 │  (parec | CoreAudio)         + 300 ms pre-roll ring     (single thread, 50 ms    │
 │                                                          tick, SessionMachine)   │
 │                                                             │       │       │    │
 │            audio_chunk / start / finish (NDJSON) ◄──────────┘       │       │    │
 │            ▼                                                        │       │    │
 │   ┌───────────────────────┐  partial / final (NDJSON)               │       │    │
 │   │ ASR sidecar (Python)  │ ─────────────────────────► final text    │       │    │
 │   │ parakeet_mlx (Metal)  │                               │          │       │    │
 │   │ nemotron (CUDA)       │                               ▼          │       │    │
 │   │ mock                  │                  sunoto-polish (Rust,    │       │    │
 │   └───────────────────────┘                  deterministic, in-proc) │       │    │
 │                                                          │          │       │    │
 │   ┌───────────────────────┐   polish{text} / polished     ▼          │       │    │
 │   │ LLM polish sidecar    │ ◄────────────────────── (opt, default on)│       │    │
 │   │ llama.cpp GGUF, Phi-4 │ ───────────────────────► polished text   │       │    │
 │   └───────────────────────┘                              │          │       │    │
 │                                                          ▼          ▼       │    │
 │   ┌───────────────────────┐   show/recording/status   UI thread   overlay    │    │
 │   │ overlay sidecar       │ ◄──── bounded chan(64) ── (insert)    writer     │    │
 │   │ Swift NSPanel | GTK4  │        try_send, drops        │                  │    │
 │   └───────────────────────┘        on backpressure        │                  │    │
 │                                                           ▼                  │    │
 │                             paste (pbcopy+Cmd+V | XTEST Ctrl+V | wl-copy+wtype)   │
 │                             fallback: synthesize keystrokes; last resort: clipboard│
 └───────────────────────────────────────────────────────────────────────────────────┘
                                                           │
                                                           ▼
                                                  focused application
```

Three things to hold in your head:

1. **One event loop owns all policy.** `apps/daemon/src/daemon.rs` is the only
   place that decides what happens. Every other thread is a dumb bridge that
   turns an OS event or a sidecar line into a `DaemonEvent` on one channel.
2. **Sidecars are replaceable processes, not libraries.** The ASR model, the
   LLM, and the overlay each speak a tiny NDJSON protocol. A crash is a
   restart with backoff, never a daemon crash.
3. **Text goes in, text comes out, and only text.** Dictation never executes
   anything. The separate System mode (voice commands) is fenced off by a
   different hotkey, a different session mode, and a type system that has no
   "run a string" variant.

## A dictation session, step by step

| Step | What happens | Where |
| --- | --- | --- |
| Idle | Mic is always open. Frames go into a 300 ms pre-roll ring so the first syllable is not lost. | `daemon.rs` audio handler, `sunoto-core::AudioPreRoll` |
| Press | Refused if ASR is not ready or the LLM has not warmed. Otherwise `SessionMachine` moves `Idle → Recording{id}`, pre-roll is flushed to the sidecar as the first `audio_chunk`. | `daemon.rs` hotkey handler |
| Recording | Each 20 ms frame is forwarded as a JSON array of 320 i16 samples and mirrored to the overlay as a peak/rms meter. Streaming backends return `partial` text. | `daemon.rs`, ASR sidecar |
| Release | `Recording → Transcribing`. Focus is captured now, not later. Watchdog set to `8000 ms + 3.0 × recorded_ms`. `finish_session` sent. | `daemon.rs` |
| Final | Sidecar returns `final{text}`. Stale ids are dropped. `Transcribing → Idle`. | `sunoto-core::SessionMachine` |
| Polish 1 | Deterministic pipeline: normalize → resolve "no wait, I mean" swaps → strip fillers → dictionary → snippets → style (terminal vs prose) → normalize. Style is picked by the focused window's class. | `crates/sunoto-polish` |
| Polish 2 | LLM pass, constrained by a GBNF grammar to answer `OK` (1 token) or `EDIT: …`. Only allowed to merge mid-utterance self-corrections. Content-loss guard reverts if 3+ content words vanish. On any error the deterministic text is used. | `apps/daemon/src/llm_polish.rs`, `services/polish/llm_polish_sidecar.py` |
| Insert | If focus moved since release, park on clipboard and say so. Else paste first on macOS/Wayland, type first on X11, with the other as fallback. Enter and Tab become spaces unless configured. | UI thread in `daemon.rs`, platform crates |

Measured on the reference machines: release-to-insertion p50 of about 110 ms
on Linux/CUDA at the 160 ms profile, and Parakeet-MLX final decode of about
240 ms on an M1 Pro. LLM polish adds roughly 250 ms for clean text and 400 to
900 ms for an edit once warm.

## What is good

- **Small, boring core.** Three third-party crates in the whole workspace
  (`serde`, `serde_json`, `url`). CoreAudio, CGEvent, Xlib and XTEST are
  hand-written FFI. Nothing to upgrade, nothing to audit.
- **The session state machine is pure and tested.** `sunoto-core` has zero
  dependencies and encodes the invariants that actually bite: stale finals
  cannot overwrite a live session, failure always returns to Idle, ids never
  repeat, and a System result can never reach the dictation insert path.
- **Latency path is protected by construction.** Overlay writes use
  `try_send` on a bounded channel and drop frames rather than block. LLM
  warm-up happens after ASR is ready, because they share the GPU. A 1-token
  keepalive ping every second stops Metal from cold-ramping, which turned a
  4 s prefill into 200 ms.
- **Failure is graded, not fatal.** ASR crash → restart with 500 ms to 5 s
  backoff. Overlay never came up → fall back to native bubble, no respawn
  loop. LLM warm-up failed → dictation continues without it. Hotkey grab
  failed → daemon stays up and the control socket still works.
- **Protocol hygiene is load-bearing and done.** Every real sidecar dups the
  real stdout and points fd 1 at stderr so library chatter cannot corrupt the
  stream. Unparseable lines become `Garbage` events, not crashes. All newer
  fields are `Option` with `serde(default)`.
- **Safety choices are explicit.** Enter/Tab neutralized by default so a
  terminal never runs dictated text. Focus is re-checked before insertion.
  System mode has no shell, no AppleScript, no `Exec` line parsing, HTTP(S)
  only URLs, and every action requires an explicit palette selection.
- **Good instrumentation.** One log line per session with the full timing
  breakdown (ASR turnaround, polish, dispatch, insert). `bench` and `eval`
  subcommands exist and write JSON.

## What is not good

- **`daemon.rs` is a 3,000 line god file.** It holds the event loop, the
  Wayland adapter, insertion ordering for three platforms, overlay
  management, LLM orchestration, System dispatch, and the control socket.
  The platform facade (`sunoto-desktop`) only abstracts X11 and macOS;
  Wayland lives inline. This is the first thing to split.
- **Duck-typed platform layer.** `sunoto-desktop` is a `cfg` re-export, not
  a trait. Both platform crates must export identically named types, and the
  macOS error type is literally called `X11Error`. Works, but nothing
  enforces it.
- **JSON arrays of i16 for audio.** 50 messages per second of 320 integers
  serialized as text. Fine at 16 kHz mono, but it is the least efficient
  part of the design and would not survive stereo or higher rates.
- **No protocol version.** The handshake is `health → ready{backend}`.
  Compatibility rests on everyone remembering `serde(default)`.
- **Clipboard behaviour differs per platform.** X11 saves and restores the
  previous clipboard. macOS and Wayland overwrite it permanently. Users will
  notice.
- **macOS reliability depends on TCC folklore.** The CGEventTap goes inert
  when the binary is rebuilt (ad-hoc signature changes) or launched from
  launchd (no responsible process). The workaround is a bash login item that
  stays the daemon's parent. `check` can pass while the tap is dead. This is
  the single most recurring bug in the project's history.
- **Transcripts are logged in full.** Every final and every polish stage
  goes to the daemon log at info level. That is a privacy problem for a
  dictation tool and is listed as a release blocker.
- **Python runtime sprawl.** Three separate venvs (`.venv-nemotron`,
  `.venv-nemotron-mac`, `.venv-llm-polish-mac`), one built by hand, plus a
  system Python for the mock and GTK overlay. Install is not one step.
- **Tests stop at the process boundary.** 200-odd unit tests, but no Rust
  integration test drives the real daemon; `system_worker.rs` has zero tests
  and is exercised only by an agent-run E2E harness. Linux System mode has
  never run on a real Linux desktop. The planning docs disagree with each
  other about whether System mode exists.
- **No spoken punctuation or number handling.** "New line", "period",
  "twenty three" are not interpreted. The ASR model's own punctuation is all
  you get.

## Functionality scorecard

Same judgement, but per feature a user touches rather than per crate.

| Feature | Works well | Weak or missing |
| --- | --- | --- |
| **Push-to-talk hotkey** | No silence timeout; works in every app; listen-only tap so the key still reaches the app; two independent shortcuts for dictation and System mode. | On macOS the tap can be silently dead after a rebuild or a launchd launch while the log says "ready". Only F-keys, space, return, tab, esc are valid key names on macOS. Wayland needs the user to edit compositor bindings. |
| **Audio capture** | Mic is always open so a 300 ms pre-roll catches the first word; follows the default input device when it changes; refuses speaker-loopback "monitor" sources; logs rms/peak so a muted mic is diagnosable. | Always-on capture costs energy and is a privacy optic. Resampling is linear interpolation. Audio crosses to Python as JSON integers. No visual mic indicator when idle. |
| **Transcription (ASR)** | Fully local; streaming partials on the macOS default; about 240 ms final on an M1 Pro with 1.2 % WER on the sample set; adaptive timeout so long utterances are not cut; CUDA backend refuses to load into a nearly full GPU. | English only. Press is silently ignored for 2 to 3 s after start (16 to 32 s on Nemotron) with no "warming up" indicator. A timed-out session cannot be cancelled on the sidecar and runs to completion. Model install is a manual `pip install`. Linux default is the mock backend. |
| **Deterministic polish** | Resolves "next Tuesday, no wait, Wednesday" style swaps and "scratch that"; strips fillers; user dictionary and snippet expansion; drops the trailing period in terminals; idempotent and never panics. | No spoken commands ("new line", "comma", "all caps"). No spoken-number handling. Dictionary and snippets are edited by hand in JSON. Swap rules are deliberately conservative and defer harder cases to the LLM. |
| **LLM polish** | Scope is narrow and grammar-enforced: answer `OK` or `EDIT:`. Clean text costs one token, about 250 ms. Falls back to deterministic text on any error. Keepalive removed the multi-second GPU cold ramp. | Blocks the hotkey until warm-up finishes. Pings the GPU every second forever, including on battery. Edits add 400 to 900 ms. The model (about 3 GB) is downloaded by hand into a hand-built venv. Content-loss guard misses short clause drops. Not validated on Linux. |
| **Text insertion** | Focus is captured at release and re-checked before insert, so text never lands in the wrong window. Paste-first on macOS works across Cocoa apps. Enter and Tab are neutralized so a terminal never executes dictation. When focus moved, text is parked on the clipboard and the overlay says so. | Clipboard is overwritten on macOS and Wayland with no restore. No detection of password or secure fields. No undo. App-aware styling needs Screen Recording permission to read app names and degrades silently without it. |
| **Overlay** | Minimal by design: dot and meter while recording, text only for status. Click-through, all Spaces, never blocks the daemon. | No "warming up", "blocked", or "mic muted" states. No menu bar icon, no settings window, no history. Nothing visible at all when the hotkey is inert. On Wayland it is disabled entirely without layer-shell. |
| **Configuration and CLI** | One JSON file with validated defaults; `config show`, `config init`, `check`, `selftest`, `bench`, `eval`, `polish` subcommands. | No GUI. Daemon restart required to apply changes. `check` on macOS passes when the tap is dead. Behaviour also depends on four environment variables. |
| **Install and lifecycle** | One script per platform; the macOS login item preserves the process context the event tap needs; systemd unit with memory limits on Linux. | macOS builds from source and runs out of the working tree. Needs Rust, `swiftc`, Homebrew Python 3.12, and three venvs. Permissions must be granted to two different paths. No signed bundle, no updater, no uninstaller. Rebuilding invalidates permission grants. |
| **Logging and diagnostics** | One line per session with every latency stage; audio stats alongside empty transcripts. | Full transcripts and every polish stage are logged at info level. Log location changes with how the daemon was launched. |
| **System mode (voice commands)** | Strong safety model: typed intents only, no shell, one-time selection tokens revalidated before execution, multi-action input rejected. | Off by default. Only "open application" is proven live. The LLM router is a stub. Never run on a real Linux desktop. |

## macOS first: path to a normal-user package

The engine is done. What a non-developer hits is permissions, warm-up, and
the build-from-source install. Everything below is ordered so each step is
useful on its own and none requires the later ones.

**Phase A. Code-only fixes, no Apple account needed.**

1. **Truthful readiness.** After the tap is created, post a synthetic
   `flagsChanged` event through `CGEventPost` and confirm the tap callback
   sees it within a second. If not, log and display "Hotkey blocked: grant
   Input Monitoring to Sunoto" instead of "ready". Make `check` run the same
   probe so it stops passing on a dead tap.
2. **Warm-up state in the overlay.** Show the pill with a neutral dot and
   "loading speech model" from process start until `ASR sidecar ready`, and
   "warming polish" until the LLM is warm. Pressing during warm-up should
   show that state, not be ignored.
3. **Clipboard restore.** Read `pbpaste` before `pbcopy`, paste, wait about
   300 ms for the target to consume the paste, then write the previous
   contents back. X11 already does this; mirror it.
4. **Secure-field guard.** Ask Accessibility for the focused element's role.
   If it is `AXSecureTextField`, refuse to insert and show "password field,
   text left on clipboard".
5. **Transcript-safe logs.** Log transcript length and a hash by default.
   Add a `log_transcripts: true` setting for debugging.

**Phase B. Packaging decisions the user has to make.**

6. **One real app bundle.** Move the daemon, the Swift overlay, and a bundled
   Python runtime into `Sunoto.app`. Sign with a Developer ID and notarize.
   A stable signing identity is what makes TCC grants survive updates and
   removes the entire "grant to two paths, re-grant after rebuild" class of
   bugs. This needs an Apple Developer account.
7. **Bundle the Python runtime.** Ship python-build-standalone plus
   `parakeet-mlx` and `llama-cpp-python` wheels inside the bundle. Download
   the ASR model (about 600 MB) and the LLM (about 3 GB) on first run into
   Application Support with a progress bar. Decide whether the LLM download
   is default or opt-in; without it the app still dictates in Fast mode.
8. **Menu bar app.** Fold the Swift overlay into a small menu bar app that
   shows state (ready, warming, blocked, recording), opens the permissions
   panes with the right binary preselected, toggles LLM polish, and opens
   the log. This replaces the bash login item.
9. **First-run onboarding.** Walk through Microphone, Accessibility and
   Input Monitoring one at a time and verify each live using the probes from
   step 1. Do not say "ready" until a synthetic press actually starts a
   recording.
10. **DMG plus automated release gate.** Drag-to-Applications DMG. A CI job
    on a macOS runner that builds, signs, notarizes, launches, grants
    permissions via `tccutil` where possible, and runs the live probes.

Steps 1 to 5 are a few days of Rust and Swift with no external dependency.
Steps 6 to 10 turn the project into something a normal user can install.
Step 6 is the one that actually kills the recurring macOS bugs.

## Reference

### Crates and services

| Unit | Lines | Job | Deps |
| --- | --- | --- | --- |
| `apps/daemon` | ~7,500 | CLI, event loop, settings, LLM client, bench/eval, System worker | serde, serde_json, all crates below |
| `crates/sunoto-core` | 358 | `SessionMachine`, `SessionMode`, `AudioPreRoll`. Pure. | none |
| `crates/sunoto-ipc` | 464 | NDJSON request/event enums, `SidecarClient` (spawn + reader thread) | serde, serde_json |
| `crates/sunoto-audio` | 1,548 | Capture. Linux spawns `parec`; macOS is raw CoreAudio IOProc FFI with in-Rust downmix and linear resample to 16 kHz | none |
| `crates/sunoto-polish` | 1,120 | Deterministic text pipeline, 26 tests | serde |
| `crates/sunoto-desktop` | 18 | `cfg` facade re-exporting the platform crate | platform crates |
| `crates/sunoto-linux` | ~2,700 | `XGrabKey` hotkey, XTEST typing, in-process CLIPBOARD owner with restore, native bubble, GIO-based System executor, compile stubs for other OSes | sunoto-system |
| `crates/sunoto-macos` | ~2,400 | `CGEventTap` hotkey on its own run loop with self re-arm, `pbcopy`+Cmd+V paste, CGEvent unicode typing, `CGWindowList` focus detection, NSWorkspace System executor via raw `objc_msgSend` | sunoto-system |
| `crates/sunoto-system` | ~4,400 | Typed intents, deterministic router, policy, capability contracts, dispatcher, bounded runner, URL and filesystem target rules. 59 tests + routing corpus | serde, serde_json, url |
| `services/asr/*.py` | ~2,100 | Five ASR backends behind one `SidecarServer` | NeMo/torch or parakeet-mlx |
| `services/polish/*.py` | ~1,700 | LLM polish sidecar and prompt/parsing module | llama-cpp-python |
| `services/macos/sunoto-overlay.swift` | 572 | AppKit pill + System palette, built with `swiftc` | none |
| `src/voice_dictation/*.py` | ~700 | GTK4 overlay, layer-shell on Wayland, override-redirect on X11 | PyGObject, python-xlib |

### Threads in the daemon

| Thread | Role |
| --- | --- |
| main | `recv_timeout(50 ms)` loop; owns `SessionMachine`; runs watchdogs every tick |
| UI | second desktop connection; focus capture, insertion, clipboard, native bubble |
| control socket | non-blocking `UnixListener`, 25 ms poll |
| hotkey (1 or 2) | one per shortcut; `wait(250 ms)` loop |
| capture | bridges audio events; restart backoff 250/500/1000/2000/4000 ms |
| ASR reader | parses sidecar stdout |
| overlay reader + writer | writer drains `sync_channel(64)` |
| LLM reader | parses polish sidecar stdout |
| SystemWorker | blocking native discovery and execution for System mode |

Signals: raw `libc::signal` for SIGINT/SIGTERM setting an `AtomicBool`.

### Protocols

All are one JSON object per line, tagged by `type`, snake_case.

- **ASR in:** `health`, `start_session{session_id, profile_ms}`,
  `audio_chunk{session_id, samples: [i16]}`, `finish_session`, `cancel_session`.
- **ASR out:** `ready{backend}`, `session_started`, `partial{text}`,
  `final{text}`, `error{session_id?, message}`.
- **Overlay in:** `show`, `hide`, `recording{elapsed_s, peak, rms, segments}`,
  `status{text}`, `segment`, `clear`, `system_palette{session_id, transcript,
  suggestions[]}`, `dismiss_system_palette`, `shutdown`.
  **Out:** `ready`, `system_selection{suggestion_id}`, `system_cancelled`.
  Palette payloads carry no paths or bundle ids by design.
- **LLM polish in:** `polish{session_id, text}`, `warmup{texts}`, `shutdown`.
  **Out:** `ready{load_ms, warmup_ms}`, `polished{text, latency_ms, …}`,
  `polish_chunk{sequence, delta}`, `warmed`, `error`.
- **Control socket** (`$XDG_RUNTIME_DIR/sunoto/daemon.sock` or
  `$TMPDIR/sunoto-$USER-daemon.sock`): bare `press`/`release`,
  `{"type":"trigger","mode","edge"}`, `{"type":"polish","text"}`,
  `{"type":"plan_system","text","dry_run":true}`. This is how Hyprland
  keybindings reach the daemon, since Wayland has no global grab.

### Config defaults

File: `~/Library/Application Support/sunoto/config.json` on macOS,
`~/.config/sunoto/config.json` on Linux. Missing file means all defaults.

| Key | Default | Note |
| --- | --- | --- |
| `shortcut` | `Ctrl+F1` | needs at least one modifier |
| `backend` | macOS `parakeet_mlx_streaming`, Linux `mock` | also `nemotron`, `nemotron_offline`, `parakeet_mlx_offline` |
| `profile_ms` | macOS 560, Linux 160 | 80/160/560/1120; maps to encoder right-context |
| `preroll_ms` | 300 | |
| `final_timeout_ms` / `final_timeout_rtf` | 8000 / 3.0 | adaptive watchdog |
| `microphone` | `auto` | re-checks default source every 1 s and follows it |
| `allow_enter_and_tab` | false | otherwise both become space |
| `overlay_enabled` / `overlay_backend` | true / macOS `macos`, else `auto` | `auto`, `x11`, `wayland`, `macos`, `mock` |
| `polish_enabled` | true | deterministic pipeline |
| `llm_polish_enabled` | true | |
| `llm_polish_model` | `phi4_mini` | Q5_K_M GGUF under `models/llm-polish-hf/` |
| `llm_polish_mode` | `constrained_one_call` | also `one_pass_minimal`, `two_step` |
| `llm_polish_timeout_ms` | 10000 | |
| `llm_polish_keepalive_secs` | 1.0 | 0 disables |
| `llm_polish_stream_insert` | false | types LLM deltas live |
| `system_mode_enabled` | false | |
| `system_shortcut` | `Ctrl+F2` | must differ from `shortcut` |
| `system_search_roots` | Desktop, Documents, Downloads, workspace | under `$HOME`, max 16, no `/` or `..` |

### ASR backends

| Backend | Model | Mode | Device | Partials | Notes |
| --- | --- | --- | --- | --- | --- |
| `mock` | none | canned | – | optional | Linux default; used by tests |
| `parakeet_mlx_streaming` | `mlx-community/parakeet-tdt-0.6b-v3` | streaming, final re-decoded from whole buffer | Metal via MLX | yes | macOS default; 560 ms chunks; ~250 ms final |
| `parakeet_mlx_offline` | same | whole utterance | Metal | no | p50 240 ms, WER 1.18 % on sample set |
| `nemotron` | `nvidia/nemotron-speech-streaming-en-0.6b` | cache-aware streaming | CUDA | yes | needs ≥4500 MiB free VRAM or refuses to load; ~3.6 GiB resident |
| `nemotron_offline` | same | whole utterance | CPU/MPS | no | superseded on macOS |

Venvs: `.venv-nemotron` (Linux CUDA), `.venv-nemotron-mac` (all mac ASR),
`.venv-llm-polish-mac` (LLM, built by hand). Mock and GTK overlay use system
Python.

### Platform matrix

| | X11 | Wayland (Hyprland) | macOS |
| --- | --- | --- | --- |
| Hotkey | `XGrabKey` ×4 lock-mask combos, in-process | none; compositor `exec`s `sunoto-daemon trigger` → unix socket | `CGEventTap` listen-only on its own thread, re-armed on timeout and after sleep |
| Focus token | window id + WM_CLASS | `hyprctl activewindow` address | `CGWindowList` first layer-0 window, skipping Window Server/Dock |
| Insert order | XTEST type → clipboard paste | `wl-copy` + `wtype` paste (Ctrl+Shift+V in terminals) → type → clipboard | `pbcopy` + Cmd+V → CGEvent type → clipboard |
| Clipboard restored | yes, daemon owns CLIPBOARD | no | no |
| Overlay | GTK4 override-redirect, parked off-screen to hide | GTK4 layer-shell; disabled if layer-shell missing | Swift `NSPanel`, status-bar level, click-through |
| Launch | systemd user unit, `MemoryMax=14G` | same | `Sunoto Login.app`: a bash login item that stays parent of the daemon so TCC keeps the tap alive |
| Permissions | none | none | Accessibility, Input Monitoring, Microphone for both the app and the bare binary |

### System mode in five lines

Separate hotkey (`Ctrl+F2`), off by default. Transcript → deterministic
router → one of 9 typed intents or `NeedsLlm` or `Rejected` (multi-action
input like `open Chrome; rm -rf` is rejected, not split). Policy today is
always "present suggestions"; there is no auto-execute variant. Native
resolvers (NSWorkspace + `mdfind` on macOS, GAppInfo + GIO on Linux) return
opaque ids; the palette shows titles only; a one-time selection token is
revalidated against the filesystem before `NSWorkspace openURL` or
`gio launch` runs. Six capabilities exist: `application.find/open`,
`target.find/open`, `url.open`, `web.search`. No shell, no AppleScript, no
LLM planner connected yet.

### Tests

`make test` = Rust workspace (about 230 tests, clippy with `-D warnings`) +
Python `unittest` suites: phase0 (16, tooling), phase1 (88, sidecar
protocols with fake engines, no GPU), phase2 (10, corpus + stream parser),
ui (8, overlay dispatch without GTK). `make phase2-eval` scores the
deterministic pipeline against 32 scripted cases. There is no Rust
integration test of the running daemon.

### Where to start reading

1. `crates/sunoto-core/src/lib.rs` for the state machine and its tests.
2. `crates/sunoto-ipc/src/lib.rs` for the protocols.
3. `apps/daemon/src/daemon.rs`: `run`, then the `DaemonEvent::Hotkey`,
   `Audio`, and `Sidecar(Final)` arms.
4. `services/asr/nemotron_sidecar.py::SidecarServer` for the Python side of
   the protocol, which every backend reuses.
5. `docs/macos-recurring-issues.md` before touching anything on macOS.

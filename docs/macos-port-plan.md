# Sunoto macOS Port Plan

> **ASR superseded.** The macOS real-ASR path described below used the NeMo
> Nemotron model on CPU/MPS. It has since moved to **Parakeet-MLX on Apple
> GPU/Metal** (`parakeet_mlx_streaming` is the config-init default) — see
> `docs/parakeet-mlx-migration-plan.md` and `docs/desktop-configuration.md`.
> The CoreAudio, CGEventTap, CGEvent insertion, NSPasteboard, and launchd
> infrastructure in this plan remains accurate and is the source of truth for
> those platform adapters.

Goal: bring the local-first voice dictation daemon to macOS, reusing the
existing pipeline. The macOS real-ASR path is now **Parakeet-MLX streaming on
Apple GPU/Metal** (`backend = "parakeet_mlx_streaming"`, default
`profile_ms=560`). The original NeMo whole-utterance CPU path
(`backend = "nemotron_offline"`, `asr_device = "cpu"`) remains available as a
legacy fallback; CPU streaming (`backend = "nemotron"`) proved too slow on
this Mac.

This is a port, not a rewrite. The Linux path stays the source of truth; macOS
is added behind the same interfaces, gated by `cfg(target_os = "macos")`.

## 1. ASR decision

> The ASR backend decision below is historical (NeMo Nemotron). The current
> macOS default is Parakeet-MLX streaming — see the header note and
> `docs/parakeet-mlx-migration-plan.md`.

- **Model:** `nvidia/nemotron-speech-streaming-en-0.6b` — the original NeMo
  path (legacy).
- **Legacy default:** `backend = "nemotron_offline"` with `asr_device = "cpu"`
  on macOS (superseded by `parakeet_mlx_streaming`).
- **Experimental:** `backend = "nemotron"` with `asr_device = "cpu"` is wired
  but not recommended; live testing showed severe backpressure.
- **Comparison only:** `asr_device = "mps"` may be tested with the offline
  backend, but it must beat CPU in live short-utterance daemon logs before
  becoming a default.

### Why not CPU streaming

The Linux path is fast because `nemotron_sidecar.py` performs cache-aware RNNT
inference on CUDA while audio is still arriving. On this Mac, the same PyTorch
streaming path on CPU could not keep up: a 26.6 s hold delivered only 4.6 s of
audio to the sidecar and still waited 7.38 s after release. That means the
streaming sidecar backpressured the daemon instead of hiding work during
recording.

The offline path processes the entire utterance after key release. It is not
sub-second, but it remains stable enough for current macOS use.

### What we reuse vs replace

| Component | Linux streaming | macOS offline CPU |
| --- | --- | --- |
| Model + weights | Nemotron 0.6b | **same** |
| `SidecarServer` NDJSON protocol layer | yes | **reused as-is** |
| `NemotronEngine` streaming engine | yes | **available but experimental** |
| `OfflineNemotronEngine` | no | **default macOS engine** |
| `conformer_stream_step` / incremental mel / cache state | yes | **not used by default** |
| Daemon / IPC / polish / insertion / UI | yes | ported per phases below |

### Protocol behavior on macOS

With `backend = "nemotron_offline"`, the macOS sidecar emits:
- `ready` on startup (after warm load),
- `session_started` on `start_session`,
- no partials while recording,
- one `final` on `finish_session`,
- `error` on failure.

`profile_ms` selects the same 80/160/560/1120 ms streaming profiles as Linux.
The offline backend still accepts `profile_ms` for config compatibility but
ignores it.

### Warm start

The model loads once at sidecar startup and stays resident (warm), preserving
the core "hotkey never waits for a model load" property. Warm-load time is a
startup cost, not dictation latency. The daemon's existing sidecar-restart
backoff and "ASR still loading..." bubble cover this unchanged.

## 2. Platform mapping

| Concern | Linux today | macOS target |
| --- | --- | --- |
| Global hotkey | `XGrabKey` (X11) / compositor socket (Wayland) | **CGEventTap** (Quartz); needs Accessibility + Input Monitoring TCC |
| Mic capture | `parec`/`pactl` (PulseAudio) | **CoreAudio** (`AVAudioEngine` or `AudioObject` IO proc), 16 kHz mono s16le |
| Insertion (typing) | XTEST key events | **CGEvent** key events + `CGEventSetUnicodeString` per char |
| Clipboard | X11 selection ownership | **NSPasteboard** (or `pbcopy`/`pbpaste`) |
| Focus / app id | `WM_CLASS` / `hyprctl activewindow` | `NSWorkspace.frontmostApplication.bundleIdentifier` + `AXUIElement` for focus token |
| Overlay UI | GTK4 pill (layer-shell/EWMH) + X11 bubble | Native **NSPanel** pill driven by the same `OverlayRequest` NDJSON protocol |
| ASR | NeMo RNNT streaming on CUDA | NeMo RNNT streaming on **CPU** (`nemotron`), with `nemotron_offline` CPU/MPS fallback |
| Service | systemd `--user` unit | **launchd** `LaunchAgent` plist |
| Install | `install.sh` (apt/systemd) | `install-macos.sh` + TCC permission guidance |

Portable and untouched: `sunoto-core` (session state machine + preroll),
`sunoto-ipc` (NDJSON protocol + sidecar process mgmt), `sunoto-polish` (text
transforms), and the `SidecarServer` protocol layer.

## 3. Code architecture

Mirror the existing X11/Wayland split:

- New crate **`crates/sunoto-macos`** (sibling of `sunoto-linux`), with a
  `macos` module exposing the same surface the daemon already expects:
  `HotkeyListener`, `Shortcut`, and a `MacosUiAdapter` providing
  `capture_focus`, `show_bubble`/`hide_bubble`, `insert`, `set_clipboard`,
  `window_class`, plus `InsertionOutcome` reuse.
- `apps/daemon/Cargo.toml`: depend on `sunoto-linux` under
  `cfg(target_os = "linux")` and `sunoto-macos` under
  `cfg(target_os = "macos")`.
- `daemon.rs`: add `DesktopBackend::Macos` and a `UiBackend::Macos` arm.
  `desktop_backend()` returns `Macos` when `cfg!(target_os = "macos")`.
- `sunoto-audio`: add a macOS CoreAudio capture branch behind
  `cfg(target_os = "macos")`, reusing `CaptureConfig`/`AudioEvent` unchanged.
- `settings.rs`: add `"macos"` to `overlay_backend` validation; macOS config
  path `~/Library/Application Support/sunoto/config.json`; socket under
  `$TMPDIR/sunoto-daemon.sock`; `repo_root()` already env-driven, fine.

The daemon's single-event-loop architecture, the bounded-channel overlay writer
thread, and the "UI must never block the latency path" rule all carry over
unchanged.

## 4. Phased plan

### Phase 0 — ASR feasibility (gate; CPU/MPS only, no GPU)

Do this first; its results set latency expectations for the whole port.

1. Create an isolated macOS venv (`.venv-nemotron-mac`): CPU/MPS PyTorch + NeMo
   (no CUDA pins). New `services/asr/setup_macos_runtime.sh`.
2. Load `nvidia/nemotron-speech-streaming-en-0.6b` with `device="cpu"` and
   `device="mps"`. Probe `model.transcribe()` on short real-speech clips.
3. **Measure:** warm-load time, release-to-final latency, RTF, and WER on
   real speech downloaded from Hugging Face.
4. If MPS hits an unsupported load/warm-up op, record the failure and compare
   against CPU. The offline sidecar falls back to CPU on MPS load failure.
5. Go/no-go: if whole-utterance CPU is acceptable for the first macOS build,
   proceed and track sub-second release latency as a follow-up optimization.
   If quality or latency is unacceptable, the fallback is Parakeet-TDT-0.6b-v2
   offline — same offline sidecar shape, different `--model` + TDT decoding
   config. This is the only branch point.

Deliverable: `docs/macos-phase-0-results.md` with the measured numbers and the
chosen device default.

### Phase 1 — Build skeleton (compiles on macOS, no behavior)

1. Add `crates/sunoto-macos` with `Cargo.toml` (macOS-gated FFI deps for
   CoreGraphics/CoreAudio/AppKit/Foundation, matching the hand-written FFI
   style of `crates/sunoto-linux/src/x11/ffi.rs`).
2. cfg-gate `sunoto-linux`/`sunoto-macos` in `apps/daemon/Cargo.toml`.
3. Add `DesktopBackend::Macos` + `UiBackend::Macos(MacosUiAdapter)` with stub
   methods returning "not implemented" so `cargo build -p sunoto-daemon`
   succeeds on macOS.
4. `cargo test --workspace` and `cargo clippy -- -D warnings` green on macOS
   (guard any X11-only tests with `cfg`).

### Phase 2 — Audio capture (CoreAudio)

- Implement CoreAudio input producing 16 kHz mono s16le frames as
  `AudioEvent::Frame`, feeding `sunoto-core` preroll unchanged.
- Platform branch in `sunoto-audio`; keep the Linux `parec`/`pactl` path under
  `cfg(target_os = "linux")`.
- First capture triggers the TCC Microphone prompt; log a clear error if
  denied (the daemon's existing `AudioEvent::Stopped` + restart backoff
  handles denial gracefully).

### Phase 3 — Hotkey (CGEventTap)

- `HotkeyListener::open` installs a system-wide `CGEventTap`
  (`kCGSessionEventTap`, head-insert, listen-only) filtered to the configured
  shortcut. Map `Shortcut` modifiers to `CGEventFlags` and key names to key
  codes.
- Add a `Cmd` modifier to `Shortcut::parse` (macOS convention) while keeping
  `Ctrl` working; default macOS shortcut will be `Cmd+F1` (config-overridable).
- If the tap cannot be created (Accessibility/Input Monitoring not granted),
  log actionable guidance; the daemon stays up and retries.

### Phase 4 — Insertion + focus + clipboard

- Insertion via `CGEventCreateKeyboardEvent` + `CGEventSetUnicodeString` per
  char, posted at `kCGHIDEventTap`. Map newlines to Return only when
  `allow_enter_and_tab` (reuse `sanitize_for_insertion`).
- Focus token + app id via `NSWorkspace.frontmostApplication` +
  `AXUIElementCopyAttributeValue`; revalidate focus between release and insert
  (mirrors X11/Wayland `ClipboardOnly` behavior on focus change).
- Clipboard via `NSPasteboard` (`pbcopy` fallback).
- Neutralize held modifiers around injection, as the Linux path does.

### Phase 5 — ASR sidecars

- Experimental macOS CPU streaming path uses existing `services/asr/nemotron_sidecar.py`:
  - `settings.rs` passes `asr_device` through to `--device`,
  - macOS prefers `.venv-nemotron-mac/bin/python` for `backend = "nemotron"`,
  - not recommended as default because live testing showed severe
    backpressure.
- Default macOS `services/asr/nemotron_offline_sidecar.py`:
  - reuses `SidecarServer` from `nemotron_sidecar.py` (extract/share it if
    needed),
  - new `OfflineNemotronEngine`: `start` clears a `_SampleBuffer`; `accept_audio`
    appends; `finish` pads to a frame boundary and calls `model.transcribe()`
    once, returning the text; `cancel` drops the buffer,
  - `--device cpu|mps`, `--model` default Nemotron, warm-up on startup with a
    short zero clip, CPU default with explicit MPS override,
  - no VRAM preflight, no `cuda.synchronize`.
- `settings.rs`: accepts `"nemotron"` (streaming), `"nemotron_offline"`
  (whole-utterance fallback), and `"mock"`. `validate()` accepts `cpu`, `mps`,
  and `cuda` for streaming; offline accepts `cpu` or `mps`.
- `profile_ms` accepted but ignored by the offline engine.

### Phase 6 — Overlay UI (native NSPanel pill)

- New tiny native overlay driven by the same `OverlayRequest` NDJSON protocol
  (Show/Hide/Recording/Status/Segment/Clear/Shutdown). Two options:
  1. **(recommended)** A small Swift `sunoto-overlay` binary: borderless
     `NSPanel` (`.floating` level, non-activating), top-center, with dot +
     `NSLevelIndicator` meter + status label. Spawned by the daemon exactly
     like the GTK `ui_sidecar.py` sidecar; fed via stdin NDJSON.
  2. (low-effort fallback) Run the existing GTK4 overlay on macOS in a
     borderless top-center window (GTK4 builds via Homebrew), with the
     layer-shell/EWMH code paths inert. Faster to first light, heavier dep.
- The daemon's overlay-failure-is-non-fatal behavior carries over: if the
  overlay dies, dictation continues.

### Phase 7 — Service + install

- `services/macos/com.earendil-works.sunoto.plist` (launchd `LaunchAgent`):
  `RunAtLoad`, `KeepAlive`, `ProgramArguments` = `sunoto-daemon run`, env
  `SUNOTO_ROOT`, standard out/err to `~/Library/Logs/sunoto/`.
- `install-macos.sh`: preflight (cargo, python3), `cargo build --release`,
  `config init` (macOS path), install plist to `~/Library/LaunchAgents/`,
  `launchctl load`, and print the **three TCC prompts** the user must grant:
  Accessibility, Input Monitoring, Microphone. (No GPU/Xid concerns apply.)
- Update `settings.rs` macOS paths; `validate()` accepts `overlay_backend =
  "macos"`.

### Phase 8 — Verification + docs

- `cargo test --workspace` + `cargo clippy -- -D warnings` on macOS.
- `sunoto-daemon check` macOS path: event tap + CoreAudio + sidecar protocol
  (probe `nemotron_offline_sidecar.py` health).
- `sunoto-daemon selftest` macOS path: insert "sunoto phase one" into focused
  TextEdit, clipboard round-trip, focus/bundle-id lookup.
- Update `README.md`, `AGENTS.md`, `install.sh` (cross-link), and
  `docs/desktop-configuration.md` with the macOS section; note the offline/
  no-partials behavior and TCC permissions.

## 5. Sequencing and first step

Recommended order to de-risk: **Phase 0 → Phase 1 → Phase 5 (offline sidecar)
→ Phase 2 + Phase 4 → Phase 3 → Phase 6 → Phase 7 → Phase 8.**

Rationale: Phase 0 confirms the ASR latency assumption that the whole port
rests on; Phase 1 makes the tree build on macOS; Phase 5 plus a synthetic
hotkey (the existing `sunoto-daemon trigger press|release` control socket) plus
Phase 2 audio gives an end-to-end "hold key → talk → text" loop on macOS using
the real model *before* any CGEventTap/insertion UI work, which is the fastest
way to validate the core pipeline on Apple Silicon.

## 6. Open questions to resolve during Phase 0

- Whether a future PyTorch/NeMo MPS release beats the CPU real-speech path.
- Whether a streaming or ONNX/quantized backend can bring macOS
  release-to-insertion below one second while keeping local inference.
- Whether to default the macOS hotkey to `Cmd+F1` (cleaner) or keep `Ctrl+F1`
  for config parity with Linux.
- Overlay choice: native Swift `NSPanel` vs GTK4-on-macOS (decide in Phase 6
  based on dep-weight preference; AGENTS.md says keep deps minimal → leans
  native).

## 7. Progress

- **Phase 1 (build skeleton): DONE (verified on macOS).** Installed Rust
  1.96 on this Mac. `sunoto-linux` now compiles on all targets: its X11
  implementation moved to `crates/sunoto-linux/src/x11/linux.rs` (Linux-only)
  with a compile stub in `x11/stub.rs` for other targets; `x11/mod.rs` is a
  cfg dispatcher. New facade crate `crates/sunoto-desktop` re-exports
  `sunoto_linux::x11::*` (will switch its macOS arm to `sunoto-macos` in
  Phase 2). New placeholder crate `crates/sunoto-macos` (real CGEventTap /
  CoreAudio / CGEvent-insertion / NSPasteboard impls land in Phase 2-4).
  `apps/daemon` imports all platform adapters from `sunoto_desktop` instead
  of `sunoto_linux::x11` directly. `daemon.rs` adds `DesktopBackend::Macos`
  (detected via `cfg!(target_os="macos")`) and a `UiBackend::Macos` arm that
  currently routes through the stub adapter (real adapter in Phase 2-4).
  `cargo build --workspace`, `cargo clippy --workspace --all-targets
  -- -D warnings`, and `cargo test --workspace` (63 tests) all pass on macOS.
  Also fixed two pre-existing `function_casts_as_integer` warnings in the
  signal-handler wiring so clippy stays green on rustc 1.96.
- **Phase 5 (ASR offline sidecar): DONE (protocol/buffer + Rust wiring,
  verified).** `services/asr/nemotron_offline_sidecar.py` reuses
  `SidecarServer`/`serve`/`log` from `nemotron_sidecar.py`; new
  `_OfflineBufferEngine` buffers i16 samples and writes a 16 kHz mono s16le
  temp WAV on `finish_session`, then `OfflineNemotronEngine._transcribe_wav`
  calls `model.transcribe()` once. CPU default with explicit MPS override; no
  partials; `profile_ms` accepted but
  ignored; warm-up transcribe on startup. Python tests in
  `tests/phase1/test_nemotron_offline_sidecar.py` (14 cases) pass.
  Rust side: `settings.rs` accepts `backend = "nemotron_offline"` (selects
  `nemotron_offline_sidecar.py`, prefers `.venv-nemotron-mac/bin/python`) and
  `overlay_backend = "macos"`; `validate()` updated; new unit tests cover
  both. Daemon `config init`/`show` round-trips `nemotron_offline` on macOS.
- **Phase 5 update (macOS streaming CPU): DONE but experimental.**
  `backend = "nemotron"` now passes `asr_device` through to
  `nemotron_sidecar.py`, and macOS uses `.venv-nemotron-mac/bin/python` for
  that streaming backend. Live testing showed it is too slow on this Mac, so
  recommended macOS real-ASR config remains `backend = "nemotron_offline"`,
  `asr_device = "cpu"`.
- **Phase 0 (ASR measurement): DONE — offline benchmark superseded as the
  defaulting signal.** Set up `.venv-nemotron-mac`
  (Python 3.12, `torch==2.7.1` MPS wheel, NeMo pinned to the Linux commit)
  via `services/asr/setup_macos_runtime.sh`. The first tone-only MPS probe
  proved the model could execute. Later live logs showed the old MPS path was
  slower for short dictation despite later offline wrapper cleanup. On the
  matched 5-clip Hugging Face LibriSpeech offline benchmark, MPS measured
  1.015s p50 / 2.123s p95 while CPU measured about 1.53–1.59s p50 /
  2.22–2.25s p95, with the same 0.0265 mean WER, but live short-utterance
  CPU logs are the stronger product signal. Results and interpretation are in
  `docs/macos-phase-0-results.md`.
- **Phases 2, 3, 4 (CoreAudio, CGEventTap, CGEvent insertion): DONE (compile
  + selftest-verified on macOS).** `crates/sunoto-macos` now holds real FFI:
  `ffi.rs` (CoreGraphics/CoreFoundation/ApplicationServices hand bindings),
  `hotkey.rs` (system-wide `CGEventTap` on a dedicated CFRunLoop, `Cmd`/`Ctrl`
  modifiers, F-key codes), `insertion.rs` (`UiAdapter` with
  `CGEventCreateKeyboardEvent` + `CGEventKeyboardSetUnicodeString` per-char,
  `pbcopy`/`pbpaste` clipboard, `CGWindowListCopyWindowInfo` focus/app name).
  `crates/sunoto-audio/src/macos.rs` adds CoreAudio capture (default input
  device, `AudioConverter` to 16 kHz mono s16le, IOProcID API) with the
  Linux `parec` path gated to `cfg(not(target_os="macos"))`. The
  `sunoto-desktop` facade now re-exports `sunoto-macos` on macOS. Runtime
  selftest on this Mac: focused-cursor insertion **passed** (real CGEvent
  posting), clipboard round-trip **passed**, window-class lookup **passed**,
  push-to-talk **passed** (Accessibility already granted here).
- **Phase 7 (launchd + install-macos.sh): DONE (assets).**
  `services/macos/com.earendil-works.sunoto.plist` + `install-macos.sh`
  (build, config at `~/Library/Application Support/sunoto`, plist render,
  `launchctl load`, TCC guidance). macOS config path added to `settings.rs`.
- **Phase 8 (docs): DONE.** README macOS section added.
- **Phase 6 (overlay): stubbed.** `UiAdapter` bubble methods are no-ops; the
  daemon runs without a visual overlay (overlay-failure is non-fatal). A
  native NSPanel pill is the remaining piece.
- **`verify-all.sh` exit 0**: 33 objective checks pass, 0 fail, 5 manual
  (live TCC/GUI) checks remain for the user to walk through.
- **Remaining for a working app on Mac:** grant Accessibility/Input
  Monitoring/Microphone TCC, `bash install-macos.sh`, then hold the hotkey,
  speak, release → text at the cursor via offline `nemotron_offline` on CPU. The
  native NSPanel overlay (Phase 6) is optional polish.

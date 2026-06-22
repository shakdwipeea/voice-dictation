# Agent guidance for voice-dictation (sunoto)

This repository builds a local, system-wide voice dictation daemon for Linux,
with an active macOS port. A Rust daemon owns capture/hotkey/insertion; Python
sidecars provide ASR (NeMo Nemotron), and platform UI helpers provide the
overlay. Keep changes small, practical, and verified against the real daemon
flow where possible.

## Project workflow

- Rust: `cargo test --workspace --offline` and
  `cargo clippy --workspace --offline --all-targets -- -D warnings` must both
  pass. `make test` runs everything (Rust + all Python suites).
- Python suites live in `tests/phase1`, `tests/phase2`, `tests/ui` (stdlib
  `unittest`; no uv/pytest). The overlay protocol tests run without GTK.
- Quick verification commands:
  - `target/debug/sunoto-daemon check` — X11/XTEST/shortcut/sidecar protocol
  - `scripts/macos-port/verify-phase.sh <N>` — objective macOS phase checks
  - `make ui-demo` — drive the overlay pill by hand
  - `journalctl --user -u voice-dictation.service -n 80 --no-pager`
- After daemon, sidecar, overlay, or unit changes, restart the user service
  before judging behavior: `systemctl --user restart voice-dictation.service`
  (Linux). On macOS, see "macOS operations" below — restart is NOT
  `systemctl`; use the commands there.

## macOS operations

The macOS port has repeatedly bitten us with "Ctrl+F1 stopped working"
and "nothing gets pasted." The detailed symptoms, root causes, and verified
fixes for every issue that has recurred more than once live in
**[docs/macos-recurring-issues.md](docs/macos-recurring-issues.md)** — read
that file BEFORE changing code when a macOS symptom matches.

The single most frequent issue is the **inert CGEventTap** (Ctrl+F1 does
nothing): it is a TCC/permission + adhoc-code-signature problem. The fix is
two-part: run the **bare binary** `target/release/sunoto-daemon` (not the
`Sunoto.app` bundle — its path-based TCC grant survives rebuilds), AND launch
it **from a terminal** (`nohup ... &`), not via launchd (launchd's no-responsible-
process context makes TCC disable the tap). Full detail in recurring-issues §1.

### How to check logs on macOS

The daemon logs to stderr (timestamped `[HH:MM:SS.msZ] [info/warn/error]`).
Where it lands depends on how the daemon was launched:

| Launch method | Log location |
| --- | --- |
| launchd (`install-macos.sh`) | `~/Library/Logs/sunoto/daemon.log` |
| `nohup ... > FILE 2>&1 &` | that file (we use `/tmp/sunoto-daemon.log`) |
| foreground in a terminal | the terminal |

Watch live with `tail -f`. Key per-session lines: `session N: recording`,
`session N: sent to ASR: ...ms audio, ... samples, rms=R, peak=P, timeout Tms`,
`session N: final transcript: "..."`, and the `inserted via ...; timing
breakdown:` line. `rms` near 0 / very low means the mic source is wrong or
muted; check System Settings → Sound → Input.

### Restarting the macOS daemon

- **Working launch (use this for actual dictation):** run the **bare binary**
  from a terminal, NOT via launchd —
  `nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 &`.
  launchd agents have no responsible process, so TCC disables the event tap
  (`tap disabled by system`); a terminal-launched process inherits the GUI/TCC
  context and the tap stays enabled. See recurring-issues §1.
- launchd (auto-start only; hotkey will be inert):
  `launchctl bootout gui/$(id -u)/com.earendil-works.sunoto` then
  `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.earendil-works.sunoto.plist`
  (or `launchctl kickstart -k gui/$(id -u)/com.earendil-works.sunoto`).
- **Never have two daemons running at once** — they fight over the control
  socket (`/var/folders/.../sunoto-antash-daemon.sock`) and the CGEventTap.
  Before starting one, kill the other and check `ps aux | grep sunoto-daemon`.
- After a restart, **wait for `ASR sidecar ready`** (~16–32s warmup) before
  judging hotkey behavior — see recurring-issues §6.

### macOS diagnosis quick-reference (full detail in the recurring-issues doc)

- **Ctrl+F1 does nothing** → recurring-issues §1 (inert tap / TCC). First,
  prove the daemon is healthy with a synthetic press over the control socket;
  if that records, it's the tap. Run the bare binary; grant Input Monitoring +
  Accessibility to `target/release/sunoto-daemon`. Do NOT trust
  ʻsunoto-daemon checkʼ alone to rule this out.
- **launchd crash-loop** (`daemon starting` every ~10s) → §2. Never
  reintroduce `CFRunLoopStop` in the hotkey `Drop`.
- **Empty transcript every time** → §3. The offline engine must invert the
  float32 contract (`round(s*32768)`, clamped); guarded by
  `test_accept_audio_converts_sidecar_float32_to_i16`.
- **Transcribes but nothing appears** → §4. Insertion MUST paste first
  (`pbcopy` + Cmd+V), fallback to typing; focus target must be the real app,
  not `Window Server`.
- **Config silently reverts / wrong log file** → §5. Confirm the real log
  (`~/Library/Logs/sunoto/daemon.log` for launchd) and the running config.

### ASR latency and the transcribe timeout

- Prefer `backend = "parakeet_mlx_offline"` on macOS. It uses
  `mlx-community/parakeet-tdt-0.6b-v3` through parakeet-mlx/MLX on Apple GPU
  (`Device(gpu, 0)` on the M1 Pro benchmark): cached load ~1.35s, warmup
  ~1.22s, p50 ASR latency ~0.24s on the LibriSpeech sample set. The daemon
  sidecar hot path avoids temp WAV + ffmpeg by feeding captured PCM directly
  into `get_logmel()` + `model.generate()`. The older macOS whole-utterance
  Nemotron backend's latency is dominated by
  `model.transcribe` (RNNT decoding of real speech), NOT the daemon. The
  streaming CPU path (`backend = "nemotron"`, `asr_device = "cpu"`) is
  selectable but live testing showed ~7.4s turnaround — it cannot keep up.
- The transcribe watchdog is ADAPTIVE: `final_timeout_ms + recorded_ms *
  final_timeout_rtf` (defaults 8000ms + 3.0x). A flat timeout will time out
  long utterances and lose transcripts. The offline `finish()` is a blocking
  call — `cancel_session` cannot interrupt a running `model.transcribe`, so
  a timed-out session still runs to completion on the sidecar.
- `asr_device` selects the Nemotron Torch device. Streaming `nemotron`
  defaults to CUDA when unset and is intended for Linux/CUDA. Offline
  `nemotron_offline` accepts `"cpu"` or `"mps"`; use CPU if you fall back to
  Nemotron on macOS. Parakeet-MLX backends ignore `asr_device`; leave it unset
  and optionally set `asr_model` to override the default checkpoint.
  `parakeet_mlx_streaming` exists for experimental partials but is not the
  default. It streams partials via `transcribe_stream()` and, by default, uses a
  direct full-utterance PCM `get_logmel()` + `model.generate()` final to recover
  offline-like accuracy; protocol smoke recovered `Quilter` where pure streaming
  had `Coulter`. Switch backend/model in config and restart — no rebuild needed.

## Runtime safety

- Be careful with GPU-heavy verification. Use `bash bin/gpu-status.sh` before
  GPU-loading work if there is any concern about prior Xid events. The
  Nemotron sidecar takes 16–32 s to warm and holds ~3.6 GiB VRAM.
- NEVER start a second Nemotron instance (probe script, benchmark, manual
  sidecar) while voice-dictation.service is running — two instances plus the
  desktop can exhaust the 12 GiB card and crash the session. Stop the
  service first, or use the mock backend. The sidecar's VRAM preflight
  (`--min-free-vram-mib`, default 4500) refuses to load into a nearly-full
  GPU; do not disable it to "make a test work".
- On macOS, prefer `backend = "parakeet_mlx_offline"` with `asr_device` unset.
  Do not run the macOS benchmark while any ASR sidecar is already warm.
- Do not run stress tests such as gpu-burn or FurMark from this repo.
- The daemon types into whatever window is focused. Never trigger a dictation
  session (real or synthetic Ctrl+F8) without first focusing a disposable
  target window; treat injection as a security surface (see the product
  plan's injection-safety section).
- The recording overlay stays minimal while recording (dot + meter only);
  status text appears only for transcribing/partial/error states.

## Audio debugging

- The daemon logs per-session audio stats (duration, rms, peak). rms near 0
  means the source is wrong or muted — check `pactl list short sources`.
  Source auto-selection rejects monitor (speaker loopback) sources on purpose.
- Empty transcripts from the real backend usually mean near-silent input,
  not an ASR bug; the daemon logs the audio summary alongside.

## Style

- Match the existing architecture: one event loop in `apps/daemon`, typed
  NDJSON sidecar protocols in `sunoto-ipc`, platform specifics in
  `sunoto-linux` / `sunoto-macos`, pure text transforms in `sunoto-polish`.
- UI must never block the latency path — overlay writes go through the
  bounded-channel writer thread; keep it that way.
- Avoid adding broad abstractions for one-off fixes; keep dependencies
  minimal. If a new runtime dependency is useful, update `pyproject.toml` /
  `Cargo.toml`, `install.sh`, and `README.md` together.

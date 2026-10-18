# Agent guidance for voice-dictation (sunoto)

This repository builds a local, system-wide voice dictation daemon for Linux,
with an active macOS port. A Rust daemon owns capture/hotkey/insertion; Python
sidecars provide ASR (NeMo Nemotron), and platform UI helpers provide the
overlay. Keep changes small, practical, and verified against the real daemon
flow where possible.

## Architecture

```
              [hotkey: Right Option on macOS / Ctrl+F1 on Linux]
                                     |
                                     v
   +-------------------------------------------------------------+
   |  sunoto-daemon (Rust single event loop, apps/daemon)        |
   |                                                             |
   |  capture  -> ring buffer (PCM f32)                          |
   |     |          |                                            |
   |     |          |   on release: flush -> ASR sidecar          |
   |     |          v                                            |
   |     |   +-------------------------+   partials (NDJSON)       |
   |     |   | ASR sidecar (Python)    | ----------------------+   |
   |     |   |  - parakeet_mlx_* (macOS)|                      |   |
   |     |   |  - nemotron / _offline  |                      v   |
   |     |   |     (Linux/CUDA)        |              +-----------+ |
   |     |   +-------------------------+              | overlay   | |
   |     |             | final transcript (NDJSON)     | (pill+    | |
   |     |             v                                | meter,    | |
   |     |   deterministic polish (sunoto-polish)       | bounded   | |
   |     |   resolve_corrections / fillers / normalize  | channel)  | |
   |     |             |                                +-----------+ |
   |     |             v                                              |
   |     |   LLM polish sidecar (Python, llama.cpp) <-(opt-in)         |
   |     |   merges mid-utterance self-corrections ONLY               |
   |     |   keepalive thread warms Metal/CUDA prefill every ~1s       |
   |     |             | polished text                                 |
   |     v             v                                              |
   |  insert: paste first (pbcopy+x, XTEST), fallback to typing       |
   +-------------------------------------------------------------+
                                     |
                                     v
                         focused app receives text
```

Key invariants:
- **One Rust event loop** owns capture, hotkey, sidecar IPC, and insertion.
  Platform specifics live in `sunoto-linux` / `sunoto-macos`; typed NDJSON
  protocols in `sunoto-ipc`; pure text transforms in `sunoto-polish`.
- **Sidecars are Python**, spoken to over stdin/stdout NDJSON. ASR runs
  during recording (streaming partials + a blocking `finish()`). The LLM
  polish sidecar runs only after the final transcript (opt-in, default off).
- **Overlay writes never block the latency path** — they go through a
  bounded-channel writer thread.
- **LLM polish keepalive**: a background thread in the polish sidecar fires a
  tiny prefill+decode ping every ~1s (`SUNOTO_LLM_POLISH_KEEPALIVE_S`, 0=off)
  to keep the GPU prefill path warm during idle/ASR. A shared `threading.Lock`
  serializes all llama.cpp calls (llama.cpp is not thread-safe); the keepalive
  thread uses a non-blocking trylock and skips when a real polish is in
  flight. This defeats the ASR↔LLM GPU cold-ramp on Metal (macOS); on
  Linux/CUDA it is the same code path and stays on harmlessly — disable via
  `llm_polish_keepalive_secs: 0` if profiling shows it is unneeded there.
- **ASR↔LLM GPU note**: on macOS both ASR (MLX) and LLM (Metal) share one
  unified-memory GPU and contend. On Linux the ASR backend is Nemotron/CUDA;
  the LLM uses llama.cpp-CUDA. The keepalive is platform-neutral.

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
  - `tools/ui/render-overlay-frames.sh OUT [GROUP...]` — render the macOS
    floating pill and notch island to PNG frames and contact sheets, with a
    synthetic clock and no permissions (groups `pill-*`, `island-*`,
    `geometry`)
  - `python3 tools/ui/macos_overlay_demo.py [--style pill] [--screenshots DIR]`
    — play a dictation on the real macOS overlay (nothing is typed) and
    screenshot the top of the screen; needs Screen Recording for the terminal
    and an awake display
  - `journalctl --user -u voice-dictation.service -n 80 --no-pager`
- After daemon, sidecar, overlay, or unit changes, restart the user service
  before judging behavior: `systemctl --user restart voice-dictation.service`
  (Linux). On macOS, see "macOS operations" below — restart is NOT
  `systemctl`; use the commands there.

### Desktop configuration reference

The complete per-platform setup (config file paths, Wayland/Hyprland + X11
bindings, macOS launch + permissions, ASR backend selection, profile tuning,
and troubleshooting) lives in
**[docs/desktop-configuration.md](docs/desktop-configuration.md)**. Read it
before editing platform-specific config or when a macOS/Linux setup question
comes up. The quick facts:
- Linux config: `~/.config/sunoto/config.json`; macOS config:
  `~/Library/Application Support/sunoto/config.json`.
- macOS default backend (config init) is `parakeet_mlx_offline` with
  `profile_ms=560` and `overlay_backend="macos"`; Linux config init defaults
  to `mock`.
- macOS runs the bare binary from a terminal (NOT launchd) so the CGEventTap
  stays enabled — see the "Starting the application on macOS" section there.

## macOS operations

### Releases and automatic updates

- Tag `vX.Y.Z` matching `Cargo.toml`. `.github/workflows/release.yml` builds
  the app and signs the zip with minisign: the secret is
  `MINISIGN_SECRET_KEY`, and the public half is
  `packaging/macos/release-signing.pub`, which is compiled into the daemon.
  The trusted comment is `Sunoto X.Y.Z Sunoto-macos-arm64.zip`.
  Without that `.minisig` asset, installed apps will not update to the release.
- Release installs update themselves (`apps/daemon/src/update.rs`, setting
  `auto_update`). They check daily, verify the download, and install after
  10 idle minutes. The installer is a copy of the old binary: it runs the new
  bundle's `setup --update` and puts the old app back on failure. Keep
  `setup --update` working; older installs call newer setups with it.
- Test without publishing: `launchctl setenv SUNOTO_UPDATE_FEED
  file://.../latest.json` (GitHub's latest-release JSON shape),
  `SUNOTO_UPDATE_KEY` (a test public key file), `SUNOTO_UPDATE_IDLE_SECS`.
  Restart the app, and `launchctl unsetenv` them afterwards. Progress goes to
  `~/Library/Logs/sunoto/update.log`.

### Recurring issues

The macOS port has repeatedly bitten us with "Ctrl+F1 stopped working"
and "nothing gets pasted." The detailed symptoms, root causes, and verified
fixes for every issue that has recurred more than once live in
**[docs/macos-recurring-issues.md](docs/macos-recurring-issues.md)** — read
that file BEFORE changing code when a macOS symptom matches.

The single most frequent issue is the **inert CGEventTap** (Ctrl+F1 does
nothing): it is normally a TCC/signing problem. `setup` creates a stable
per-machine signing identity and shows a native onboarding panel that requests
one permission at a time from the daemon's main loop. It relaunches only after
all grants are live and the user clicks Done. Rebuilds signed by that identity
retain their grants. Never launch the installed app through launchd; use its
Login Item/Launch Services context. Full detail is in recurring-issues §1.

### How to check logs on macOS

The daemon logs to stderr (timestamped `[HH:MM:SS.msZ] [info/warn/error]`).
Where it lands depends on how the daemon was launched:

| Launch method | Log location |
| --- | --- |
| `Sunoto.app` login item (`install-macos.sh` / `sunoto-daemon setup`) | `~/Library/Logs/sunoto/daemon.log` |
| `nohup ... > FILE 2>&1 &` | that file (we use `/tmp/sunoto-daemon.log`) |
| foreground in a terminal | the terminal |

`target/release/sunoto-daemon status` asks the running daemon (whichever way
it was launched) for its health over the control socket: hotkey delivery,
microphone, speech model, polish, session state.

Watch live with `tail -f`. Key per-session lines: `session N: recording`,
`session N: sent to ASR: ...ms audio, ... samples, rms=R, peak=P, timeout Tms`,
`session N: final transcript: "..."`, and the `inserted via ...; timing
breakdown:` line. `rms` near 0 / very low means the mic source is wrong or
muted; check System Settings → Sound → Input.

### Restarting the macOS daemon

- **Installed launch (what users run):** `Sunoto.app` in `~/Applications`,
  whose executable is the daemon itself, started by Launch Services as a
  Login Item. Install or reinstall with `bash install-macos.sh` (builds, then
  runs `sunoto-daemon setup`, which waits for the app's own ready report).
  Restart: `pkill -f "Sunoto.app/Contents/MacOS/sunoto-daemon"; open
  ~/Applications/Sunoto.app`. Permissions (Accessibility and Microphone;
  Accessibility also covers the shortcut's event tap, so Input Monitoring is
  not requested) are granted to the one entry `Sunoto.app`; the stable local
  signature keeps those grants across rebuilds and reinstalls.
- **Development launch:** run the **bare binary** from a terminal, NOT via
  launchd — `nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log
  2>&1 &`. launchd agents have no responsible process, so TCC disables the
  event tap; a terminal-launched process inherits the GUI/TCC context. Never
  run this while `Sunoto.app` is running.
- **Never have two daemons running at once** — they fight over the control
  socket (`/var/folders/.../sunoto-antash-daemon.sock`) and the CGEventTap.
  Before starting one, kill the other and check `ps aux | grep sunoto-daemon`.
- After a restart, **wait for `ASR sidecar ready`** (~16–32s warmup) before
  judging hotkey behavior — see recurring-issues §6.

### macOS diagnosis quick-reference (full detail in the recurring-issues doc)

- **The shortcut does nothing** (Right Option; Ctrl+F1 on older configs) →
  recurring-issues §1 (inert tap / TCC). First,
  inspect `sunoto-daemon status` and the onboarding panel. If a synthetic
  control-socket press records, the audio/ASR pipeline is healthy and only the
  physical-keyboard event tap is blocked.
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
  and optionally set `asr_model` to override the default checkpoint. By
  default the app loads setup's bf16 copy from
  `~/Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16`
  (identical weights, half the download) and falls back to the Hugging Face
  original when that folder is missing.
  `parakeet_mlx_offline` is the **default macOS backend** (config init).
  A dictation under 10 s gets one whole-utterance pass on release. A longer
  one is cut at pauses (forced at the quietest moment by 20 s) and each piece
  is transcribed on a background thread while you keep talking; pieces
  re-hear 3 s before their cut and are stitched with parakeet-mlx's own
  overlap merge. Memory stays bounded however long the dictation runs, and
  release-to-text no longer grows with length (a 66 s whole pass took 2.5 s).
  The Apple GPU clocks down after ~2 s idle and the first decode after that
  pays a fixed ~0.6 s; neither `mx.set_wired_limit` nor a light keepalive
  (tiny matmul or 0.5 s silence pass every 2 s) keeps it up. So the sidecar
  also decodes the open audio 200 ms into every pause after speech ("pause
  decode"); a tap with nothing said since reuses it, and the GPU wake-up
  happens before the tap. The last 200 ms before the tap (the key click) is
  ignored. A/B on 8 real-time-paced dictations (4.8–74 s, 0.4–1 s pause
  before the tap): tap-to-text median 571 → 3 ms, worst 974 → 265 ms, same
  text and memory, GPU decode time +36% (~16% duty while recording).
  Benchmark with idle gaps between sessions; back-to-back runs keep the GPU
  warm and hide all of this. Tune with the sidecar's
  `--segment-min-s/--segment-max-s/--segment-overlap-s`
  (`--segment-min-s 0` = one pass on release) and `--no-pause-decode`.
  `parakeet_mlx_streaming` is still selectable: it streams partials via
  `transcribe_stream()` (never shown) and re-decodes the whole buffer on
  release, so its wait and memory spike grow with dictation length.
  Switch backend/model in config and restart — no rebuild needed.

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
- Word list (`word_list_enabled`, macOS + `parakeet_mlx_offline`): at each
  press a `word-list` thread (`apps/daemon/src/vocabulary.rs`) gathers
  unusual words from the focused window (Accessibility: text, title,
  `AXDocument`, and a bounded walk of the window when the box is nearly
  empty), its project, project folders and apps (opt-in: Contacts,
  Documents/recent files), and sends them as `session_words`;
  `services/asr/word_boost.py` favours them in the greedy TDT decode, and
  the daemon fixes misses after the final (`fix_near_misses`: letters,
  sound, joins of up to 4 words). Never log the words themselves (screen
  text). "Unusual" = not in the system's everyday vocabulary
  (NaturalLanguage, which has no proper names) nor MODERN_WORDS; the fix
  also spares exact `/usr/share/dict/words` entries ("William"),
  contractions and possessives. Never read under Documents/Desktop/
  Downloads/iCloud without `word_list_documents`: it raises a macOS dialog.
  Guards that came from benchmark failures: no first-piece bonus for
  one-letter pieces, a start bonus never beats blank or a longer start, a
  quarter-strength first piece (noise), <unk> output is decoded again (MLX
  emits runs of <unk> now and then when several MLX processes share the GPU,
  with or without a list). Change bonuses only with `tools/vocab/` (app
  stopped; never run two GPU benchmarks at once, see <unk> above).
  2026-10-18: LibriSpeech rare-word error 9.8%→5.3% (clean), 18.3%→10.0%
  (other), common words unchanged; personas 207→385 of 536 names (404 with
  both opt-ins); ordinary speech unchanged; decode time +0.3–1.4%, MLX peak
  unchanged, daemon 7.8→14 MB.
- Qwen3-ASR-0.6B (context in its prompt, mlx-audio) was benchmarked against
  Parakeet on 2026-10-18 and not adopted: better with the screen text itself
  (433 vs 385 persona names) and in loud background talk, but 2–3× slower
  (≈360 vs 108 ms per clip with screen text), worse on LibriSpeech rare
  words and harder speech, 11 languages vs 25, and with a list in its prompt
  it writes the list out on silence. Rerun with `--engine qwen3-8bit` on the
  tools/vocab benchmarks (venv: build/venv-qwen3-asr) before revisiting.
- The pill never shows partial transcripts: they are often wrong where the
  final is right. While recording it shows the dot, sound ribbon, timer, and
  a start hint that fades once you start talking (3 s at most). After the
  second tap it shows words only if the wait passes 1.5 s, and a check mark
  confirms the paste.
- On a Mac with a notch the pill is an island grown out of the notch
  (`IslandView`, `overlay_style: "auto"`, the default; design A of
  `docs/notch-island-designs.html`): the notch grows a short pill under the
  camera with the dot, the sound and the timer in one row, words in a second
  row, and after a paste a check in the middle under the camera before it
  pulls back in. Loading shows the ink dot and a glinting line with the words
  below. Every size change is one spring; parts only fade in place.
  Elsewhere, or with `overlay_style: "pill"`, it floats under the menu bar
  (`PillView`). Both draw from one `PillModel`; the surface is chosen only
  when the pill opens from hidden, never mid-dictation. The island's window
  is fixed-size at level `.mainMenu + 3` and never resizes.

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

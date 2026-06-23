# voice-dictation (sunoto)

System-wide, local-first voice dictation for Linux. Hold a hotkey, speak,
release — clean text lands at the cursor of whatever application is focused.
No cloud, no API keys.

Rust daemon + always-warm streaming ASR (NVIDIA Nemotron Speech Streaming
0.6B via NeMo) + deterministic text polish + GTK4 overlay. Built for speed:
**release-to-insertion is 151 ms p95** measured on an RTX 3060 (target was
600 ms).

X11 is supported directly. Hyprland/Wayland is supported through compositor
bindings that call the daemon control socket, with insertion via `wl-copy` plus
a paste shortcut, and direct `wtype` as a fallback.

## Demo

[Watch a 48-second demo video](docs/assets/sunoto-demo.mp4).

A macOS port is in progress (see `docs/macos-port-plan.md`): the same daemon
with CGEventTap hotkeys, CoreAudio capture, CGEvent text insertion, and local
ASR. The default macOS real-ASR path is **Parakeet-MLX streaming on Apple
GPU/Metal** (`backend="parakeet_mlx_streaming"`, model
`mlx-community/parakeet-tdt-0.6b-v3`, `profile_ms=560`). The stable
whole-utterance `parakeet_mlx_offline` backend and the older Nemotron CPU
backend remain available for comparison. Status by phase is tracked in
`scripts/macos-port/phases.md`; verify any phase with
`scripts/macos-port/verify-phase.sh <N>`.

## Quickstart

```bash
bash install.sh        # build, write config, install + start the user service
```

Then **hold Ctrl+F1 anywhere**, speak, release — the text is typed at your
cursor. The default backend is `mock` (fixed text, useful to verify the
plumbing); switch to real ASR by setting `"backend": "nemotron"` in
`~/.config/sunoto/config.json` and restarting the service.

For development, skip the service and run directly:

```bash
make phase1-run             # daemon with the mock ASR backend
make phase1-run-nemotron    # daemon with real Nemotron ASR
make test                   # all Rust + Python suites
make ui-demo                # drive the overlay pill by hand
```

## How it works

```
hold Ctrl+F1          release
     │                   │
     ▼                   ▼
persistent mic ──► streaming ASR sidecar ──► deterministic polish ──► desktop
capture + preroll    (Nemotron cache-aware     (fillers, corrections,   insertion
(PulseAudio)          RNNT; partials stream     dictionary, snippets,   (XTEST on X11,
                      while you speak)          app-aware styles)       paste on Wayland)
```

- **Always warm:** the ASR model loads once at startup and stays on the GPU;
  the hotkey never waits for a model load.
- **Push-to-talk is the end-of-utterance signal** — no VAD silence wait. On
  release, residual audio is flushed immediately.
- **Deterministic polish, not an LLM,** is on the latency path (measured in
  microseconds): filler removal, "Tuesday, actually Wednesday" corrections,
  personal dictionary, snippets, and per-app style (e.g. terminals get
  lowercase/no-punctuation treatment via WM_CLASS detection).
- **Injection safety:** Enter/Tab are neutralized by default, focus is
  revalidated before typing (if you switched windows, the text parks on the
  clipboard with a notice instead), and held modifiers are released around
  injection.
- **Status UI:** a GTK4 pill overlay (recording dot + live level meter +
  status text) anchored top-center — layer-shell on Wayland compositors,
  EWMH hints on X11. If GTK4 isn't installed, a native X11 bubble takes
  over automatically. Either way the UI is fed off the latency path and the
  daemon keeps dictating if it dies.

## Commands

| Command | What it does |
| --- | --- |
| `sunoto-daemon run` | Run the dictation daemon (what the service runs). |
| `sunoto-daemon check` | Verify X11, XTEST, shortcut grab, and the sidecar protocol. |
| `sunoto-daemon selftest` | X11 insertion, push-to-talk, clipboard self-tests. |
| `sunoto-daemon insert TEXT` | Type TEXT at the focused cursor (plumbing test). |
| `sunoto-daemon trigger press\|release` | Push-to-talk edge used by Hyprland/Wayland bindings. |
| `sunoto-daemon bench` | Release-to-insertion latency percentiles. |
| `sunoto-daemon eval` | Zero-edit rate of the polish pipeline on a corpus. |
| `sunoto-daemon config show\|init` | Inspect or create the settings file. |
| `bash bin/gpu-status.sh` | GPU health summary (Xid events, daemon action log). |

## Configuration

`~/.config/sunoto/config.json` (create with `sunoto-daemon config init`):

| Field | Default | Meaning |
| --- | --- | --- |
| `shortcut` | `"Ctrl+F1"` | Push-to-talk hold shortcut. |
| `backend` | `"mock"` | `"nemotron"` for Linux/CUDA streaming ASR; `"parakeet_mlx_streaming"` for the default macOS Parakeet streaming backend (live partials + direct PCM final); `"parakeet_mlx_offline"` for the stable macOS whole-utterance backend; `"nemotron_offline"` for the older macOS NeMo CPU backend. macOS config init defaults to `parakeet_mlx_streaming`. |
| `asr_device` | unset | Streaming Nemotron defaults to CUDA when unset. `parakeet_mlx_offline` and `parakeet_mlx_streaming` select Apple compute units themselves, so leave this unset for those backends. |
| `asr_model` | unset | Optional model override for Parakeet-MLX backends; unset means `mlx-community/parakeet-tdt-0.6b-v3`. |
| `profile_ms` | `160` | Streaming chunk: 80 (fastest) / 160 / 560 / 1120 (most accurate). macOS config init defaults to `560`. |
| `microphone` | `"auto"` | PulseAudio source name; auto rejects monitor sources. |
| `overlay_enabled` | `true` | GTK4 pill overlay; falls back to the X11 bubble if unavailable. |
| `allow_enter_and_tab` | `false` | Keep control characters out of terminals unless you opt in. |
| `polish_enabled` | `true` | Deterministic cleanup pipeline on/off. |
| `overlay_backend` | `"auto"` | `"wayland"` for a Wayland GTK/layer-shell overlay, `"x11"` for X11 anchoring, `"auto"` to pick from the active GDK backend. |
| `polish.app_styles` | terminal rules | Per-WM_CLASS writing styles. |

See [desktop configuration](docs/desktop-configuration.md) for the complete
Wayland/Hyprland and X11 setup, keybindings, profiles, and troubleshooting.

## Repo layout

```
apps/daemon/           Rust dictation daemon (run/check/bench/eval CLIs)
crates/
  sunoto-core/         session state machine
  sunoto-audio/        persistent PulseAudio capture + preroll
  sunoto-ipc/          sidecar process management, NDJSON event pump
  sunoto-linux/        X11 adapters: hotkey, XTEST insertion, clipboard, bubble
  sunoto-polish/       cleanup, dictionary, snippets, app-aware styles
services/asr/          Nemotron streaming sidecar + mock sidecar (Python)
src/voice_dictation/   GTK4 overlay pill + its stdin-NDJSON UI sidecar driver
bin/                   GPU health/watchdog helper scripts
tests/, tools/         Rust + Python suites, corpus record/transcribe tools
docs/                  product plan, phase results, integration plan
```

## Performance (measured, RTX 3060, X11)

- Release-to-insertion: **151 ms p95** (160 ms profile), 104 ms (80 ms profile)
- Release-to-final ASR result: 125 ms p95
- Text insertion: 26 ms p95 including echo confirmation
- Deterministic polish: microseconds
- Zero-edit rate on the scripted corpus: **96% polished vs 28% raw**
- Sidecar VRAM while streaming: ~3.6 GiB; warm start 16–32 s (optimization pending)

## macOS performance note

Use `backend="parakeet_mlx_streaming"` on macOS (the default). It runs
`mlx-community/parakeet-tdt-0.6b-v3` through parakeet-mlx on MLX's Apple GPU
backend (`Device(gpu, 0)` on the M1 Pro test machine) and streams live partials
while recording via `transcribe_stream()`, then runs the final transcript from
the full buffered utterance through parakeet-mlx's low-level `get_logmel()` +
`model.generate()` API. Normal dictation feeds captured PCM directly into that
path, so temp WAVs and ffmpeg are not on the hot path. The default chunk is
`profile_ms=560`. One-time setup in the existing macOS ASR venv:

```bash
.venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx
# Optional: needed for parakeet-mlx CLI/file benchmarks or chunked transcribe fallback
brew install ffmpeg
```

M1 Pro Phase 0 result: cached load ~1.35s, warmup ~1.22s, p50 release-to-final
ASR latency ~0.24s on the LibriSpeech sample set, p50 RTF ~0.024, peak RSS
~1.6 GiB. A direct-PCM protocol smoke decoded a 4.82s LibriSpeech clip in
151ms. See `docs/parakeet-mlx-migration-plan.md` for the benchmark details.

`backend="parakeet_mlx_offline"` is the stable whole-utterance alternative
(no streaming partials; the overlay stays minimal until the final result). A
protocol smoke emitted 9 partials for the streaming backend and recovered the
offline-quality final (`Quilter`, not the streaming partial's `Coulter`) with
~247ms final decode on a 4.82s LibriSpeech clip.

The older macOS NeMo backend remains available as `backend="nemotron_offline"`
with `asr_device="cpu"` (slow PyTorch/NeMo CPU path). The cache-aware Nemotron
streaming sidecar can run on CPU, but live testing showed it blocked badly: a
26.6s hold delivered only about 4.6s of audio and still waited about 7.4s after
release.

## Troubleshooting

- **Transcript generated but no text appears:** read the `insertion target at
  release: "instance" / "Class"` and `inserted via ...` journal lines for that
  session. On Wayland, `Pasted` means clipboard paste was used, `Typed` means
  direct `wtype` fallback was used, and `ClipboardOnly` means focus changed so
  the result was left on the clipboard.
- **Short phrases like "Thank you." from silence:** near-silence audio can
  make the ASR model hallucinate a short phrase. Check the session's logged
  rms value — if it's near zero, the mic heard nothing; fix the input
  source rather than the model.
- **Service won't start:** `journalctl --user -u voice-dictation.service -n 50`.
  On Wayland, verify `hyprctl`, `wtype`, and `wl-copy` are installed. On X11,
  verify the X11 display and XTEST are available.
- **No overlay pill:** install GTK4 bindings and, on Wayland, gtk4-layer-shell.
  The daemon logs overlay startup failures and keeps dictation running.
- **Hotkey does nothing:** the ASR sidecar may still be loading (the daemon
  logs `ASR sidecar ready` when warm; Nemotron takes 16–32 s today). The
  bubble shows "ASR still loading..." if you press too early.
- **Wrong microphone:** set `"microphone"` in the config to a `pactl list
  short sources` name. Auto-selection rejects monitor (speaker loopback)
  sources on purpose.
- **GPU issues:** `bash bin/gpu-status.sh` summarizes Xid events and the
  daemon action log.

## History

This repository merges two lineages: a Python faster-whisper dictation
daemon (whose GTK4 overlay survives as the UI) and the sunoto Rust pipeline
(which replaced the Python backend). `git log` preserves both histories;
`docs/product-plan.md` and `docs/integration-plan.md` carry the full plan.

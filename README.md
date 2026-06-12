# voice-dictation (sunoto)

System-wide, local-first voice dictation for Linux. Hold a hotkey, speak,
release — clean text lands at the cursor of whatever application is focused.
No cloud, no API keys.

Rust daemon + always-warm streaming ASR (NVIDIA Nemotron Speech Streaming
0.6B via NeMo) + deterministic text polish + GTK4 overlay. Built for speed:
**release-to-insertion is 151 ms p95** measured on an RTX 3060 (target was
600 ms).

X11 is the supported session type today; Wayland adapters are planned
(Phase 4 in `docs/product-plan.md`).

## Quickstart

```bash
bash install.sh        # build, write config, install + start the user service
```

Then **hold Ctrl+F8 anywhere**, speak, release — the text is typed at your
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
hold Ctrl+F8          release
     │                   │
     ▼                   ▼
persistent mic ──► streaming ASR sidecar ──► deterministic polish ──► XTEST
capture + preroll    (Nemotron cache-aware     (fillers, corrections,   typing, with
(PulseAudio)          RNNT; partials stream     dictionary, snippets,   clipboard
                      while you speak)          app-aware styles)       fallback
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
| `sunoto-daemon bench` | Release-to-insertion latency percentiles. |
| `sunoto-daemon eval` | Zero-edit rate of the polish pipeline on a corpus. |
| `sunoto-daemon config show\|init` | Inspect or create the settings file. |
| `bash bin/gpu-status.sh` | GPU health summary (Xid events, daemon action log). |

## Configuration

`~/.config/sunoto/config.json` (create with `sunoto-daemon config init`):

| Field | Default | Meaning |
| --- | --- | --- |
| `shortcut` | `"Ctrl+F8"` | Push-to-talk hold shortcut. |
| `backend` | `"mock"` | `"nemotron"` for real ASR (needs `.venv-nemotron`). |
| `profile_ms` | `160` | Streaming chunk: 80 (fastest) / 160 / 560 / 1120 (most accurate). |
| `microphone` | `"auto"` | PulseAudio source name; auto rejects monitor sources. |
| `overlay_enabled` | `true` | GTK4 pill overlay; falls back to the X11 bubble if unavailable. |
| `allow_enter_and_tab` | `false` | Keep control characters out of terminals unless you opt in. |
| `polish_enabled` | `true` | Deterministic cleanup pipeline on/off. |
| `polish.app_styles` | terminal rules | Per-WM_CLASS writing styles. |

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

## Troubleshooting

- **Transcript generated but no text appears:** read the
  `insertion target at release: "instance" / "Class"` journal line for that
  session — it names the window that actually received the keystrokes. If
  it isn't the app you expected, the focus was elsewhere when you released
  the hotkey; if it is, that app may discard synthetic XTEST input.
- **Short phrases like "Thank you." from silence:** near-silence audio can
  make the ASR model hallucinate a short phrase. Check the session's logged
  rms value — if it's near zero, the mic heard nothing; fix the input
  source rather than the model.
- **Service won't start:** `journalctl --user -u voice-dictation.service -n 50`.
  The daemon needs an X11 session (`echo $XDG_SESSION_TYPE`).
- **No overlay pill:** install `gir1.2-gtk-4.0` and `python3-xlib`; the
  daemon logs `overlay UI exited before becoming ready` and uses the native
  bubble until then.
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

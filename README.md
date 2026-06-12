# voice-dictation

System-wide voice dictation for Linux / Hyprland. Press a hotkey, speak, press again — your words get pasted into whatever input is focused (terminal TUI, browser, editor, Slack).

Local Whisper (faster-whisper + CTranslate2 on CUDA), silero-VAD streaming, GTK4 layer-shell overlay, Hyprland-native paste injection. No cloud, no API keys.

The overlay is intentionally visual-only while recording: a glowing signal orb, a wide level rail, and an animated waveform show whether the selected microphone is actually receiving sound. It does not show transcripts, status words, or "done" messages.

## Quickstart

```bash
cd /home/shakdwipeea/workspace/pg/voice-dictation
uv sync                  # install all Python deps (already done)
bash install.sh          # wire up systemd + Hyprland binding
```

Then press **SUPER + I** anywhere — overlay appears, speak, press again, text pastes.

## What it does

```
SUPER+I press         mic → silero-VAD → faster-whisper                paste
   │             ┌──────────────────────────────────────┐                │
   ▼             │  (segments emit at silence; whisper  │                ▼
recording        │   transcribes each on GPU; segments  │     wl-copy + hyprctl
overlay          │   appear in overlay live)            │     dispatch sendshortcut
appears          └──────────────────────────────────────┘     (Ctrl+Shift+V for
                                  │                            terminals like
SUPER+I press ────────────────────┘                            Ghostty/Claude Code,
                                                               Ctrl+V for the rest)
```

- **Model**: `large-v3` (Whisper) running float16 on CUDA. Prioritizes transcription quality over turbo latency.
- **Latency**: optimized for quality rather than minimum delay; use `large-v3-turbo` if you prefer lower latency.
- **Visual feedback**: the overlay meter is driven directly by live peak/RMS audio levels. Quiet slate means little/no input; green glow means live input; amber/red means hot/loud input.
- **Daemon model is loaded once** and reused across every toggle. Full `large-v3` needs more RAM/VRAM than turbo, so the user service allows a larger memory budget.

## Commands

| Command | What it does |
| --- | --- |
| `vd` / `vd toggle` | Start or stop recording — what the hotkey runs. |
| `vd status` | Daemon state, model, selected mode, last text. |
| `vd last` | Print the most recent transcription. |
| `vd simulate <wav>` | Test: transcribe a 16-kHz mono wav instead of mic. |
| `vd shutdown` | Stop the daemon. |
| `vd-samples` | Interactive TUI for recording + transcribing test samples (compare models, measure WER on your voice). |
| `vd-smoke --device cuda --model M --wav F.wav` | One-shot pipeline check. |
| `bash bin/gpu-status.sh` | RTX 4090 health summary — current state, Xid events past + present, daemon action log. |

## Visual overlay

The recording overlay is a compact visual instrument, not a transcript panel:

- **Orb**: recording heartbeat / signal state.
- **Level rail**: live amplitude, scaled from the current peak and RMS values.
- **Wave bars**: animated waveform-style activity. A tiny moving pulse means the daemon is recording but receiving little signal; larger green/yellow bars mean audio is reaching the selected input.

If the overlay barely moves while you speak, the issue is almost certainly the OS audio source, input gain, mute state, or the physical mic connection — not Whisper.

## Configuration

Daemon flags (edit `~/.config/systemd/user/voice-dictation.service` then `systemctl --user daemon-reload && systemctl --user restart voice-dictation`):

```
--device cuda|cpu          # default: cuda
--model large-v3           # any faster-whisper model name; large-v3-turbo / distil-large-v3 for lower VRAM/latency
--no-paste                 # transcribe but don't inject
--no-overlay               # headless (testing / SSH)
--input-device hw:2,0      # override sounddevice probe
--vocabulary-file FILE     # bias Whisper with extra terms, one term per line
--technical-vocabulary-bias # bias Whisper with the built-in programming glossary
--no-technical-corrections # disable conservative programming-term cleanup
--lazy-load                # delay model load until first toggle
```

Technical dictation gets narrow cleanup by default for common ASR mistakes such
as `origin slash monsters` → `origin/master`, `cube cuddle` → `kubectl`, and
`type script` → `TypeScript`. Whisper vocabulary biasing is opt-in because large
prompts can hurt general dictation: use `--technical-vocabulary-bias` for the
built-in glossary or `--vocabulary-file FILE` for custom/product terms. Vocabulary
files use one term per line; blank lines and `# comments` are ignored.

Hotkey lives in `~/.config/hypr/voice-dictation.conf`. To rebind, edit the `bindd = SUPER, I, …` line and run `hyprctl reload`.

## Repo layout

```
voice-dictation/
├── src/voice_dictation/
│   ├── daemon.py        # long-running process; IPC + overlay main loop
│   ├── pipeline.py      # mic → VAD → whisper → overlay accumulator
│   ├── segmenter.py     # silero-VAD streaming segmentation
│   ├── overlay.py       # GTK4 layer-shell pill
│   ├── audio.py         # input device probe + 48k→16k resample
│   ├── stt.py           # faster-whisper wrapper
│   ├── inject.py        # wl-copy + hyprctl dispatch sendshortcut
│   ├── client.py        # the `vd` CLI
│   ├── smoke.py         # `vd-smoke` one-shot test
│   ├── sample_manager.py# `vd-samples` TUI
│   └── _cuda_preload.py # bundle cu12 libs from venv into dlopen path
├── bin/
│   ├── gpu-watch.sh     # background Xid watcher (persistent log)
│   ├── gpu-snapshot.sh  # nvidia-smi rolling log
│   ├── gpu-status.sh    # one-shot health summary (also the gpu-status skill)
│   └── action-log.sh    # last-action.txt + actions.log for crash post-mortem
├── tests/samples/       # vd-samples writes here (ground-truth + wav per sample)
├── systemd/voice-dictation.service
├── hypr/voice-dictation.conf
├── install.sh
└── pyproject.toml
```

## Troubleshooting

### "GPU has fallen off the bus" (Xid 79)
This project has a survive-reboot Xid monitor. Run `bash bin/gpu-status.sh` to see the current GPU state, any Xid events on the current or previous boot, and what action the daemon was attempting right before any crash. On a 4090, Xid 79 most often means the 12VHPWR connector isn't fully seated — reseat it firmly (the latch should click) before running heavy GPU work again.

### `PortAudioError: ALSA error -2 'No such file or directory'`
PortAudio's PipeWire shim is fragile on some Arch setups. The daemon auto-probes input devices (`hw:2,0 @ 48 kHz` typically works) and resamples to 16 kHz via torchaudio. Override with `--input-device hw:X,Y` if probing picks the wrong mic.

### Overlay moves but transcription says the same generic phrase
Whisper can hallucinate short phrases from near-silence. The daemon suppresses the full-recording fallback for low-RMS audio so low-signal captures do not paste generic phrases such as "Thank you." If this happens, verify the mic source first:

```
uv run python - <<'PY'
import sounddevice as sd
print('default:', sd.default.device)
for i, d in enumerate(sd.query_devices()):
    if d.get('max_input_channels', 0) > 0:
        print(i, d['name'], d['max_input_channels'], d['default_samplerate'])
PY
pactl list short sources
wpctl status
```

On this machine, PipeWire exposes `USB Audio Microphone` as the named mic source. If the visualizer stays quiet while speaking, check that source is selected and unmuted in your audio settings.

### `GtkWindow is not a layer surface`
gtk4-layer-shell must load before libwayland-client. The daemon auto re-execs itself with `LD_PRELOAD=/usr/lib/libgtk4-layer-shell.so` on startup — if you see this warning, either the lib is missing (`pacman -S gtk4-layer-shell`) or you launched the daemon with `--no-overlay` set (which is fine — headless mode skips the re-exec).

### Paste doesn't reach the focused window
- Terminal vs other apps use different paste shortcuts. The daemon detects the active window class via `hyprctl activewindow -j` and picks `Ctrl+Shift+V` for matching terminals (Ghostty, Alacritty, Kitty, Foot, WezTerm) or `Ctrl+V` for everything else.
- Zed's integrated terminal still reports the Zed app class, not a terminal class. The daemon treats Zed as a `Ctrl+Shift+V` target and leaves the dictated text on the clipboard so `SUPER+V` manual paste does not select the previous dictation.
- Hyprland's `sendshortcut` can return `ok` even when the focused app does not receive the paste event. If `wtype` is installed, the daemon can fall back to direct Wayland typing. Install with `sudo pacman -S wtype`.
- If the active class isn't recognized as a terminal but should be, add it to `TERMINAL_CLASS_REGEX` in `src/voice_dictation/inject.py`.

### Daemon stuck or unresponsive
```
systemctl --user restart voice-dictation
journalctl --user -u voice-dictation -n 100 --no-pager
```

## Calibrating accent / tech vocab

Use `vd-samples`:

```
$ uv run vd-samples
> n                                # record a sample, type the ground truth
> t all                            # transcribe every sample, see WER per sample
> t 1 --model small.en --device cpu # compare another model on sample 1
```

The TUI saves wav + ground-truth JSON to `tests/samples/`. Re-run `t all` after any pipeline change to see if you've made things better or worse on YOUR voice.

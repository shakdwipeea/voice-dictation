# Desktop configuration

This is the short setup reference for running voice-dictation on
Hyprland/Wayland, X11, and macOS.

## Common config

App config:

```text
~/.config/sunoto/config.json
```

Typical real-ASR config:

```json
{
  "shortcut": "Ctrl+F1",
  "backend": "nemotron",
  "profile_ms": 160,
  "overlay_enabled": true,
  "overlay_backend": "wayland"
}
```

Restart after config changes:

```bash
systemctl --user restart voice-dictation.service
```

Useful logs:

```bash
journalctl --user -u voice-dictation.service -n 120 --no-pager
```

## Hyprland / Wayland

Wayland does not let the daemon grab global keys directly. Hyprland owns the
shortcut and calls:

```bash
sunoto-daemon trigger press
sunoto-daemon trigger release
```

Required tools:

```bash
hyprctl
wtype
wl-copy
gtk4-layer-shell
```

Use:

```json
{
  "overlay_backend": "wayland"
}
```

Live Hyprland files:

```text
~/.config/hypr/voice-dictation.conf
~/.config/hypr/bindings.conf
```

Repo template:

```text
hypr/voice-dictation.conf
```

`~/.config/hypr/bindings.conf` should include:

```ini
source = ~/.config/hypr/voice-dictation.conf
```

Default Wayland shortcut:

```text
Hold Ctrl+F1 to record. Release to transcribe and paste.
```

The Hyprland template also includes aliases for keyboards that emit media keys
instead of real F-keys:

- `Ctrl+XF86MonBrightnessDown` for F1 media mode.
- `Ctrl+F8` legacy binding.
- `Ctrl+XF86AudioPlay` / `Ctrl+XF86AudioPause` for F8 media mode.

Release bindings are intentionally duplicated with and without `CTRL`. This
prevents stuck recordings when you release `Ctrl` before releasing `F1/F8`.

After editing Hyprland config:

```bash
hyprctl reload
hyprctl configerrors
hyprctl binds | rg -C 3 'Voice dictation|sunoto-daemon trigger'
```

Wayland insertion:

- First tries clipboard paste: `wl-copy` + paste shortcut through `wtype`.
- Uses `Ctrl+Shift+V` for terminals and `Ctrl+V` elsewhere.
- Falls back to direct `wtype` typing if paste fails.
- Leaves text on clipboard if focus changed before insertion.

## X11

On X11, the daemon owns the global shortcut directly. Hyprland bindings are not
used.

Use:

```json
{
  "shortcut": "Ctrl+F1",
  "overlay_backend": "x11"
}
```

The shortcut must include at least one modifier. Good examples:

```json
{ "shortcut": "Ctrl+F1" }
{ "shortcut": "Ctrl+Shift+Space" }
{ "shortcut": "Alt+F8" }
```

Avoid bare keys like `"F1"`.

X11 insertion uses XTEST direct typing first, then clipboard fallback when
needed.

Verify X11:

```bash
target/release/sunoto-daemon check
target/release/sunoto-daemon selftest
```

## macOS

macOS uses a different config path:

```text
~/Library/Application Support/sunoto/config.json
```

Typical macOS real-ASR config:

```json
{
  "shortcut": "Ctrl+F1",
  "backend": "parakeet_mlx_streaming",
  "profile_ms": 560,
  "overlay_backend": "macos"
}
```

`backend = "parakeet_mlx_streaming"` is the default macOS backend. It uses
parakeet-mlx `transcribe_stream()` for live partials while recording, then runs
the final transcript from the full buffered utterance through the same direct
PCM `get_logmel()` + `model.generate()` path as the offline backend. There is
no WAV/ffmpeg/file-based fallback. The default chunk is 560ms
(`profile_ms = 560`): a larger chunk gives more accurate streaming partials at
the cost of slightly slower partial cadence. Leave `asr_device` unset; MLX
selects Apple GPU/Metal compute units itself. One-time setup in the existing
macOS ASR venv:

```bash
.venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx
# Optional: needed for parakeet-mlx CLI/file benchmarks or chunked transcribe fallback
brew install ffmpeg
```

`backend = "parakeet_mlx_offline"` is the stable whole-utterance alternative:
same parakeet-mlx `mlx-community/parakeet-tdt-0.6b-v3` model and direct PCM
`get_logmel()` + `model.generate()` path, but no streaming partials (the overlay
stays minimal until the final result arrives). Use it if streaming partials are
unreliable or you only care about the final transcript.

`backend = "nemotron_offline"` remains available as the older whole-utterance
RNNT/NeMo sidecar. If using it on macOS, set `asr_device = "cpu"`; MPS and CPU
streaming remain experimental, and live testing showed CPU streaming cannot keep
up on this Mac.

Run the Parakeet-MLX real-speech benchmark:

```bash
.venv-nemotron-mac/bin/python tools/phase0/parakeet_mlx_measure.py \
  --limit 5 \
  --output build/phase0/parakeet-mlx-v3.json
```

For the older Nemotron CPU benchmark:

```bash
.venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
  --device cpu \
  --output build/phase0/macos-real-speech-cpu.json
```

Both benchmarks download a small LibriSpeech sample from Hugging Face, export
16 kHz mono WAVs, and report latency, real-time factor, and WER for the offline
transcribe path. Stop the running daemon/sidecar first so a second ASR model is
not loaded.

macOS requires one-time Privacy & Security permissions:

- Accessibility for CGEvent insertion and hotkeys.
- Input Monitoring for the global event tap.
- Microphone for CoreAudio capture.

### Starting the application on macOS

Build the release binary and the native overlay:

```bash
cargo build --release
swiftc services/macos/sunoto-overlay.swift -o target/release/sunoto-overlay
```

Make sure your config uses the default macOS backend:

```bash
target/release/sunoto-daemon config init
target/release/sunoto-daemon config show
```

For automatic startup, install the GUI Login Item so Sunoto starts through
Launch Services with a responsible GUI/TCC context:

```bash
bash install-macos.sh
open "$HOME/Applications/Sunoto Login.app"
```

For manual development, run the daemon from a terminal:

```bash
nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 &
```

Wait for `ASR sidecar ready` (~16–32s warmup while parakeet-mlx loads), then
hold Ctrl+F1, speak, and release — clean text is pasted at the focused cursor.

Watch live logs:

```bash
tail -f /tmp/sunoto-bare.log
```

Stop the daemon:

```bash
pkill -f 'target/release/sunoto-daemon run'
```

The old launchd LaunchAgent is intentionally removed by `install-macos.sh`:
launchd has no responsible GUI process and macOS disables its event tap. See
[macos-gui-login-item-plan.md](macos-gui-login-item-plan.md) and
[macos-recurring-issues.md](macos-recurring-issues.md) for full detail.

## ASR profile

Current default:

```json
{ "profile_ms": 160 }
```

On macOS the default is `560` (larger chunks, more accurate streaming partials):

```json
{ "profile_ms": 560 }
```

Profiles:

| Profile | Notes |
| --- | --- |
| `80` | Lowest latency. |
| `160` | Balanced default (Linux). |
| `560` | macOS default; larger chunks, more accurate partials. |
| `1120` | Largest chunks, highest latency. |

Use `"backend": "nemotron"` for streaming real ASR on Linux/CUDA,
`"backend": "parakeet_mlx_streaming"` for the default macOS streaming Parakeet
backend (live partials + direct PCM final), `"backend": "parakeet_mlx_offline"`
for the stable macOS whole-utterance Parakeet backend (no partials),
`"backend": "nemotron_offline"` for the older macOS NeMo CPU backend, and
`"mock"` only for plumbing tests.

## Quick troubleshooting

- Shortcut does nothing: check `hyprctl binds` on Wayland or
  `sunoto-daemon check` on X11.
- Release sometimes does not stop: make sure the no-modifier release bindings
  from `hypr/voice-dictation.conf` are installed.
- Text appears late: check logs for `inserted via Pasted` versus `Typed`.
- Text is not inserted: `ClipboardOnly` means focus changed; paste manually.
- Empty result: check session `rms` in logs; low `rms` usually means wrong or
  muted microphone.
- Nemotron/GPU status: `bash bin/gpu-status.sh`.

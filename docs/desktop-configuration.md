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
  "backend": "nemotron_offline",
  "asr_device": "cpu",
  "overlay_backend": "macos"
}
```

`backend = "nemotron_offline"` uses the whole-utterance RNNT sidecar. CPU is the
current macOS default because it is much more stable than MPS in live short
dictation. `backend = "nemotron"` with `asr_device = "cpu"` remains selectable
for experiments, but live testing showed CPU streaming cannot keep up on this
Mac.

Run the real-speech benchmark:

```bash
.venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
  --device cpu \
  --output build/phase0/macos-real-speech-cpu.json
```

The benchmark downloads a small LibriSpeech sample from Hugging Face, exports
16 kHz mono WAVs, and reports latency, real-time factor, and WER for the
offline transcribe path. Stop the running daemon/sidecar first so a second
Nemotron instance is not loaded.

macOS requires one-time Privacy & Security permissions:

- Accessibility for CGEvent insertion and hotkeys.
- Input Monitoring for the global event tap.
- Microphone for CoreAudio capture.

## ASR profile

Current default:

```json
{ "profile_ms": 160 }
```

Profiles:

| Profile | Notes |
| --- | --- |
| `80` | Lowest latency. |
| `160` | Balanced default. |
| `560` | Larger chunks, more latency. |
| `1120` | Largest chunks, highest latency. |

Use `"backend": "nemotron"` for streaming real ASR on Linux/CUDA,
`"backend": "nemotron_offline"` for macOS whole-utterance real ASR, and
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

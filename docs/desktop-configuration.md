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

Idle and privacy keys (all optional):

```json
{
  "capture_idle_stop_secs": 0,
  "clipboard_restore": true,
  "log_transcripts": true,
  "llm_polish_keepalive_secs": 1.0,
  "llm_polish_keepalive_always": false
}
```

- `capture_idle_stop_secs`: how long the microphone stays on after a
  dictation ends. `0` (the default) turns it off right away, so the menu-bar
  microphone indicator shows only while you dictate. A tap turns it back on;
  on a MacBook Pro the first sound arrives 70 to 105 ms after the tap (about
  180 ms for the first dictation after launch), and speech before that is not
  recorded. A larger value keeps the mic on for quick follow-ups, which then
  also get the 300 ms pre-roll.
- `clipboard_restore` (macOS): put the previous clipboard back about 300 ms
  after a paste.
- `log_transcripts`: `false` replaces transcript text in the log with its
  length and a short hash.
- `llm_polish_keepalive_secs`: GPU ping interval while a dictation session
  is in flight (press until 20 s after the polish result). `0` disables.
- `llm_polish_keepalive_always`: ping all day, as before 2026-09-17. Costs
  battery; only for profiling.

Restart after config changes:

```bash
systemctl --user restart voice-dictation.service
```

Useful logs:

```bash
journalctl --user -u voice-dictation.service -n 120 --no-pager
```

## Voice Spotlight / System mode

System mode is a separate push-to-talk path and remains disabled unless it is
explicitly enabled. It searches installed applications plus approved local
roots, presents one native palette, and never opens a result before selection.

```json
{
  "system_mode_enabled": true,
  "system_shortcut": "Ctrl+F2",
  "system_search_roots": ["Desktop", "Documents", "Downloads", "workspace"]
}
```

Relative search roots resolve below the current user's home directory;
absolute roots are also accepted when deliberately configured. The filesystem
root and parent traversal are rejected. Search is bounded, hidden/system-like
subtrees are skipped, symlinks must remain inside an approved canonical root,
and only safe document types are offered. Project folders are identified from
common project markers rather than user-specific paths. On macOS an indexed
Spotlight lookup feeds the same safe classifier before the bounded filesystem
fallback; Linux uses the cancellable bounded fallback.

Examples:

```text
Open Chrome
Open who-else-is-free
Open who-else-is-free in VS Code
Find the file quarterly report
Show quarterly report in Finder
Open folder Downloads
```

Read-only inspection never authorizes execution:

```bash
target/debug/sunoto-daemon system plan "open who-else-is-free in VS Code"
target/debug/sunoto-daemon system resolve "open who-else-is-free"
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
Tap Ctrl+F1 to start recording. Tap it again to transcribe and paste.
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
  "shortcut": "RightOption",
  "backend": "parakeet_mlx_offline",
  "profile_ms": 560,
  "overlay_backend": "macos"
}
```

### Shortcut on macOS

The default is **Right Option** on its own: one key under the right thumb that
does nothing by itself in macOS. **Tap** it to start recording, speak, and
**tap** it again to transcribe and paste. Recordings stop on their own after 5
minutes.

**Esc** throws away the recording in progress. The tap only listens, so the
app in front also receives the Esc. A tap counts only when no other key is
pressed while Right Option is down, so Option characters (Option-E for an
accent, say) still work and never start or stop a recording.

Other single keys: `"RightCommand"`, `"RightControl"`, `"RightShift"`. Chords
such as `"Ctrl+F1"` or `"Cmd+Shift+Space"` still work. Left-hand modifiers and
the Globe/Fn key are not offered: macOS and other apps already use them.

`backend = "parakeet_mlx_offline"` is the default macOS backend. It runs
parakeet-mlx's direct PCM `get_logmel()` + `model.generate()` path, with no
WAV/ffmpeg step. A dictation under 10 seconds gets one whole-utterance pass
when you tap to finish. A longer one is transcribed in pieces while you talk:
once 10 seconds are untranscribed, the sidecar cuts at the next pause (or at
the quietest moment by 20 seconds) and a background thread transcribes that
piece. Each piece re-hears the 3 seconds before its cut, and the pieces are
stitched with parakeet-mlx's own overlap merge, so a long dictation reads the
same as one pass. Only the open piece's audio is kept.

The Apple GPU slows down after a couple of idle seconds, and waking it costs
about 0.6 s. So 200 ms into each pause after you speak, the sidecar starts
transcribing what you said so far. If you then tap without saying anything
more, that result is the transcript and the wake-up already happened during
your pause: in testing the wait after the tap fell from about 0.6 s to a few
milliseconds (up to ~0.3 s with a very quick tap). If you keep talking, it is
discarded. The click of the key itself is ignored. Leave `asr_device`
unset; MLX selects Apple GPU/Metal compute units itself. One-time setup in the
existing macOS ASR venv:

```bash
.venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx
# Optional: needed for parakeet-mlx CLI/file benchmarks or chunked transcribe fallback
brew install ffmpeg
```

Setup downloads the speech model as Sunoto's bf16 copy of
`mlx-community/parakeet-tdt-0.6b-v3` (1.25 GB instead of 2.5 GB, from the
`model-parakeet-tdt-0.6b-v3-bf16` GitHub release, checksums verified) into
`~/Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16`.
parakeet-mlx casts the float32 original to bf16 on load, so the weights are
identical. The app uses that folder when it is there and the Hugging Face
original otherwise; an install that already has the original keeps it.
`asr_model` overrides both.

`backend = "parakeet_mlx_streaming"` is the older alternative: the same model
streams partials through `transcribe_stream()` while you talk (never shown;
they back up the final when it comes out garbled), then re-decodes the whole
buffered dictation on release. That release pass grows with the dictation
(about 2.5 s and a 3 GB+ memory spike for a minute of speech), and the
partials keep the GPU busy while you talk. `profile_ms = 560` sets its chunk
size: larger chunks give more accurate partials at a slower cadence.

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

- Accessibility, for the shortcut's event tap and for pasting (CGEvent).
- Microphone, for CoreAudio capture.

Input Monitoring is not needed: Accessibility grants both listening to and
posting keyboard events, while Input Monitoring only grants listening.

On first install, Sunoto opens one onboarding panel with two steps. Only the
next missing permission can be opened, so macOS never stacks several prompts
or Settings panes. Accessibility is requested by the daemon's main loop when
its **Allow** button is clicked; Microphone capture does not start until its
step. Terminal commands such as `check`, `selftest`, and `insert` do not
create separate permission entries. After both rows say **Allowed**, click
**Done**. The panel then closes and Sunoto
relaunches once to apply the keyboard grants. The standard close button lets
you postpone setup without granting anything else.

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

For automatic startup, install `Sunoto.app` as a Login Item. The daemon is
the bundle's executable, so Launch Services starts it with a responsible
GUI/TCC context and there is a single entry to grant permissions to:

```bash
bash install-macos.sh          # build, then: sunoto-daemon setup
target/release/sunoto-daemon status
```

`setup` accepts `--dry-run` (assemble `target/release/Sunoto.app` only),
`--no-login-item`, and `--timeout-secs N`. It creates a self-signed
**Sunoto Local Code Signing** identity in the login keychain, re-signs the
installed app, and waits until the app reports `ready` over the control socket.
The first install after migrating from an ad-hoc build asks for permissions
once. Later rebuilds and reinstalls retain the grants because the signing
identity is stable. If identity creation fails, setup warns and uses an ad-hoc
signature rather than aborting the install.

For manual development, run the daemon from a terminal:

```bash
nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 &
```

Wait for `ASR sidecar ready` (~16–32s warmup while parakeet-mlx loads), then
tap Right Option, speak, and tap it again — clean text is pasted at the
focused cursor.

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
`"backend": "parakeet_mlx_offline"` for the default macOS Parakeet backend
(no partials; long dictations transcribed in pieces while you talk),
`"backend": "parakeet_mlx_streaming"` for streaming Parakeet partials with a
whole-buffer final,
`"backend": "nemotron_offline"` for the older macOS NeMo CPU backend, and
`"mock"` only for plumbing tests.

## Quick troubleshooting

- Shortcut does nothing: check `hyprctl binds` on Wayland or
  `sunoto-daemon check` on X11.
- Release sometimes does not stop: make sure the no-modifier release bindings
  from `hypr/voice-dictation.conf` are installed.
- Text appears late: check logs for `inserted via Pasted` versus `Typed`.
- Text is not inserted: `ClipboardOnly` means focus changed; paste manually.
- `SecureField` in the log and "password field: nothing inserted" in the
  pill: keyboard focus was in a password box, so Sunoto typed nothing and
  put nothing on the clipboard. Move focus to a normal field and dictate
  again.
- Clipboard after a paste (macOS): the previous contents come back about
  300 ms later, images and files included, unless something else wrote to
  the clipboard in between. Set `"clipboard_restore": false` for an app that
  reads the pasteboard late. Dictated text is marked transient, so clipboard
  managers skip it.
- Empty result: check session `rms` in logs; low `rms` usually means wrong or
  muted microphone.
- Nemotron/GPU status: `bash bin/gpu-status.sh`.

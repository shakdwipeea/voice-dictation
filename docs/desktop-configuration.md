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

Dictation history (on by default):

```json
{
  "history_enabled": true,
  "history_retention_days": 90,
  "history_exclude_apps": ["com.1password.1password", "com.bitwarden.desktop"]
}
```

- `history_enabled`: keep a private record of each dictation on this
  machine, for finding mistakes and slow spots. On by default; `false`
  turns it off. Setup says so when it runs. Each dictation is one JSON
  line in a daily file under
  `~/Library/Application Support/sunoto/history/dictations/` (macOS) or
  `$XDG_DATA_HOME/sunoto/history/dictations/` (Linux, default
  `~/.local/share`). A line holds what the speech engine heard, each polish
  step, what was inserted and where, and the timings. It holds no audio.
  Nothing is uploaded. The folder and files are readable only by you.
  `SUNOTO_HISTORY_DIR` moves the folder.
- `history_retention_days`: day files older than this are deleted when the
  daemon starts and once a day. `0` keeps everything.
- `history_exclude_apps`: dictations into these apps are recorded without
  any text (bundle ids or app names on macOS, WM_CLASS names on X11). The
  default lists the common password managers. Password fields are always
  left out.
- `sunoto-daemon history list | show last | forget ID | clear --yes` reads
  and deletes entries. `sunoto-daemon history status` shows the folder and
  the counts.

Your words (macOS, `parakeet_mlx_offline`; on by default):

```json
{
  "word_list_enabled": true,
  "word_list": ["Kubernetes", "Priya"],
  "word_list_contacts": false,
  "word_list_documents": false
}
```

- `word_list_enabled`: spell names and tech words from around you right.
  When a dictation starts, the daemon gathers unusual words (ones the
  system's everyday English vocabulary does not know, rare real words like
  "hidradenitis" included) from the focused window (its text box or
  terminal screen and title, and the rest of the window when the box is
  nearly empty, as in a chat or a reply), the project you are in (Ghostty's
  working folder, a file's folder, or a project name in an editor's title),
  your project folders (`~/workspace`, `~/Developer`, `~/Projects`, `~/code`,
  …) and your apps. The speech model favours those words while it decodes.
  Afterwards a word one or two letters off a listed word ("Tsunoto", "Dun
  Lytics"), or that sounds like one ("Vanny" for vaani), and up to four
  words that spell one ("mini sign", "C G event tap") become the listed
  word. Everyday words, names the system knows ("William"), contractions
  and possessives are never changed. The words are used for that dictation
  only and never logged or saved. Reading takes tens of ms on its own
  thread while you talk; decoding time and memory stay the same. `false`
  turns it off. Folders macOS guards (Documents, Desktop, Downloads,
  iCloud Drive) are never touched unless `word_list_documents` is on.
- `word_list`: your own words to favour in every dictation, spelled as you
  want them written.
- `word_list_contacts` (off): also favour the names in Contacts. macOS asks
  once for Contacts access the first time the list is built with this on.
  In the persona benchmark this was the most useful source (+99 of 536
  names).
- `word_list_documents` (off): also favour the subfolder names of Documents
  and Desktop (clients, cases, courses) and the names of documents opened in
  the last two weeks (Spotlight). macOS asks once for Documents and Desktop
  access.
- `sunoto-daemon words` prints what a dictation would favour in the focused
  window now (`--delay 3` gives you time to switch windows).
- Benchmarks (`tools/vocab/`, stop the app first): `make_audio.py` +
  `bench_words.py` (your own names and history), `make_personas.py` +
  `bench_personas.py` (six kinds of users in eight voices; `make_noisy.py`
  adds background talk), `bench_biasing.py` + `rescore_biasing.py` (the
  LibriSpeech rare-word benchmark, facebookresearch/fbai-speech),
  `bench_speed.py` and `bench_silence.py`. Each takes `--engine
  qwen3-bf16|qwen3-8bit` to compare Qwen3-ASR (run with a venv that has
  mlx-audio, e.g. `build/venv-qwen3-asr`).

Updates (macOS release installs):

```json
{
  "auto_update": "install"
}
```

- `auto_update`: `"install"` (the default) checks GitHub a minute after
  launch and then daily. A newer release is downloaded and must carry a
  minisign signature from the release key built into the app. The signature
  also names the version, so an older release cannot pose as a newer one.
  The update installs after 10 minutes without dictation, with no panel open.
  A copy of the running version moves the app aside, runs the new app's
  `setup --update` (same signing identity, so permissions stay; the Login
  Item is left alone), and puts the old app back if the new one does not
  report ready. Versions that failed are not retried automatically.
  `"notify"` only logs that a release is out; `"off"` never checks.
  `sunoto update` installs the newest release on request (`--check` only
  reports). The menu bar's **Install Updates Automatically** switches the
  setting between `"install"` and `"off"` and takes effect at once (turning
  it on checks right away); **Check for Updates…** looks now and offers
  Install Now. Builds from a checkout never update themselves.
  Progress goes to `~/Library/Logs/sunoto/update.log`.
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

### The pill and the notch

On a MacBook with a camera notch the pill grows out of the notch, the way
the iPhone's Dynamic Island grows out of its camera. While you dictate, the
black notch grows a short pill under the camera with the record dot, your
voice and the timer in one row. Words (the start hint, a slow
"Transcribing…", what is loading) get a second row under it; a message or an
error sits in the row itself. After a paste a check mark appears in the
middle, right under the camera, and the island pulls back into the notch.
Nothing is drawn beside or under the camera, and the island is black in
light and dark mode alike.

On a screen without a notch (an external display as the main screen, a
closed lid, a Mac without a notch) the pill floats under the menu bar as
before. Sunoto picks one as the pill opens and keeps it for that dictation,
so it never jumps between the two mid-way.

`"overlay_style": "pill"` always floats the pill, for example if you run
another notch app (NotchNook, Boring Notch, Alcove) or prefer the floating
pill. The default is `"auto"`. Restart Sunoto after changing it. Linux
ignores the setting.

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

Setup downloads the speech model, Parakeet TDT 0.6B v2 (English), as
Sunoto's bf16 copy of `mlx-community/parakeet-tdt-0.6b-v2` (1.24 GB instead
of 2.5 GB, from the `model-parakeet-tdt-0.6b-v2-bf16` GitHub release,
checksums verified) into
`~/Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v2-bf16`, and
deletes what it downloaded for the earlier v3 model: its bf16 copy and, on
release installs, the original in Sunoto's own Hugging Face cache
(`~/Library/Application Support/sunoto/hf`). The shared
`~/.cache/huggingface` is never touched. parakeet-mlx casts the float32
original to bf16 on load, so the weights are identical. The app uses that
folder when it is there and the Hugging Face original otherwise; an install
that already has the original keeps it. `asr_model` overrides both (for
development; the app offers one model).

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

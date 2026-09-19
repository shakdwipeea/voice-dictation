# Sunoto Voice Dictation

Sunoto is a local, system-wide voice dictation app. Hold **Ctrl+F1**, speak,
and release—the transcript is inserted into the focused application.

- Local ASR: no cloud service or API key
- Push-to-talk: no silence timeout
- Streaming partial transcripts and a small status overlay
- Deterministic cleanup, with optional local LLM polishing
- macOS and Linux support

## macOS

### Install (Apple silicon, no developer tools needed)

Paste this in Terminal:

```bash
curl -fsSL https://raw.githubusercontent.com/shakdwipeea/voice-dictation/master/install-macos.sh | bash
```

It downloads the latest prebuilt `Sunoto.app` (the daemon, the overlay, and
the Python runtime are inside), verifies the checksum, installs it in
`~/Applications`, registers it as a Login Item, starts it, fetches the
speech model, offers the 2.7 GB polish model, and then watches the app's
own health until it is ready. Nothing needs `sudo`. To skip the question
about the polish model, append `-s -- --with-llm` or `-s -- --without-llm`.

Releases are built by [the release workflow](.github/workflows/release.yml)
from [packaging/macos/build-app.sh](packaging/macos/build-app.sh); a
Homebrew formula also exists in [packaging/homebrew](packaging/homebrew/README.md).

### Install from a checkout (developers)

From the repository root, with Rust, the Xcode command line tools, and the
Python runtimes set up:

```bash
bash install-macos.sh
```

This builds Sunoto, installs **Sunoto.app** in `~/Applications`, registers it
as a macOS Login Item, starts it, and then prints the app's own health report
until it is ready: hotkey verified, microphone capturing, speech model loaded.
An existing configuration is preserved.

The default backend is Parakeet-MLX on Apple Silicon. If the installer reports
that the ASR environment is missing, set it up once and rerun the installer:

```bash
brew install python@3.12 && bash services/asr/setup_macos_runtime.sh && .venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx && bash install-macos.sh
```

### Grant permissions once

Sunoto opens an onboarding panel that guides **Input Monitoring**,
**Accessibility**, and **Microphone** one at a time. Click the enabled
**Allow** button, grant that permission, then continue to the next row. When
all three say **Allowed**, click **Done**; Sunoto relaunches once and the
installer prints `Sunoto is ready`. Setup signs the app with a stable
per-machine identity, so later rebuilds and upgrades do not require another
grant.

Later, check on it any time with:

```bash
target/release/sunoto-daemon status
```

### Logs and restart

```bash
tail -f "$HOME/Library/Logs/sunoto/daemon.log"
open "$HOME/Applications/Sunoto.app"
```

`open` on the already-running app is a no-op; to restart, quit it first:

```bash
pkill -f "Sunoto.app/Contents/MacOS/sunoto-daemon"; open "$HOME/Applications/Sunoto.app"
```

The configuration file is:

```text
~/Library/Application Support/sunoto/config.json
```

See [macOS recurring issues](docs/macos-recurring-issues.md) if the hotkey,
permissions, microphone, or text insertion stops working.

## Linux

### Install and run

From the repository root:

```bash
bash install.sh
```

This builds Sunoto, creates the configuration, and installs and starts the
systemd user service. The initial `mock` backend verifies the complete flow
without loading a real ASR model.

To use Nemotron/CUDA ASR, set the backend in
`~/.config/sunoto/config.json`:

```json
{
  "backend": "nemotron"
}
```

Then restart the service:

```bash
systemctl --user restart voice-dictation.service
```

### Status and logs

```bash
systemctl --user status voice-dictation.service
journalctl --user -u voice-dictation.service -f
```

Wayland/Hyprland requires compositor keybindings and clipboard tools. X11 uses
the daemon's native global shortcut and XTEST insertion. See
[desktop configuration](docs/desktop-configuration.md) for the exact setup.

## Everyday use

1. Focus the application where text should appear.
2. Hold **Ctrl+F1**.
3. Speak while holding the keys.
4. Release to transcribe, polish, and insert the text.

Sunoto waits for its ASR sidecar to warm up after startup. Do not judge the
hotkey until the log says `ASR sidecar ready` or `Sunoto ready for dictation`.

## Useful commands

| Command | Purpose |
| --- | --- |
| `target/release/sunoto-daemon config show` | Show the active configuration. |
| `target/release/sunoto-daemon check` | Check platform integration, hotkey delivery, and sidecar startup. |
| `sunoto status` / `sunoto-daemon status` | Ask the running daemon for its health (hotkey, mic, model, polish). |
| `sunoto restart`, `sunoto log`, `sunoto uninstall` | macOS lifecycle: reopen the app, follow the log, remove it. |
| `sunoto setup --with-llm` | Download the polish model later if you skipped it. |
| `make test` | Run Rust and Python tests. |
| `bash scripts/macos-port/verify-all.sh` | Run the complete macOS verification gate. |
| `bash bin/gpu-status.sh` | Read Linux NVIDIA GPU and Xid health safely. |

## How it works

```text
hotkey → microphone capture → local ASR → text polish → focused application
```

The Rust daemon owns capture, hotkeys, sidecar IPC, and insertion. Python
sidecars provide ASR and optional local LLM polishing. Overlay writes use a
bounded background channel and never block the transcription latency path.

For backend selection, microphone settings, profiles, Wayland bindings, and
troubleshooting, use [desktop configuration](docs/desktop-configuration.md).
The macOS Login Item design is documented in
[macOS GUI Login Item plan](docs/macos-gui-login-item-plan.md).

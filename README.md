# Sunoto Voice Dictation

Sunoto is a local, system-wide voice dictation app. Hold **Ctrl+F1**, speak,
and release—the transcript is inserted into the focused application.

- Local ASR: no cloud service or API key
- Push-to-talk: no silence timeout
- Streaming partial transcripts and a small status overlay
- Deterministic cleanup, with optional local LLM polishing
- macOS and Linux support

## macOS

### Install and run

From the repository root:

```bash
bash install-macos.sh
```

This builds Sunoto, installs **Sunoto Login** in
`~/Applications`, registers it as a macOS Login Item, and starts it. It also
preserves an existing configuration.

The default backend is Parakeet-MLX on Apple Silicon. If the installer reports
that the ASR environment is missing, set it up once and rerun the installer:

```bash
brew install python@3.12 && bash services/asr/setup_macos_runtime.sh && .venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx && bash install-macos.sh
```

### Grant permissions once

In **System Settings → Privacy & Security**, grant both **Accessibility** and
**Input Monitoring** to:

- `~/Applications/Sunoto Login.app`
- `target/release/sunoto-daemon`

Allow **Microphone** access when macOS asks. Then open Sunoto again:

```bash
open "$HOME/Applications/Sunoto Login.app"
```

Wait for `Sunoto ready for dictation`, then hold **Ctrl+F1**, speak, and
release.

### Logs and restart

```bash
tail -f "$HOME/Library/Logs/sunoto/daemon.log"
open "$HOME/Applications/Sunoto Login.app"
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
| `target/release/sunoto-daemon check` | Check platform integration and sidecar startup. |
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

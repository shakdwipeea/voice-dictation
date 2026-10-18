# Sunoto

Local voice dictation for macOS and Linux. Tap **Right Option**, speak, and
tap it again: your words are pasted wherever the cursor is. Speech
recognition runs on your machine, with no cloud service or account.

## Install (macOS, Apple silicon)

```bash
curl -fsSL https://raw.githubusercontent.com/shakdwipeea/voice-dictation/master/install-macos.sh | bash
```

This installs **Sunoto.app** in `~/Applications`, starts it at login, and
downloads the English speech model (1.24 GB, once). A setup panel then asks for two
permissions: **Accessibility** (to notice the shortcut and paste) and
**Microphone**. Click **Allow** on each, then **Done**. No `sudo` needed.

Optional AI polish merges mid-sentence corrections ("Tuesday, no, Wednesday").
It is off by default because it adds about a second per dictation and a
2.7 GB download. To include it, append `-s -- --with-llm` to the command
above.

## Use

1. Click where the text should go.
2. Tap **Right Option** and speak.
3. Tap it again to paste. On macOS, **Esc** throws the recording away.

The microphone is on only while you dictate, and a recording stops on its
own after 5 minutes.

## Manage

```bash
alias sunoto="$HOME/Applications/Sunoto.app/Contents/MacOS/sunoto-daemon"
sunoto --version
sunoto status              # shortcut, microphone, speech model
sunoto restart
sunoto log                 # follow the log
sunoto setup --with-llm    # add AI polish later
sunoto update              # install the newest release now (--check only looks)
sunoto uninstall           # --purge also removes settings, models and history
sunoto history list        # recent dictations, if history is on
```

Sunoto keeps a history of your dictations: the text and timings of each
one, never audio, in `~/Library/Application Support/sunoto/history/` on
your Mac only. Password fields and password managers are left out, and
entries older than 90 days are deleted. `sunoto history clear --yes` removes
it all, and `"history_enabled": false` in the settings turns it off.

Sunoto updates itself. Once a day it checks for a new release, downloads it,
and checks its signature. It installs the update after 10 minutes without
dictation, and restarts in a few seconds with your settings and permissions
kept. If the new version does not start, the old one comes back.
`"auto_update": "notify"` in the settings only logs that an update is out,
and `"off"` stops the checks. The update log is
`~/Library/Logs/sunoto/update.log`.

Settings live in `~/Library/Application Support/sunoto/config.json`; for
example `"shortcut"` picks another key. Every option is described in
[desktop configuration](docs/desktop-configuration.md). If the shortcut,
permissions, microphone, or pasting stop working, see
[macOS recurring issues](docs/macos-recurring-issues.md).

## Linux

```bash
bash install.sh
```

This builds Sunoto and starts it as a systemd user service with a test
backend. For real speech recognition (Nemotron on CUDA), set
`"backend": "nemotron"` in `~/.config/sunoto/config.json` and run
`systemctl --user restart voice-dictation.service`. The shortcut is
**Ctrl+F1**. Wayland needs compositor key bindings; see
[desktop configuration](docs/desktop-configuration.md).

## Development

From a checkout, `bash install-macos.sh` builds Sunoto and installs it the
same way, keeping your settings. It needs Rust, the Xcode command line tools,
and the speech runtime, set up once with:

```bash
brew install python@3.12 && bash services/asr/setup_macos_runtime.sh
```

Run `make test` for the Rust and Python suites. To release, set `version` in
`Cargo.toml` and push a matching `vX.Y.Z` tag; [the release
workflow](.github/workflows/release.yml) builds and publishes the app.

- [AGENTS.md](AGENTS.md): conventions, verification, and macOS operations
- [Architecture](docs/architecture.md), and a plain-language version in
  [how Sunoto works](docs/how-sunoto-works.html)
- [Homebrew formula](packaging/homebrew/README.md)

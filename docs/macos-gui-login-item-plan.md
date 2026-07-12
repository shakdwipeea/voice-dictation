# Plan: reliable macOS GUI login item

## Problem

The existing `launchd` LaunchAgent starts Sunoto at login, but a background
agent has no responsible GUI process. macOS consequently disables its
`CGEventTap`, even when the bare daemon has Accessibility and Input Monitoring
permission. The old agent also hid a broken ASR virtual environment by falling
back to `python3`, which caused an endless sidecar restart loop after Homebrew
removed Python 3.12.

## Decision

Install a small, agent-style GUI application at
`~/Applications/Sunoto Login.app` and register it in the user's macOS Login
Items. Launch Services starts this application in the GUI session. Its only
job is to validate the configured daemon and ASR interpreter, establish the
repository root and log file, then launch the **bare**
`target/release/sunoto-daemon run` process and remain as its parent.

This deliberately keeps the daemon binary outside the app bundle:

- the bare binary's path-based TCC grant survives normal Rust rebuilds;
- the GUI launcher supplies the responsible-process context required by the
  event tap;
- the launcher stays alive and forwards termination, preserving the Launch
  Services responsibility chain that macOS consults for event-tap access;
- the daemon remains the single owner of hotkey, capture, sidecars, and
  insertion.

Because macOS considers both the event-tap client and its responsible GUI
application, the first installation requires Accessibility and Input
Monitoring grants for both `Sunoto Login.app` and the bare daemon. These grants
cannot be installed by a script. The installed launcher path remains stable so
the grant survives normal reinstalls.

## Implementation

1. Add the executable `services/macos/sunoto-login` launcher.
   - Read daemon, repository, log, and ASR interpreter paths from a generated,
     shell-quoted file in the installed app's `Contents/Resources` directory.
   - Refuse to start with an actionable log message if the daemon or configured
     ASR interpreter is missing or non-executable.
   - Redirect stdout/stderr to `~/Library/Logs/sunoto/daemon.log`, set
     `SUNOTO_ROOT`, change to the repository root, start the daemon, forward
     termination signals, and wait for it.
2. Change `install-macos.sh` to build and ad-hoc sign the launcher app, copy it
   to `~/Applications`, remove/unload the obsolete LaunchAgent, and create one
   idempotent macOS Login Item with System Events.
3. Grant permissions to both the bare daemon and responsible launcher. The
   launcher is an `LSUIElement` app, so it has no Dock icon or normal window.
4. Preflight the ASR venv during installation. For Parakeet backends, fail
   before registration when `.venv-nemotron-mac/bin/python` or its `mlx` /
   `parakeet_mlx` imports are unavailable.
5. Update the macOS phase verifier and operating documentation.

## Installation and lifecycle

```bash
bash install-macos.sh
open "$HOME/Applications/Sunoto Login.app"
tail -f "$HOME/Library/Logs/sunoto/daemon.log"
```

The installer is safe to rerun. It replaces the launcher bundle, removes any
older Sunoto Login item before adding exactly one current entry, and does not
rewrite an existing Sunoto configuration.

To disable automatic startup:

```bash
osascript -e 'tell application "System Events" to delete every login item whose name is "Sunoto Login"'
pkill -f 'target/release/sunoto-daemon run'
```

## Verification

Automated:

- launcher compiles and its generated bundle passes `plutil` and `codesign`;
- phase 7 verifier confirms the launcher source and Login Item installer;
- Rust build, clippy, workspace tests, and Python suites remain green;
- the installed daemon log reaches `ASR sidecar ready` without a respawn loop.

Manual (macOS TCC cannot be proven headlessly):

1. Confirm System Settings → General → Login Items contains **Sunoto Login**.
2. Log out and back in (or reboot).
3. Confirm one launcher parent, one daemon process, and `tap is_enabled=1` in
   the log.
4. Focus a disposable TextEdit document, hold Ctrl+F1, speak, and release.
5. Confirm the transcript is pasted into TextEdit.

## Failure recovery

- Missing Python after a Homebrew change: `brew install python@3.12`, then
  rerun `install-macos.sh`. Keep it pinned with `brew pin python@3.12`.
- Missing TCC grant: grant Accessibility and Input Monitoring to both
  `~/Applications/Sunoto Login.app` and `target/release/sunoto-daemon`, then
  quit/reopen the Login Item.
- Duplicate daemon: remove the legacy LaunchAgent and stop all daemon copies,
  then open the Login Item once. The installer performs this cleanup.

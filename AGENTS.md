# Agent guidance for voice-dictation

This repository builds a local, system-wide voice dictation daemon for Linux / Hyprland. Keep changes small, practical, and verified against the real daemon flow where possible.

## Project workflow

- Use `uv` for Python commands: `uv run ...` and `uv sync`.
- Prefer targeted checks before broader/manual testing:
  - `uv run python -m compileall -q src`
  - `uv run vd status --json`
  - `journalctl --user -u voice-dictation.service -n 80 --no-pager`
- After daemon code, service template, overlay, audio, or injection changes, restart the user service before judging behavior:
  - `systemctl --user restart voice-dictation.service`
- Clean generated caches before committing:
  - `rm -rf src/voice_dictation/__pycache__ .pytest_cache`

## Runtime safety

- Be careful with GPU-heavy verification. Use `bash bin/gpu-status.sh` before GPU-loading work if there is any concern about prior Xid events.
- Do not run stress tests such as gpu-burn or FurMark from this repo.
- The user-facing overlay should remain visual-first. Avoid adding transcript text, "done", or status words to the recording overlay unless explicitly requested.
- Do not paste or type test text into the user's focused application without making it clear and using a disposable target when possible.

## Audio and paste debugging

- If transcription repeats generic phrases such as "Thank you.", suspect low-signal audio or Whisper hallucination from near-silence before changing model settings.
- Check input devices with `uv run python - <<'PY' ... sounddevice.query_devices() ... PY`, `pactl list short sources`, and `wpctl status`.
- The overlay meter uses peak/RMS as the immediate source-of-truth for whether audio is being captured.
- Hyprland paste injection can report `ok` even when text is not inserted. Prefer testing against a disposable terminal/window instead of trusting `hyprctl` output alone.

## Style

- Keep the daemon simple: one long-running process, Unix socket IPC, GTK overlay on the main thread, worker threads for audio/VAD/STT.
- Avoid adding broad abstractions for one-off fixes.
- Keep dependencies minimal; if a new runtime dependency is useful, update `pyproject.toml`, `install.sh`, and `README.md` together.

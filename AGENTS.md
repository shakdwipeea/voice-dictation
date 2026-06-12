# Agent guidance for voice-dictation (sunoto)

This repository builds a local, system-wide voice dictation daemon for Linux.
A Rust daemon owns capture/hotkey/insertion; Python sidecars provide ASR
(NeMo Nemotron) and the GTK4 overlay UI. Keep changes small, practical, and
verified against the real daemon flow where possible.

## Project workflow

- Rust: `cargo test --workspace --offline` and
  `cargo clippy --workspace --offline --all-targets -- -D warnings` must both
  pass. `make test` runs everything (Rust + all Python suites).
- Python suites live in `tests/phase1`, `tests/phase2`, `tests/ui` (stdlib
  `unittest`; no uv/pytest). The overlay protocol tests run without GTK.
- Quick verification commands:
  - `target/debug/sunoto-daemon check` — X11/XTEST/shortcut/sidecar protocol
  - `make ui-demo` — drive the overlay pill by hand
  - `journalctl --user -u voice-dictation.service -n 80 --no-pager`
- After daemon, sidecar, overlay, or unit changes, restart the user service
  before judging behavior: `systemctl --user restart voice-dictation.service`

## Runtime safety

- Be careful with GPU-heavy verification. Use `bash bin/gpu-status.sh` before
  GPU-loading work if there is any concern about prior Xid events. The
  Nemotron sidecar takes 16–32 s to warm and holds ~3.6 GiB VRAM.
- NEVER start a second Nemotron instance (probe script, benchmark, manual
  sidecar) while voice-dictation.service is running — two instances plus the
  desktop can exhaust the 12 GiB card and crash the session. Stop the
  service first, or use the mock backend. The sidecar's VRAM preflight
  (`--min-free-vram-mib`, default 4500) refuses to load into a nearly-full
  GPU; do not disable it to "make a test work".
- Do not run stress tests such as gpu-burn or FurMark from this repo.
- The daemon types into whatever window is focused. Never trigger a dictation
  session (real or synthetic Ctrl+F8) without first focusing a disposable
  target window; treat injection as a security surface (see the product
  plan's injection-safety section).
- The recording overlay stays minimal while recording (dot + meter only);
  status text appears only for transcribing/partial/error states.

## Audio debugging

- The daemon logs per-session audio stats (duration, rms, peak). rms near 0
  means the source is wrong or muted — check `pactl list short sources`.
  Source auto-selection rejects monitor (speaker loopback) sources on purpose.
- Empty transcripts from the real backend usually mean near-silent input,
  not an ASR bug; the daemon logs the audio summary alongside.

## Style

- Match the existing architecture: one event loop in `apps/daemon`, typed
  NDJSON sidecar protocols in `sunoto-ipc`, X11 specifics in `sunoto-linux`,
  pure text transforms in `sunoto-polish`.
- UI must never block the latency path — overlay writes go through the
  bounded-channel writer thread; keep it that way.
- Avoid adding broad abstractions for one-off fixes; keep dependencies
  minimal. If a new runtime dependency is useful, update `pyproject.toml` /
  `Cargo.toml`, `install.sh`, and `README.md` together.

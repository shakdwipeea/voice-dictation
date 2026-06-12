# Integration Plan: sunoto Backend + voice-dictation Overlay UI

*June 12, 2026. The overall plan for the merged repository — what is done,
what comes next, and in what order. Product scope and phase gates live in
`product-plan.md`; this document covers the work created by the merge.*

## Where We Are

The `sunoto-backend` branch of `github.com/shakdwipeea/voice-dictation`
combines, with both commit histories preserved:

- **Kept from sunoto:** the Rust workspace (`apps/daemon`, `crates/*`), the
  Nemotron/mock ASR sidecars (`services/asr/`), the Phase 1/2 test suites,
  eval corpus, tools, and docs.
- **Kept from voice-dictation:** the GTK4 layer-shell pill overlay
  (`src/voice_dictation/overlay.py`), `hypr/` window rules, `systemd/` unit,
  `install.sh`, and the `bin/` GPU helpers.
- **Removed:** the upstream whisper/silero Python backend (pipeline, STT,
  segmenter, injector, daemon, CLI entry points), superseded by the Rust
  pipeline. `pyproject.toml` was trimmed to the overlay package.

Verified on the branch: `cargo check --workspace` clean; all Phase 1 (18)
and Phase 2 (3) Python tests pass.

## Workstream A: Overlay as a UI Sidecar

The overlay is currently orphaned — its upstream Python daemon was removed.
Wire it to `sunoto-daemon` using the same managed-sidecar pattern as the ASR
service (`sunoto-ipc`: spawn, NDJSON over stdin/stdout, restart with backoff).

1. **Driver entry point** (`src/voice_dictation/ui_sidecar.py`): runs the GTK
   main loop on the main thread (`build_and_run()` blocks, as required), and
   a reader thread that maps stdin NDJSON onto the overlay's thread-safe
   methods — they already marshal via `GLib.idle_add`:

   | message                                              | overlay call      |
   | ---------------------------------------------------- | ----------------- |
   | `{"op":"show"}` / `{"op":"hide"}`                     | `show` / `hide`   |
   | `{"op":"recording","elapsed":s,"peak":p,"rms":r,"segments":n}` | `set_recording` |
   | `{"op":"status","text":"transcribing"}`               | `set_status`      |
   | `{"op":"segment","text":...}` / `{"op":"clear"}`      | `add_segment` / `clear_segments` |
   | `{"op":"shutdown"}` or stdin EOF                      | `shutdown`        |

   Emit `{"event":"ready"}` on startup so the daemon knows the window exists
   (same handshake as the ASR sidecar).
2. **Daemon wiring:** a `ui` module behind a small trait so the daemon picks
   the backend at runtime — layer-shell sidecar under Wayland, the existing
   X11 override-redirect bubble under X11, headless with `--no-overlay`.
   Session start → `show` + periodic `recording` frames from the audio
   pipeline's existing level data; finalization → `status`/`hide`.
3. **Failure rules:** the UI sidecar is never on the latency path — daemon
   functions fully if it dies; restart with backoff, never block insertion.
4. **Testing:** protocol unit test against a fake stdin (mirror
   `tests/phase1/test_nemotron_sidecar_protocol.py`); visual pass deferred to
   a Wayland/Hyprland machine — gtk4-layer-shell is a Wayland protocol and
   cannot run on the current X11 dev box.

## Workstream B: Packaging and Docs Adaptation

`install.sh`, `systemd/voice-dictation.service`, and `README.md` still
describe the removed Python daemon.

1. systemd unit: `ExecStart` → the `sunoto-daemon` binary; keep the GPU
   ordering/env bits that still apply.
2. `install.sh`: build/install the Rust daemon (`cargo build --release`),
   install the Python pieces (ASR sidecar env, overlay package), keep the
   hypr config install for Wayland users.
3. README: rewrite around the merged architecture (Rust daemon + Nemotron
   sidecar + overlay UI); fold in the relevant parts of the upstream one.
4. `AGENTS.md` / `.claude/`: reconcile upstream agent docs with the sunoto
   layout so assistant tooling isn't following stale instructions.

## Workstream C: Upstream Collaboration

1. Open the PR: `sunoto-backend` → `master` on
   `shakdwipeea/voice-dictation` (branch is pushed; PR intentionally not yet
   opened). The PR description should state the architecture decision
   explicitly: Rust pipeline replaces the whisper Python backend, overlay UI
   is kept and becomes daemon-driven.
2. Review with Akash — in particular the backend removal, the trimmed
   `pyproject.toml`, and whether his Hyprland setup becomes the reference
   Wayland environment for Workstream A's visual pass (the current dev
   machine cannot exercise layer-shell).

## Workstream D: Outstanding Phase Work (unchanged by the merge)

From `product-plan.md` §13 — these need a human at the machine:

1. Manual five-app desktop compatibility pass + live-mic accuracy check to
   close Phase 1 (`make phase1-run-nemotron`).
2. Record the Phase 2 corpus (`make phase2-record` → `phase2-transcribe` →
   `phase2-eval-recorded`), then tune correction gates from real data.
3. Phase 3 UX (settings window, tray, setup flow) and warm-start reduction
   (16–32 s today vs the 10 s goal).

## Order

1. **C1 (open the PR)** — cheap, unblocks review while the rest proceeds.
2. **A (overlay sidecar)** — the merge's core promise; A1+A3+A4 are doable on
   this machine now, the visual pass lands wherever Wayland is available.
3. **B (packaging)** — after A settles the process model that the unit and
   installer must describe.
4. **D** — independent; interleave whenever a human is available at the
   machine.

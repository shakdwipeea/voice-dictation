---
name: macos-port
description: Drive the sunoto voice-dictation macOS port, phase by phase, in parallel where safe. Loads the port plan, runs the objective phase verifiers, dispatches independent phases to pi subagents, and runs the full integration gate until the app works on Mac. Use when working on the macOS port of this repo (CoreAudio/CGEventTap/CGEvent insertion/NSPasteboard/launchd, or the Nemotron-offline-MPS ASR backend).
---

# macOS Port Skill (sunoto)

This skill drives the macOS port of the voice-dictation daemon described in
`docs/macos-port-plan.md`. It is project-specific and lives in
`.agents/skills/macos-port/`.

## Mental model

- **Plan:** `docs/macos-port-plan.md` — the full design (ASR decision, platform
  mapping, 8 phases, sequencing).
- **Tracker:** `scripts/macos-port/phases.md` — single source of truth for
  phase status. Update your phase's row when its verifier passes.
- **Verifier (objective, no LLM needed):** `scripts/macos-port/verify-phase.sh [N...]`
  and `scripts/macos-port/verify-all.sh`. This is the **only** reliable way to
  know a phase works. Always trust the verifier over self-report.
- **ASR runtime:** `services/asr/setup_macos_runtime.sh` creates
  `.venv-nemotron-mac`; `services/asr/phase0_macos_measure.py` measures MPS
  latency. The offline sidecar is `services/asr/nemotron_offline_sidecar.py`.

## Ground rules (every worker, every phase)

1. Keep the **universal gate** green at all times on macOS:
   `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`, plus all Python suites
   (`python3 -m unittest discover -s tests/{phase0,phase1,phase2,ui}`).
   If cargo isn't on PATH, `. "$HOME/.cargo/env"`.
2. **Do not break Linux.** `crates/sunoto-linux` real X11 code stays under
   `cfg(target_os="linux")`; stubs only affect non-Linux. Never `#[link]` an
   X11/CUDA lib unconditionally.
3. Match the existing architecture: one event loop in `apps/daemon`, typed
   NDJSON in `sunoto-ipc`, platform specifics in `sunoto-macos` (re-exported
   via the `sunoto-desktop` facade), pure transforms in `sunoto-polish`.
   The UI must never block the latency path.
4. Runtime safety still applies on macOS where relevant: the daemon types into
   the focused window — never trigger a real dictation session without a
   disposable target focused. No CUDA/GPU concerns on Mac, but the Nemotron
   sidecar is heavy; don't spawn a second ASR instance while one is running.
5. **Verify before claiming done.** Run `scripts/macos-port/verify-phase.sh N`
   for your phase; only flip the tracker to ✅ when it passes. If a check is
   `⚠ manual` (a live TCC permission test), note it in the tracker and give
   the user the exact command to run.

## How to verify a phase

```bash
# one or more phases
bash scripts/macos-port/verify-phase.sh 1
bash scripts/macos-port/verify-phase.sh 2 3 4
# everything (the final integration gate)
bash scripts/macos-port/verify-all.sh
```

Exit code 0 ⇒ all requested phases pass their objective checks. A `✗` is a
real failure (fix it); a `⚠ manual` needs a live GUI/TCC step the script
can't perform headlessly — surface it to the user with the exact command.

## macOS TCC permissions (phases 2, 3, 4)

Live hotkey/capture/insertion need system permissions granted once:
- **Accessibility** — `CGEventTap` hotkey + `CGEvent` insertion (System
  Settings → Privacy & Security → Accessibility).
- **Input Monitoring** — event tap key monitoring.
- **Microphone** — CoreAudio capture (first capture triggers the prompt).

The verifier flags these as `manual`. When implementing, log actionable
guidance if a tap/insertion/capture fails for permission reasons (mirror how
the Linux path logs X11/`parec` failures).

## Working phases in parallel

pi can run independent phases concurrently via the **subagent** extension
(spawned `pi --mode json -p --no-session` subprocesses, isolated context).
If the subagent extension is installed, dispatch like:

```
Run these in parallel with subagent (agentScope: both, cwd: <repo>):
 - worker: Implement macOS port Phase 6 (overlay) per docs/macos-port-plan.md. Verify with scripts/macos-port/verify-phase.sh 6.
 - worker: Implement macOS port Phase 7 (launchd + install-macos.sh) per docs/macos-port-plan.md. Verify with scripts/macos-port/verify-phase.sh 7.
 - worker: Implement macOS port Phase 8 (docs) per docs/macos-port-plan.md. Verify with scripts/macos-port/verify-phase.sh 8.
```

**Parallelization rules (see `scripts/macos-port/phases.md`):**
- Phases 2, 3, 4 all edit `crates/sunoto-macos` FFI and overlap → run them as
  a **chain on one worker** (or split strictly by file: `capture.rs`,
  `hotkey.rs`, `insertion.rs` with careful merge). Do **not** run them
  parallel against the same files.
- Phases 6 (overlay), 7 (launchd/install), 8 (docs) are independent of the
  `sunoto-macos` FFI → safe to run **in parallel** with each other and with
  the 2/3/4 chain.
- Max 8 tasks / 4 concurrent per the subagent extension.

If the subagent extension is **not** installed, work phases sequentially in
the current session — the verifier + tracker still keep things honest. To
enable it, see the `subagent` example under pi's
`examples/extensions/subagent/` (symlink into `~/.pi/agent/extensions/`).

## Per-phase implementation pointers

- **Phase 2 (CoreAudio):** 16 kHz mono s16le frames → `AudioEvent::Frame`,
  feeding `sunoto-core` preroll unchanged. Add a `cfg(target_os="macos")`
  branch in `sunoto-audio`; keep the Linux `parec`/`pactl` path under
  `cfg(target_os="linux")`. Reuse the `CaptureConfig`/`AudioEvent` interface.
- **Phase 3 (CGEventTap):** system-wide tap filtered to the configured
  shortcut; map `Shortcut` modifiers → `CGEventFlags`. Add a `Cmd` modifier
  to `Shortcut::parse` (keep `Ctrl` working); default macOS shortcut
  `Cmd+F1` (config-overridable). Log guidance if the tap can't be created.
- **Phase 4 (insertion):** `CGEventCreateKeyboardEvent` +
  `CGEventSetUnicodeString` per char at `kCGHIDEventTap`; newlines → Return
  only when `allow_enter_and_tab` (reuse `sanitize_for_insertion`). Focus
  token + app id via `NSWorkspace.frontmostApplication` + `AXUIElement`;
  revalidate focus between release and insert (reuse
  `InsertionOutcome::ClipboardOnly` on focus change). Clipboard via
  `NSPasteboard` (or `pbcopy`). Neutralize held modifiers around injection.
- **Phase 6 (overlay):** native `NSPanel` pill (`.floating`, non-activating)
  driven by the existing `OverlayRequest` NDJSON protocol, spawned by the
  daemon like the GTK `ui_sidecar.py`. Keep overlay-failure non-fatal.
- **Phase 7 (launchd):** `services/macos/*.plist` (`RunAtLoad`, `KeepAlive`,
  `ProgramArguments` = `sunoto-daemon run`, env `SUNOTO_ROOT`, logs to
  `~/Library/Logs/sunoto/`). `install-macos.sh` builds, writes config
  (macOS path `~/Library/Application Support/sunoto/config.json`), installs
  the plist to `~/Library/LaunchAgents/`, `launchctl load`, prints the three
  TCC prompts. Add macOS paths to `settings.rs`.
- **Phase 8 (docs):** README + AGENTS.md + `docs/desktop-configuration.md`
  macOS section; note offline/no-partials behavior and TCC permissions.

## End-of-port full testing (the goal)

When all phases claim done:

1. Run `bash scripts/macos-port/verify-all.sh` — must exit 0 (every objective
   check green).
2. Walk the `⚠ manual` items with the user on a live macOS GUI session:
   - grant Accessibility, Input Monitoring, Microphone;
   - `install-macos.sh` then `launchctl` start the service;
   - hold the hotkey, speak a short phrase into TextEdit, release → clean
     text appears at the cursor via the `nemotron_offline` (MPS) sidecar.
3. Fix anything that fails; re-run `verify-all.sh`; iterate **until the app
   works as expected on Mac**. Update `scripts/macos-port/phases.md` to ✅
   per phase as the verifier passes.

Do not declare the port complete until `verify-all.sh` is green and the live
push-to-talk → text-in-TextEdit loop works with the user.

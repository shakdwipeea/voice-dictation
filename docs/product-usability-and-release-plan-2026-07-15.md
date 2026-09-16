# Sunoto Product Usability and Release Plan

**Date:** July 15, 2026  
**Status:** Proposed execution plan  
**Scope:** Turn the existing local dictation engine into an installable,
observable, safe, and dependable desktop product for macOS and Linux.

## 1. Product Direction

Sunoto is a local-first, system-wide voice dictation application. The core
interaction remains deliberately simple:

1. Focus a writable field in any application.
2. Hold the configured push-to-talk shortcut.
3. Speak naturally.
4. Release the shortcut.
5. Sunoto transcribes, polishes, and inserts the result at the cursor.

The product should feel like a normal desktop utility. A user should not need
to understand Rust, Python environments, model backends, TCC, systemd,
launchd, control sockets, or daemon logs to install or use it.

### Product principles

- **Local by default:** captured audio, transcripts, and polish remain on the
  device after required models have been downloaded.
- **Polished by default:** local LLM polish is enabled by default and is part
  of the standard Sunoto experience.
- **Dictation never depends on optional recovery work:** if LLM polish is
  warming, unavailable, or unhealthy, Sunoto immediately falls back to the
  deterministic polish path instead of rejecting dictation.
- **Truthful status:** Sunoto says it is ready only when the hotkey,
  microphone, ASR, and insertion path required for dictation are healthy.
- **Safe insertion:** Sunoto must not insert into the wrong window, execute
  dictated control characters unexpectedly, or silently expose sensitive
  text.
- **No terminal required:** installation, permissions, model setup, settings,
  diagnostics, updates, and removal must be accessible through product UI.

## 2. Product Modes

Sunoto should expose three text-processing modes. These modes affect text
cleanup only; all three use the same push-to-talk capture and ASR pipeline.

| Mode | Behavior | Default |
| --- | --- | --- |
| **Polish** | ASR, deterministic cleanup, then constrained local LLM polish | **Yes** |
| **Fast** | ASR and deterministic cleanup only | No; automatic fallback when LLM polish is unavailable |
| **Raw** | ASR output with only safety-critical normalization | No |

Polish mode must visibly report whether LLM polish is active or whether a
particular dictation used the Fast fallback. The fallback should preserve user
flow and should not display a blocking error.

## 3. Current Product Assessment

The repository already contains a strong engine:

- A single Rust daemon owns capture, hotkeys, sidecar IPC, polishing, and
  insertion.
- macOS and Linux platform adapters exist.
- ASR sidecars are persistent and stream partial transcripts.
- The overlay uses a bounded writer channel and cannot block the latency path.
- Focus is captured at shortcut release and checked before final insertion.
- Enter and Tab are neutralized unless explicitly enabled.
- Deterministic cleanup, dictionary entries, snippets, and application styles
  are implemented in the core polish library.
- Linux measurements have demonstrated release-to-insertion latency well
  inside the existing target.

The remaining gap is primarily productization. Installation, permissions,
health reporting, settings, privacy controls, compatibility verification, and
release packaging are not yet at consumer-product quality.

### Known release blockers

1. macOS can announce `Sunoto ready for dictation` when the ASR is ready but
   the physical hotkey event tap is disabled.
2. macOS installation still builds from the repository and launches binaries
   through paths inside the working tree.
3. Setup requires command-line tools and manual runtime installation.
4. There is no settings window or persistent menu-bar/tray status surface.
5. Normal daemon logs contain complete transcript and polish text.
6. Clipboard restoration is not consistent across macOS, Wayland, and X11.
7. Secure/password-field detection is not implemented.
8. The macOS live permission, capture, hotkey, insertion, overlay, and login
   checks are not all automated and verified as one release gate.
9. The Phase 1 Python suite currently has a disagreement between the
   constrained LLM prompt and its filler-removal test.
10. Planning documents and the phase tracker contain stale status and default
    descriptions.

## 4. Release-Blocking Workstreams

### 4.1 Truthful health and recovery

Create a single health model covering:

- daemon lifecycle;
- duplicate-daemon detection;
- microphone permission and active capture;
- physical hotkey event delivery or compositor binding;
- ASR model download, loading, ready, crashed, and restarting states;
- LLM model download, loading, active, degraded, and restarting states;
- insertion/Accessibility permission;
- overlay availability; and
- focused-target compatibility where it can be detected.

The user-facing states should include at least:

- Starting
- Downloading model
- Warming ASR
- Warming Polish
- Ready — Polish active
- Ready — Fast fallback
- Permission required
- Microphone unavailable
- Shortcut unavailable
- Model unavailable
- Restarting
- Disabled

Every unhealthy state must offer an actionable explanation and a direct
repair action where the operating system permits one. Repeated low-level
diagnostic messages should remain in debug logs and must not flood normal
logs.

**Acceptance gate:** Sunoto never reports Ready when a real keyboard press
cannot start recording. Revoking each permission produces the correct state
and repair guidance without requiring log inspection.

### 4.2 Standalone installation and lifecycle

#### macOS

- Produce one signed and notarized application distributed through a DMG or
  PKG.
- Give the user one stable application identity for TCC permissions.
- Embed the daemon and overlay or install them into stable, versioned paths.
- Provide the supported Python runtime and dependencies without requiring a
  user-managed virtual environment.
- Download required ASR and LLM models through the application.
- Verify model checksums and available disk space.
- Register login startup through the supported GUI-context mechanism.
- Provide update, rollback, uninstall, and model-removal flows.

#### Linux

- Produce a `.deb` for the first supported Ubuntu/Mint targets.
- Produce an AppImage where global integration constraints permit it.
- Install and manage the systemd user service automatically.
- Detect X11, Hyprland, and unsupported Wayland sessions and explain the
  resulting capabilities before installation finishes.
- Install or clearly resolve required desktop integration dependencies.

**Acceptance gate:** a non-developer can install Sunoto on a clean supported
machine, complete onboarding, and dictate successfully within five minutes
without opening a terminal.

### 4.3 Default-on LLM polish

LLM polish remains enabled by default. Productizing this decision requires:

- bundling or automatically downloading the selected supported model;
- showing model size and download progress;
- warming ASR and LLM components concurrently where GPU contention permits;
- allowing dictation during LLM warmup through deterministic Fast fallback;
- keeping the constrained edit contract conservative;
- preserving names, numbers, URLs, code, and protected dictionary terms;
- falling back to deterministic output on timeout, malformed output, model
  crash, or safety-validation failure;
- exposing Polish, Fast, and Raw modes in settings;
- making LLM keepalive behavior power-aware; and
- measuring idle CPU, memory pressure, energy use, warmup time, p50/p95
  polish latency, and fallback frequency on supported hardware.

Keepalive may remain enabled by default for Polish mode, but its interval and
strategy must meet an explicit idle-resource budget. It should pause while
Sunoto is disabled, during system sleep, under serious thermal pressure, and
when the user selects Fast or Raw mode.

**Acceptance gates:**

- A missing or failed LLM never prevents dictation.
- Clean transcripts are not made worse by the LLM quality gate.
- Names, numbers, URLs, and protected terms survive unchanged.
- The UI distinguishes active LLM polish from deterministic fallback.
- Idle resource targets are measured and documented for every supported
  hardware tier before public release.

### 4.4 Privacy and insertion safety

- Redact transcript and polish contents from normal logs.
- Add an explicit, time-limited diagnostic-content logging option with a
  visible privacy warning.
- Restore the previous clipboard after a successful paste on every platform
  where the clipboard protocol permits reliable restoration.
- Clearly notify the user when the result must remain on the clipboard.
- Detect password and secure text fields where platform APIs expose them and
  block automatic insertion.
- Add an optional sensitive-application denylist.
- Keep Enter and Tab disabled by default.
- Preserve focus revalidation immediately before insertion.
- Document clipboard-manager exposure.
- Provide one action to remove local history, diagnostic logs, downloaded
  models, and configuration.
- Verify that runtime dictation makes no network request after model setup.

**Acceptance gate:** a privacy test demonstrates that normal logs contain no
dictated content, secure fields reject insertion, focus changes cannot redirect
text, and clipboard behavior matches the documented platform contract.

### 4.5 Quality and compatibility gate

- Resolve the current LLM filler-removal prompt/test disagreement based on the
  intended division between deterministic and LLM polish.
- Keep Rust build, clippy, Rust tests, and all Python suites green.
- Add a repeatable live end-to-end checklist for each supported platform.
- Test repeated dictation, first/last-word retention, Unicode, long
  utterances, rapid consecutive sessions, sleep/wake, microphone changes,
  permission revocation, sidecar crash, model timeout, and focus changes.
- Test at least five representative applications per supported desktop.
- Add at least one non-US keyboard layout to the insertion matrix.
- Record accuracy and latency on live speech rather than relying exclusively
  on scripted transcript inputs.

Initial application matrix:

| Category | macOS | Linux |
| --- | --- | --- |
| Native editor | TextEdit, Notes | xed or another GTK editor |
| Browser | Safari, Chrome/Firefox | Firefox, Chromium |
| Electron | VS Code, Slack | VS Code, Slack-compatible client |
| Office/editor | Pages or LibreOffice | LibreOffice Writer |
| Terminal | Terminal, iTerm2 | GNOME Terminal, Kitty or equivalent |

**Acceptance gate:** the complete supported-platform matrix passes from a
fresh install, and the release verifier has no test failures or undocumented
manual gaps.

## 5. Usable Beta Product Shell

After the release blockers are addressed, build the permanent user-facing
surface.

### 5.1 Menu-bar/tray application

The menu should provide:

- current health and active processing mode;
- Enable/Disable;
- Open Settings;
- microphone selection;
- shortcut display;
- model download/warmup progress;
- Restart Sunoto;
- Run Diagnostics;
- Open Help;
- Quit.

### 5.2 Guided onboarding

The first-run flow should:

1. Explain local processing and model storage.
2. Check hardware and disk requirements.
3. Download and verify the selected ASR and polish models.
4. Request microphone, hotkey/Input Monitoring, and insertion/Accessibility
   permissions one at a time.
5. Let the user choose and test a microphone.
6. Let the user record or confirm the shortcut.
7. Provide a safe built-in practice text field.
8. Finish only after a real push-to-talk transcription succeeds.

### 5.3 Settings

Settings should expose user concepts rather than backend implementation names:

- shortcut;
- microphone;
- Polish, Fast, or Raw mode;
- startup at login;
- overlay enablement and position;
- personal dictionary;
- snippets;
- filler-removal preferences;
- language, initially showing English as the supported language;
- model storage and removal;
- privacy and diagnostic logging;
- optional local history and retention period;
- advanced hardware/performance profile.

Raw backend names, Python paths, watchdog values, and model paths should live
under an Advanced or Developer section, if exposed at all.

### 5.4 Session feedback

The overlay should communicate only what helps during the active interaction:

- Recording: dot and microphone meter.
- Transcribing: short status and stable partials where available.
- Polishing: brief state without blocking the desktop.
- Fast fallback: subtle non-blocking indication.
- Success: optional short confirmation before hiding.
- Failure: actionable message that persists long enough to understand.
- Clipboard-only result: explicit instruction to paste manually.

## 6. Public Beta Readiness

Before a public beta, add:

- automated build and release pipelines;
- signed artifacts and checksum publication;
- crash-loop detection and safe recovery;
- configuration and model migration across upgrades;
- rollback to the previous working version;
- an uninstall flow that can optionally retain or delete models and settings;
- a maintained supported-hardware and desktop compatibility page;
- concise privacy documentation;
- first-run and update analytics only if explicitly opted in; and
- a structured local diagnostics export with transcripts excluded by default.

The repository should also contain one current roadmap and status tracker.
Superseded phase notes may remain as historical evidence, but they should link
to the current plan and must not present stale defaults as current behavior.

## 7. Delivery Sequence

### Milestone A — Reliability alpha

- Truthful health state and repair guidance
- Non-blocking default-on LLM polish with Fast fallback
- Transcript-safe logging
- Clipboard and secure-field safety work
- Green automated verification gate
- Live macOS and Linux compatibility passes

**Exit:** developers can use Sunoto daily without reading logs or restarting it
manually, and failure states are self-explanatory.

### Milestone B — Closed beta

- Standalone signed macOS installer
- Linux package for the first supported distribution
- Menu-bar/tray application
- Guided onboarding
- Settings for the core user-facing controls
- Automatic model management

**Exit:** invited non-technical users can install, configure, use, update, and
remove Sunoto without terminal assistance.

### Milestone C — Public beta

- Release automation
- Update, rollback, and migration
- Broader hardware and application compatibility matrix
- Privacy and support documentation
- Diagnostics export
- Performance and quality dashboards generated from release tests

**Exit:** installation success, first-dictation success, crash-free use,
latency, quality, and resource use meet documented beta targets.

### Milestone D — Product differentiation

- Improved application-specific writing styles
- Dictionary and snippet ergonomics
- Developer-aware dictation
- Additional languages
- Optional local history and scratchpad
- Carefully scoped voice-command capabilities after a separate design and
  safety review

## 8. Product Success Measures

The release process should track at least:

- installation completion rate;
- first successful dictation rate;
- median time from launch to Ready;
- shortcut-to-recording feedback latency;
- release-to-insertion p50, p95, and p99;
- LLM polish p50 and p95;
- percentage of sessions using Fast fallback;
- zero-edit rate on the maintained speech corpus;
- protected-term, digit, URL, and code-token preservation;
- insertion success by application and desktop;
- idle CPU, memory, GPU activity, and energy impact;
- sidecar crash and automatic recovery rate;
- permission-related failure rate; and
- clipboard-only fallback rate.

Metrics derived from real user content must remain local unless the user
explicitly chooses to share a diagnostics package.

## 9. Explicit Non-Goals for This Plan

This plan does not include accounts, cloud sync, team collaboration, mobile
applications, meeting transcription, or a general voice assistant.

### Deferred separate design: System mode

A possible **System mode** would interpret explicit spoken commands such as
“open Chrome” and perform operating-system actions instead of inserting text.
This is intentionally not specified or scheduled in this document. It changes
Sunoto from a dictation tool into an action-execution surface and therefore
needs a separate discussion covering activation, command boundaries,
confirmation, permissions, allowlists, destructive actions, auditability, and
platform support before implementation begins.

Until that design is agreed, normal dictation must always be treated as text
and must never be executed as an operating-system command.

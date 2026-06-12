# Sunoto: Local-First Voice Dictation for Linux

## 1. Product Definition

Sunoto is a Linux-native, local-first voice input application inspired by the
core workflow of Wispr Flow:

1. Hold a global shortcut.
2. Speak naturally into any application.
3. Release the shortcut.
4. Receive clean, context-aware text at the current cursor.

The product is not just speech-to-text. Its value comes from the full pipeline:

- Fast global activation
- Accurate speech recognition
- Removal of fillers and abandoned phrases
- Recognition of mid-sentence corrections
- Punctuation and structural formatting
- Personal vocabulary and reusable snippets
- App-aware writing styles
- Reliable insertion into text fields across Linux applications
- Local processing and local storage by default

As of June 11, 2026, Wispr Flow officially supports Mac, Windows, iPhone, and
Android, but not a native Linux desktop application. Linux is therefore the
platform gap Sunoto should target.

## 2. What Wispr Flow Does

The important product behaviors to reproduce are:

### Core Dictation

- Dictation into any focused text field
- Push-to-talk and hands-free modes
- Automatic language detection
- Automatic punctuation
- Filler-word removal
- Backtracking, such as converting "Tuesday, actually Wednesday" to
  "Wednesday"
- Automatic formatting of paragraphs, lists, emails, and prompts
- A small recording/transcription status bubble

### Personalization

- Personal dictionary for names, product terms, and jargon
- Voice-triggered snippets that expand into predefined text
- Different writing styles based on the active application
- Use of nearby text as context for spelling and formatting

### Voice Commands

- Transform selected text using a spoken instruction
- Generate text at the cursor using a spoken instruction
- Optional commands such as "press enter"

### Later-Stage Features

- Dictation history and scratchpad
- Developer-aware formatting for variables, file names, and CLI commands
- Shared dictionaries and snippets
- Usage statistics

## 3. Key Technical Finding

`nvidia/nemotron-speech-streaming-en-0.6b` is the primary ASR model. It is a
600M-parameter Cache-Aware FastConformer-RNNT built specifically for streaming
English speech recognition. Its cache-aware encoder processes only new audio
frames while retaining context from previous frames, which makes it a better
architectural fit for low-latency dictation than an offline-first model.

Nemotron is still only the speech recognition layer. It does not by itself
provide all of the cleanup, personalization, application context, or text
insertion behaviors that make this product useful.

Because speed is the primary product requirement, the initial model strategy
should be:

- **Live ASR default:** Nemotron Speech Streaming 0.6B, permanently loaded on
  the GPU
- **Default streaming profile:** 160 ms chunks for the initial
  latency-versus-accuracy balance using `att_context_size=[70,1]`
- **Ultra-fast profile:** 80 ms chunks for the lowest possible partial-result
  latency using `att_context_size=[70,0]`
- **Accuracy profile:** 560 ms chunks for users who accept higher latency using
  `att_context_size=[70,6]`
- **Fast cleanup:** deterministic rules with a strict 20 ms processing budget
- **Optional deep polish:** a local instruction model that runs only on demand
  or after the fast result has already been inserted
- **Non-NVIDIA fallback ASR:** a replaceable backend for unsupported or
  unavailable GPU environments

Nemotron Speech Streaming is English-only and has native punctuation and
capitalization. Its supported runtime chunk sizes are 80, 160, 560, and
1120 ms. NVIDIA's published average WER is 8.43% at 80 ms, 7.67% at 160 ms,
7.07% at 560 ms, and 6.93% at 1120 ms, demonstrating the expected
latency-versus-accuracy tradeoff.

The model runs through NVIDIA NeMo 25.11 or Riva 2.25.0 and supports Linux and
NVIDIA Ampere GPUs. The RTX 3060 in the current development machine is Ampere,
so it is architecturally supported. The exact chunk profile must still be
selected using measured p95 latency and dictation accuracy on this machine.

The current development machine has:

- Linux Mint 22.2 with Cinnamon on X11
- Intel i7-8700K, 6 cores / 12 threads
- 32 GiB RAM
- An NVIDIA RTX 3060 detected on PCI

However, `nvidia-smi` cannot currently communicate with the NVIDIA driver.
Local streaming Nemotron must not be assumed viable until the NVIDIA driver and
NeMo runtime are working and benchmarked.

## 4. Recommended Architecture

```text
Global shortcut
      |
      v
Linux integration daemon
  - focused app and selection
  - microphone capture
  - status bubble
      |
      v
Audio pipeline
  - continuously open microphone stream
  - short pre-roll buffer (a plain ring buffer; no idle DSP)
  - resample to model format
  - low-cost VAD (active sessions and hands-free mode only)
      |
      v
ASR backend interface
  - always-warm Nemotron cache-aware RNNT streaming
  - configurable 80/160/560 ms streaming profile
  - non-NVIDIA fallback backend
      |
      v
Fast path, maximum 20 ms
  - dictionary and snippets
  - deterministic cleanup
  - app-specific style
      |
      v
Text insertion adapter
  - X11
  - Wayland / portal / AT-SPI
  - clipboard fallback

Optional slow path after insertion
  - local text LLM polish
  - selected-text command mode
```

### Application Stack

- **Core daemon and Linux integration:** Rust
- **Desktop UI:** GTK4 with a small settings window, tray indicator, and
  always-on-top dictation bubble (Phase 1 ships an X11 override-redirect
  status bubble; GTK4 arrives with the Phase 3 settings UI). *(Amended
  June 12, 2026: the project merged with the upstream `voice-dictation`
  repository and adopts its GTK4 layer-shell pill overlay —
  `src/voice_dictation/overlay.py`, Python/PyGObject, a recording dot plus
  thin level meter with a thread-safe interface — as the dictation bubble.
  Layer-shell is a Wayland-only protocol, so the overlay selects its
  anchoring backend at runtime: gtk4-layer-shell where supported (Hyprland
  is its proven home), EWMH window hints on X11 — always-above, sticky,
  unfocusable notification window pinned top-center — so the same GTK pill
  runs on both session types. The native X11 override-redirect bubble
  remains as a no-GTK fallback. The Rust daemon drives the overlay as a UI
  sidecar over stdin-JSON, the same managed-process pattern as the ASR
  service.)*
- **ASR service:** Python sidecar using NVIDIA NeMo cache-aware streaming
  inference
- **Local text model:** llama.cpp-compatible local model behind a provider
  interface
- **IPC:** typed newline-delimited JSON over the managed sidecar's
  stdin/stdout. *(Amended June 12, 2026: originally "Unix domain socket";
  child-process pipes give the same typed protocol with the connection tied
  to the process lifetime and no socket-file management. The contract must
  support unsolicited sidecar-initiated events — streamed partials — not
  one-reply-per-request pairing.)*
- **Storage:** SQLite for settings, dictionary, snippets, and optional history
  (Phase 1 uses a JSON settings file; SQLite arrives with history)
- **Audio capture:** PulseAudio-protocol capture, which also works under
  PipeWire's pulse compatibility layer as shipped by Mint 22.2. *(Amended
  June 12, 2026 from Phase 0 findings: source auto-selection must reject
  monitor sources — the OS default on the target machine is the speaker
  monitor — and capture must request ~20 ms latency buffer attributes,
  because default PulseAudio buffering adds roughly two seconds, which alone
  destroys the latency gate. Device hot-plug is handled by respawning
  capture with backoff and re-resolving the source.)*
- **Packaging:** `.deb` and AppImage first; Flatpak later because sandboxing
  complicates global input and text insertion

Keeping inference in a separate process isolates CUDA/Python failures from the
desktop daemon and allows model backends to be replaced independently.

### Latency-First Execution Rules

- Load the ASR model during application startup and never load it on hotkey
  activation.
- Keep the microphone stream open while Sunoto is enabled, but retain only a
  short in-memory pre-roll buffer until the shortcut is pressed.
- Capture audio continuously in small frames and feed complete 80 or 160 ms
  chunks into Nemotron while the user speaks.
- Preserve Nemotron's encoder caches for the duration of each dictation and
  reset them immediately after finalization.
- Treat push-to-talk release as an explicit end-of-utterance signal. Do not wait
  for VAD silence after release.
- On release, immediately pad and flush any residual audio shorter than the
  configured chunk instead of waiting for the next chunk boundary.
- Keep audio resampling, IPC, cleanup, and insertion off the UI thread.
- Reuse the inference process and allocated GPU memory across dictations.
- Never run a general-purpose LLM before the first usable text is inserted.
- Show partial text in the dictation bubble, but insert only the stable final
  result in the MVP.
- Fall back immediately when the GPU backend is unhealthy rather than waiting
  through repeated timeouts.

## 5. Linux Integration Strategy

Linux desktop integration is the largest product risk. X11 and Wayland require
different implementations.

### X11: First Supported Target

The first release should target the current Linux Mint Cinnamon X11 system.

- Register global push-to-talk shortcuts with X11
- Detect the active application and window
- Insert text through direct accessibility APIs where possible
- Fall back to clipboard plus simulated paste
- Preserve and restore clipboard contents
- Provide an undo-friendly insertion operation

This is the fastest path to a complete end-to-end product.

### Injection Safety (added June 12, 2026)

Sunoto synthesizes keystrokes into whatever window is focused, which makes
text injection a security surface, not just a convenience:

- **Control characters never pass through by default.** Enter or Tab landing
  in a focused terminal or form executes commands or submits data; dictated
  text, snippet expansions, and any future "press enter" command must strip
  or neutralize control characters unless the user explicitly enables them.
- **Focus is captured at shortcut release and revalidated immediately before
  insertion.** If focus changed in between, the result goes to the clipboard
  with a visible notice instead of being typed into the wrong window.
- **Modifier hygiene.** Physically held modifiers (the push-to-talk Ctrl
  itself) are logically released around injection so dictated characters
  cannot become Ctrl/Alt chords, and are restored afterwards.
- **Clipboard exposure.** The clipboard fallback necessarily places dictated
  text — potentially secrets — where clipboard managers may persist it. The
  previous clipboard content is restored after pasting, but third-party
  clipboard history is outside Sunoto's control; documentation must say so,
  and password-field detection (where available) must disable insertion.

### Keyboard-Layout Dependence (added June 12, 2026)

XTEST injection resolves characters through the active keymap. The Phase 1
implementation maps US-layout keysyms; on other layouts unmappable characters
automatically fall back to clipboard-paste insertion, which is
layout-independent. Full keymap-aware resolution (spare-keycode remapping as
xdotool does, MappingNotify handling, per-app paste chords) is required
before non-US layouts are supported; the desktop compatibility matrix gains a
non-US-layout column for that work.

### Wayland: Explicit Second Target

Wayland intentionally prevents ordinary applications from freely reading other
windows or injecting arbitrary input. Sunoto must use capability-based adapters:

- XDG Global Shortcuts portal for push-to-talk activation
- AT-SPI editable-text APIs when the focused application exposes them
- Clipboard-based insertion where supported
- XDG Remote Desktop portal or an explicitly installed `uinput` helper only
  when the user grants the required access
- Clear UI when a focused application only supports "copy result to clipboard"

Wayland support must be tested separately on GNOME and KDE. "Works on Wayland"
cannot be treated as one checkbox.

## 6. MVP Scope

The first useful release should include only:

- Global hold-to-talk shortcut
- Microphone selection and level meter
- Always-warm Nemotron Speech Streaming 0.6B transcription
- English-only transcription
- Built-in punctuation and capitalization
- User-selectable 80 ms ultra-fast and 160 ms balanced streaming profiles
- Filler removal, punctuation, and simple self-correction cleanup
- Personal dictionary
- Generic text insertion at the cursor in any writable focused X11 text field,
  without an application allowlist
- Clipboard fallback
- Recording/transcribing/error status bubble
- Settings for shortcut, microphone, ASR backend, and history retention
- Local-only operation after model download

The MVP should not include accounts, cloud sync, teams, analytics, mobile apps,
meeting transcription, or a large command-mode assistant.

## 7. Quality Pipeline

Raw ASR output should pass through a staged pipeline:

1. **Normalize:** whitespace, punctuation spacing, repeated partials.
2. **Resolve corrections:** phrases such as "no", "actually", and "I mean".
3. **Remove fillers:** configurable removal of "um", "uh", and similar tokens.
4. **Apply dictionary:** user terms and context-specific spellings.
5. **Expand snippets:** exact voice triggers only, with confirmation for risky
   expansions.
6. **Apply style:** lightweight rules based on active app category.
7. **Insert fast result:** insert as soon as deterministic processing completes.
8. **Optional local LLM polish:** run only on demand or after insertion using a
   constrained prompt that preserves meaning, names, numbers, and code.
9. **Safety validation:** reject or fall back to raw text if the polished output
   changes facts, becomes much longer, or drops protected terms.

The user must be able to choose raw transcription, deterministic cleanup, or
LLM polish. Raw and deterministic modes are the only modes allowed to block
initial text insertion.

The default product mode is **Fast**: streaming ASR plus deterministic cleanup.
**Polish** is an explicit secondary mode because a general-purpose local LLM
cannot be allowed to determine dictation latency.

## 8. Delivery Plan

### Phase 0: Feasibility Spike

Goal: prove the two riskiest components before building the full UI.

- Repair or install the NVIDIA driver for the RTX 3060
- Run Nemotron Speech Streaming 0.6B through NVIDIA NeMo
- Measure cold start separately from warm dictation latency
- Measure VRAM, real-time factor, time to first partial, release-to-final-text,
  and insertion latency at p50, p95, and p99
- Compare Nemotron's 80, 160, and 560 ms profiles for interactive latency,
  stability of partial results, and accuracy
- Measure the non-NVIDIA fallback separately; it is not allowed to weaken the
  Nemotron fast path
- Record a small test corpus containing accents, filler words, corrections,
  names, code terms, and background noise
- Build a minimal X11 prototype that captures a hotkey and inserts fixed text
  into Firefox, VS Code, a terminal, and a GTK text editor

**Exit gate:** Nemotron's selected default profile produces a first partial
within 250 ms and inserts usable final text within 600 ms of releasing the
shortcut at p95 on the target machine. The default should remain 160 ms unless
the recorded corpus proves that the 80 ms profile has acceptable accuracy.

*(Status note, June 12, 2026: Phase 0 closed with compatibility proven but
the interactive latency gate unmeasured — the offline wrapper could not
measure it — and the corpus unrecorded. The gate measurement was carried
into the Phase 1 latency harness and has since been met: release-to-
insertion p95 was measured at 151 ms (160 ms profile) and 104 ms (80 ms
profile), see `phase-1-results.md`. The 250 ms first-partial clause was
written without a reference point and is superseded by the anchored metric
in section 9. Corpus recording moved to Phase 2 scope.)*

### Phase 1: X11 Vertical Slice

- Rust background daemon
- Global push-to-talk shortcut
- Persistent microphone capture, pre-roll buffer, and VAD (VAD runs only in
  active sessions; push-to-talk release, not silence, ends an utterance)
- ASR sidecar lifecycle and IPC. The IPC contract must support asynchronous
  partial-transcript events pushed by the sidecar while audio chunks are in
  flight, demonstrated against the mock sidecar emitting timed partials —
  a request/response design cannot pass this gate
- Simple status bubble
- Generic focused-target insertion with clipboard-preserving fallback
- Basic settings file

**Exit gate:** a user can dictate repeatedly at the cursor in any writable
focused X11 text field without restarting Sunoto or manually pasting text,
while maintaining the release-to-insertion p95 target. The desktop
compatibility matrix must include at least five applications from different UI
toolkits and application categories; those applications are test coverage, not
an allowlist or product limitation. The latency harness must also record
warm-start time and VRAM, the two Phase 0 measurements that were deferred.

### Phase 2: Polished Dictation

- Deterministic cleanup pipeline
- Personal dictionary
- Snippets
- Active-application style profiles
- Optional local text LLM
- Raw-versus-polished diff logging for local evaluation
- Record the evaluation corpus (accents, fillers, corrections, names, code
  terms, background noise) — moved here from Phase 0, where it was never
  produced

**Exit gate:** the polished result improves the zero-edit rate on the recorded
test corpus without changing names, numbers, or meaning.

### Phase 3: Product UX

- Settings window
- Tray indicator
- Microphone and model setup flow
- Model download and disk-space management
- Error recovery and offline status
- Optional local history and scratchpad

### Phase 4: Wayland Adapters

- Global Shortcuts portal integration
- AT-SPI focused-field detection and insertion
- GNOME Wayland test pass
- KDE Wayland test pass
- Hyprland test pass using the adopted layer-shell overlay and the `hypr/`
  window rules inherited from the upstream voice-dictation project (added
  June 12, 2026)
- Permissioned fallback path and clear capability diagnostics

### Phase 5: Packaging and Reliability

- `.deb` package for Ubuntu/Mint
- AppImage
- Autostart and systemd user-service integration
- Crash recovery
- Upgrade and model migration path
- Privacy documentation and local-data deletion controls

## 9. Performance Targets

*(Amended June 12, 2026: each target is now annotated **[measured]** — backed
by the Phase 1 latency harness on the target RTX 3060, see
`phase-1-results.md` — or **[provisional]** — still a goal, not yet
validated. The first-partial metric now has a defined anchor; without one it
was unfalsifiable.)*

- Shortcut-to-recording feedback: under 50 ms **[provisional]**
- Time to first partial text, measured from speech onset in the audio stream
  (pre-roll included) to the partial reaching the status bubble: the
  cache-aware encoder's first chunk dominates this — measured ~830 ms p95 on
  the 160 ms profile and ~715 ms p95 on the 80 ms profile. Target: under
  1 second p95 **[measured]**; the original 250 ms figure predates the
  first-chunk reality and applied no anchor.
- Release-to-final-text insertion: under 300 ms p50 **[measured: 110 ms on
  the 160 ms profile]**
- Release-to-final-ASR-result: under 450 ms p95 **[measured: 125 ms]**
- Deterministic cleanup: under 20 ms p95 **[measured: microseconds]**
- Text insertion: under 50 ms p95 **[measured: 26 ms including echo
  confirmation]**
- Release-to-final-text insertion: under 600 ms p95 and under 1 second p99
  **[measured: 151 ms p95 / 151 ms p99, five paced sessions]**
- Warm-model availability after login: under 10 seconds **[provisional —
  measured 16.6–32.3 s spawn-to-ready today; needs load-time optimization
  (Riva/exported engine or lazy decoder init) or a revised target]**
- Sidecar VRAM while streaming: ~3.6 GiB measured on the 12 GiB card; set
  the product minimum after testing a lower-end card **[measured]**
- No dropped first or last word in normal push-to-talk use (pre-roll covers
  the first word; release-flush covers the last) **[provisional — needs the
  live-microphone corpus pass]**
- Idle CPU while listening for activation: under 1% on the target machine
  **[provisional]**
- No inference compute while idle; keeping model weights in VRAM is
  acceptable **[measured: the sidecar blocks on stdin between sessions]**
- No network access after models are installed **[provisional]**
- Dictation bubble must never steal keyboard focus **[measured:
  override-redirect window, never given input focus]**

## 10. Test Strategy

### Speech Quality

- Word error rate for raw ASR
- Zero-edit rate for final output
- Protected-term accuracy for names, numbers, URLs, and code identifiers
- Self-correction and filler-removal accuracy
- Latency percentiles, not only averages

### Desktop Compatibility Matrix

- Cinnamon X11, GNOME Wayland, KDE Wayland
- Firefox and Chromium
- VS Code and another Electron app
- GTK and Qt text editors
- Common terminals
- LibreOffice
- Password fields, where insertion must be disabled
- At least one non-US keyboard layout, plus a layout switch during an active
  dictation (added June 12, 2026; see "Keyboard-Layout Dependence")

### Reliability and Privacy

- Microphone disconnect/reconnect
- Model process crash and restart
- GPU out-of-memory fallback
- Clipboard restoration
- No text insertion after focus changes
- Verification that audio and text never leave the machine

## 11. Main Risks

| Risk | Mitigation |
| --- | --- |
| Nemotron's 80 ms profile loses too much accuracy | Default to 160 ms and expose 80 ms as an ultra-fast option |
| Nemotron is too GPU-heavy for lower-end NVIDIA cards | Keep the backend interface replaceable and provide a fallback |
| NVIDIA runtime is unavailable | Provide clear diagnostics and a CPU/fallback backend |
| Wayland prevents universal insertion | Use portals and AT-SPI, then expose capability-specific fallbacks |
| LLM polish changes meaning | Use constrained prompts, protected terms, validation, and raw fallback |
| Context capture leaks sensitive text | Make it opt-in, local-only, app-blocklist aware, and never read password fields |
| Model downloads make setup difficult | Build resumable downloads, checksums, disk-space checks, and model presets |

## 12. Repository Layout (as built)

*(Amended June 12, 2026: the codebase moved to the `sunoto-backend` branch of
`github.com/shakdwipeea/voice-dictation`, merging the sunoto backend with that
project's overlay UI. Its whisper/silero Python backend was removed as
superseded by the Rust pipeline; both commit histories are preserved.)*

```text
apps/
  daemon/              dictation daemon, settings, latency bench, self-tests
  desktop/             GTK settings window (Phase 3, not yet created)
crates/
  sunoto-core/         session state machine and shared types
  sunoto-audio/        persistent PulseAudio (parec) capture
  sunoto-ipc/          sidecar process management and NDJSON event pump
  sunoto-linux/        X11 adapters (hotkey, insertion, clipboard, bubble)
  sunoto-polish/       cleanup, dictionary, snippets, styles
services/
  asr/                 Nemotron streaming sidecar, mock sidecar, Phase 0 bench
src/
  voice_dictation/     GTK4 layer-shell pill overlay (Python), adopted from
                       the upstream voice-dictation project; needs a stdin-
                       JSON driver to be run as a UI sidecar by the daemon
hypr/                  Hyprland window rules for the overlay (upstream)
systemd/, install.sh   upstream packaging — still points at the removed
                       Python daemon, pending adaptation to sunoto-daemon
bin/                   upstream GPU status/snapshot helper scripts
tests/
  corpus/              local recorded/evaluation fixtures and the scripted
                       phase2 zero-edit corpus
  phase0/, phase1/, phase2/  Python test suites
tools/
  phase0/              system, audio, and X11 feasibility probes
  phase2/              corpus recorder and sidecar transcriber
docs/
  product-plan.md, phase-0-results.md, phase-1-results.md,
  phase-2-results.md, code-review-2026-06-12.md
```

## 13. Immediate Next Step

*(Amended June 12, 2026 — the original instruction, "start with Phase 0
only," is complete: the always-warm streaming backend plus X11 insertion
measure 151 ms release-to-insertion p95 against the 600 ms gate.)*

Next steps, in order:

1. Run the manual five-application desktop compatibility pass and a
   live-microphone accuracy spot check to formally close Phase 1.
2. Record the Phase 2 evaluation corpus — the harness is built and the
   workflow is `make phase2-record` (human speaks), `make phase2-transcribe`,
   `make phase2-eval-recorded`. The scripted text corpus already measures
   96% polished vs 28% raw zero-edit rate with zero regressions; tune the
   correction gates from the recorded data (the `known-gap` case documents
   the first candidate).
3. Integrate the adopted overlay (added June 12, 2026): the stdin-JSON
   driver (`src/voice_dictation/ui_sidecar.py`, `vd-overlay` entry point)
   and the X11 EWMH anchoring backend are built and protocol-tested
   (`make ui-test`; `make ui-demo` drives the pill by hand). Remaining:
   wire `sunoto-daemon` to manage it through `sunoto-ipc` exactly like the
   ASR sidecar — show/hide on session start/stop, stream the mic level —
   and adapt `install.sh`, the systemd unit, and the README from the
   removed Python daemon to `sunoto-daemon`. The X11 dev machine needs the
   `gir1.2-gtk-4.0` system package installed for the GTK pill; until then
   the override-redirect bubble remains the working UI there.
4. Phase 3 product UX: install GTK4 development packages (`libgtk-4-dev` is
   not currently installed), then build the settings window, tray indicator,
   and setup flow; investigate warm-start reduction (currently 16–32 s
   against the 10 s goal).

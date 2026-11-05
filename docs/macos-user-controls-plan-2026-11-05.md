# macOS user controls implementation plan

Date: 2026-11-05
Status: Partially implemented; release gate not met. See [implementation evidence](macos-user-controls-evidence.html).

## Outcome and scope

A person can configure Sunoto, check their microphone and permissions, learn
the shortcut, control privacy, pause, and quit without editing JSON or using
Terminal. Preserve the current fast dictation flow and existing installations.

This plan covers macOS controls only. App Store packaging and sandbox
feasibility remain a separate release dependency; completing this plan does
not establish App Store eligibility. Do not rewrite the recognition engine,
enable optional AI polish by default, or expand System mode as part of it.

The older July product plan contains superseded defaults and missing-feature
claims. Use the current code and this document for this workstream: tap to
toggle, Right Option, and optional LLM polish off remain the existing defaults.

## Existing foundations

| Area | Existing implementation | Work required |
| --- | --- | --- |
| Native UI | `services/macos/sunoto-overlay.swift`: AppKit menu, permission and report panels | Settings window and practice view |
| Settings | `apps/daemon/src/settings.rs`: typed config, validation, persistence | Narrow UI-facing updates and live application |
| UI protocol | `crates/sunoto-ipc/src/lib.rs`: typed NDJSON messages | Settings state, commands, acknowledgements |
| Audio | `crates/sunoto-audio/src/macos.rs`: CoreAudio capture and device resolution | Device enumeration, stable selection, preview lifecycle |
| Meter | `apps/daemon/src/session.rs`: peak/RMS calculation | Reuse for an explicit microphone test |
| Shortcuts | `crates/sunoto-macos/src/hotkey.rs`, `TriggerGestures` in `sunoto-core` | Recorder, safe rebind, selectable hold/toggle behavior |
| Health | `apps/daemon/src/health.rs`, platform permission helpers | Reusable status and repair controls |
| Vocabulary | `apps/daemon/src/vocabulary.rs`, `word_list*` settings | Editable list and source controls |
| History | `apps/daemon/src/history.rs`, retention/exclusion settings | Controls, app picker, truthful deletion feedback |
| Login | `apps/daemon/src/first_open.rs`, desktop login-item APIs | Explicit preference and actual OS status |

Read `docs/desktop-configuration.md` before changing platform configuration,
and `docs/macos-recurring-issues.md` before touching hotkeys or permissions.

## Product decisions

- Keep the current AppKit helper as the native UI host. Split new controllers
  into focused Swift files and update the build/render scripts accordingly;
  avoid adding all controls to the existing large overlay file.
- The Rust daemon remains the authority for settings, session state, capture,
  and shutdown. UI controllers send commands; they do not edit config files or
  launch additional daemons, audio capture processes, or ASR instances.
- Settings is a single reusable window with General, Dictation, Vocabulary,
  and Privacy panes. Use standard native controls, keyboard navigation,
  VoiceOver labels, and layouts that support increased text sizes.
- General: open at login, permissions, and replay setup. Dictation: shortcut,
  interaction mode, microphone, and test. Vocabulary: personal terms and
  context sources. Privacy: history, retention, exclusions, and diagnostics.
- Use a draft with Apply and Cancel for editable preferences. Permission
  requests, microphone tests, login registration, and history deletion are
  explicit immediate actions with their own result feedback. Warn before
  closing a window with unapplied edits.
- Apply shortcut, microphone, and interaction changes only when no session or
  preview is active. Show an explicit pending state and a way to cancel it;
  apply before accepting another session. Never change a running recording.
- Keep normal dictation defaults for existing users. New installations ask
  whether Sunoto should open at login; do not silently register them.
- Pause is temporary for the running app. It stops capture and blocks new
  sessions, while keeping the menu and Settings usable. Relaunch resumes the
  normal ready state. Quit is distinct and stops the daemon and its helpers.

## Phase 1 — Settings state and native shell

Deliver a Settings… menu item, native window, and reliable read/apply flow.

1. Introduce a narrow settings view model containing supported preferences,
   capabilities, current health, and a revision. Exclude sidecar paths and
   other implementation settings from the UI contract.
2. Add typed request/response messages for fetching and applying a patch,
   including request IDs, validation errors, applied/pending state, and revision
   conflicts. Reject stale drafts instead of overwriting newer changes.
3. Validate a complete candidate using the existing settings validation.
   Preserve unrelated settings and use atomic persistence. For resource
   changes, prepare replacements first and retain/restore the previous usable
   state on registration, device-open, or disk-write failure.
4. Keep disk work and platform preparation off the latency-sensitive path;
   publish completion back to the one daemon event loop. Reuse the bounded UI
   channel and coalesce state updates.
5. Initially wire simple preferences such as retention and vocabulary. Add
   lifecycle-specific application handlers in the following phases.

Acceptance: open Settings repeatedly without duplicate windows; valid changes
survive relaunch; invalid/stale changes explain the problem; disk failure does
not report success; Cancel changes nothing; dictation remains responsive.

## Phase 2 — Pause, Quit, and login control

1. Add Pause Sunoto / Resume Sunoto, Settings…, and Quit Sunoto to the menu.
   A paused app must say Paused rather than Ready.
2. While recording or transcribing, Pause or Quit asks whether to discard the
   current session. Cancel preserves it. Confirm invalidates its session ID,
   cancels capture, and prevents late ASR/polish/insertion results from typing.
3. On pause, stop capture even when an idle microphone timeout is configured;
   reset gesture state and prevent control-socket triggers from bypassing it.
4. Route Quit through daemon cleanup. Stop audio, release hotkeys, flush owned
   history work, stop sidecars/workers, close the overlay and control socket,
   and prevent helper restart loops. Handle pending clipboard restoration
   without overwriting a newer clipboard value.
5. Expose Open at Login through existing native login-item APIs. Display actual
   OS registration/approval status, including external changes. Update the
   existing route marker so disabling is respected on future launches.
6. Preserve existing registration choices. Detect legacy registration and
   explain the supported removal path without unexpectedly requesting
   Automation permission. Failed changes must not display a successful toggle.

Acceptance: paused shortcuts never capture; resume works; Quit leaves no owned
process or microphone recording; disabling login survives relaunch; no new
install registers before an explicit choice. Quit does not change login choice.

## Phase 3 — Shortcut recorder and interaction modes

1. Add a recorder for supported key chords and right-hand modifiers, with
   clear instructions, Escape to cancel, and Restore Default.
2. While recording a shortcut, suppress dictation triggers and do not send
   recorded keys to another app. Restore the previous binding on failure or
   cancellation. Refuse to enter recorder mode during a session.
3. Reject unsupported/reserved combinations and collisions with Sunoto's
   System shortcut. Report registration failures; do not claim every conflict
   with another app can be detected.
4. Add a persisted interaction enum: `toggle` and `hold`, defaulting to toggle
   when absent. Extend the existing gesture routing rather than creating a
   second session implementation. Keep System mode behavior separate.
5. Toggle: first tap starts, next tap finishes. Hold: key down starts, key up
   finishes. Escape cancels either. Handle key repeat, interrupted input,
   permission loss, sleep/wake, and mode changes with no stuck recording.
6. Update menu and overlay instructions to match the selected mode and key.

Acceptance: both modes work with physical keys and supported chords; the
recorder never starts dictation; failed rebind retains the previous shortcut;
old config files keep their behavior; changing preferences requires no restart.

## Phase 4 — Microphone selection, test, and permissions

1. Enumerate available CoreAudio input devices with readable names. Persist
   a stable device UID, resolving to runtime device IDs when opening capture;
   keep compatibility with current `auto` and numeric-device configurations.
2. Offer Follow System Default plus explicit devices. Refresh after plug,
   unplug, and default-device changes. If an explicit device disappears, show
   it as unavailable and offer another choice rather than silently switching.
3. Add Start Test / Stop Test using the daemon's capture owner. Feed only
   throttled level readings to Settings; preview audio must not reach ASR,
   history, reports, transcript logs, or insertion. Do not retain audio.
4. Stop the test on window close, permission loss, device loss, pause, quit,
   or after a 30-second timeout. Disable dictation while testing and explain
   how to stop the test. Do not automatically activate the mic on window open.
5. Display separate Microphone and Accessibility status. Reuse existing
   permission actions; provide Allow or Open System Settings as appropriate.
   Refresh when Sunoto regains focus and when permissions change.

Acceptance: test levels respond to speech; the mic turns off when testing ends;
USB/Bluetooth changes are reflected; selected devices survive reconnect and
relaunch; denied access gives a repair action without implying permission was
granted or requiring Terminal.

## Phase 5 — Vocabulary and privacy controls

1. Provide add/edit/remove for personal terms with exact spelling preserved,
   case-aware deduplication rules, and reasonable length/count validation.
   Apply changes to subsequent sessions without restarting the model.
2. Expose current-context vocabulary and optional Contacts/Documents sources,
   explaining their scope. Protected sources stay off unless explicitly
   enabled. Request access only following the corresponding user action;
   represent denied access separately from the user's saved preference.
3. Add history on/off and retention presets: 7, 30, 90 days, and Forever.
   Preserve existing custom values. Explain that disabling stops new entries
   but does not delete old ones; offer a separate Clear History… action.
4. Add an app picker and removable exclusion list, preserving built-in
   password-manager exclusions. Label this accurately as excluding transcript
   text from history: current exclusions can still retain metadata and do
   not mean disabling dictation or context collection for that app.
5. Apply retention changes to existing history; confirm before a shorter
   period deletes entries. Reflect partial deletion or persistence failures.
6. Turn transcript diagnostic logging off for new default configurations and
   provide a clear opt-in control. Preserve explicit existing preferences;
   explain that history exclusions do not protect separately enabled debug
   transcript logs. Clear History must describe its scope, including that it
   does not clear separately saved reports or old logs.

Acceptance: vocabulary is used on the next session; privacy preferences survive
relaunch; disabled history records no new entries; excluded-app text is absent
from new history; secure fields remain protected; UI never overstates deletion
or privacy coverage. Turning a source off prevents subsequent collection from it.

## Phase 6 — Guided first dictation

1. Extend setup: welcome → model/permissions readiness → microphone check →
   shortcut choice → practice → login choice → completion. Reuse the controls
   above rather than maintaining a second settings implementation. Download
   progress remains visible, and users can defer practice or revisit setup.
2. Introduce an explicit practice destination in session metadata, separate
   from Dictation/System routing. Practice must exercise real hotkey, capture,
   ASR, and configured cleanup, but return text only to the native practice
   view. Never call clipboard, external insertion, history, or report paths;
   redact practice transcript logs regardless of debug preferences.
3. Reuse a suggested phrase and editable result field. Let users retry and
   see whether microphone, shortcut delivery, and recognition succeeded.
   Do not present practice success as proof of cross-app paste compatibility.
4. Close, skip, pause, or loss of the practice view cancels the session and
   discards late results. Clear practice text on exit. Store only completion
   state, and add Try Dictation Again in Settings.

Acceptance: a new user reaches a practice result without Terminal; changing
focus to another app never types or copies practice text; interrupted practice
can restart; existing users are not forced through new onboarding.

## Delivery sequence and verification

Implement phases as six reviewable changes, keeping each usable before moving
on. Phase 1 is the dependency for all controls; phase 6 depends on phases 2–5.
Deliver phase 2 early because reliable cancellation/shutdown protects all later
preview and configuration interactions. Record completed acceptance criteria
and unresolved compatibility limitations in this document as work progresses.

Automated coverage should target behavior: settings validation/atomic failure,
stale revisions, gesture transitions, pause/quit late-result suppression,
practice destination isolation, preview cleanup, history policies, stable device
selection, and protocol round trips. Use mocks for lifecycle tests; do not
start a second real speech engine for verification.

For code changes, run repository-required checks:

```sh
cargo test --workspace --offline
cargo clippy --workspace --offline --all-targets -- -D warnings
make test
```

Also compile the native helper through the actual app build path. Check the
new panels visually and with keyboard navigation/VoiceOver. Restart the
installed app before judging changed daemon/helper behavior, following AGENTS.

Manual release matrix: clean install and upgrade; denied/revoked permissions;
built-in/USB/Bluetooth microphones; unplug/reconnect; sleep/wake; multiple
displays/Spaces/full screen; hold and toggle; pause/quit in each session state;
Settings closed mid-test; debug logging and history disabled; login disabled
externally; failed config write; Notes, Safari/Chrome, an Electron app, and a
disposable terminal target. Run real insertion only into disposable fields.

Before and after, compare dictation turnaround and idle resource use on the
same Mac. Investigate any repeatable regression. Merely opening Settings must
not activate the microphone, restart/load another model, or add ongoing GPU work.

Release gate: every listed control works without JSON editing, its displayed
state matches daemon/OS state, and no preview, practice, cancelled, paused, or
quit session can insert text into an external app.

## Implementation checkpoint

Implemented the narrow vocabulary/diagnostics Settings window and protocol, revision rejection, asynchronous atomic settings persistence, vocabulary limits/deduplication, and diagnostic logging default off. Native helper compiles. Full lifecycle and onboarding phases remain outstanding; see the evidence report for exact coverage and verification limitations.

# macOS red flags: review and fix plan

**Created:** 2026-09-16
**Scope:** macOS only. Linux and System mode are untouched by this plan.
**Input:** the nine red flags in `docs/architecture.md` and the owner's
review of each one.

## 1. Decisions from the review

| # | Red flag | Owner's call | Decision |
| --- | --- | --- | --- |
| 1 | Hotkey silently dies on macOS | fix | **Fix.** Probe the tap for real event delivery and report the truth. |
| 2 | Says "ready" when it is not | fix | **Fix.** Introduce explicit health states and show them in the overlay. |
| 3 | Install needs a developer setup | wants `apt`/`brew` style install | **Fix.** Homebrew tap. Formula first, signed cask later. |
| 4 | Permissions are fragile | make it easy | **Fix.** One binary to grant, guided setup that verifies each grant live. |
| 5 | Clipboard is overwritten | investigate how to fix | **Investigate, then fix.** Save and restore the pasteboard around the paste. Findings in §2.5. |
| 6 | Types into password fields | ok for now, better plan welcome | **Fix cheaply.** An Accessibility role check is about 60 lines and shares code with item 4. |
| 7 | Transcripts in the log | keep, useful for debugging | **Keep.** Add a `log_transcripts` setting so it can be turned off before a public release. Default stays `true`. |
| 8 | Mic always on | keep first-word capture and LLM warmth, open to a better way | **Change.** Keep the pre-roll while the user is active, stop capture after idle. Move the LLM keepalive from "forever" to "press until polish done". Details in §2.8. |
| 9 | One giant file | modularize | **Fix.** Split `daemon.rs` by responsibility, and split the parts that items 1, 2, 5 and 6 touch first. |

## 2. Plan per red flag

### 2.1 Hotkey silently dies

**Root cause.** `CGEventTapCreate` succeeds without Input Monitoring, and
the current health checks (`CGPreflightListenEventAccess`,
`CGEventTapIsEnabled`) can both say yes while no event is delivered. The
daemon then logs "Sunoto ready for dictation" on the strength of the ASR
sidecar alone.

**Fix: a delivery probe.**

1. In `crates/sunoto-macos/src/hotkey.rs`, after the tap is installed, post
   a synthetic `flagsChanged` event with `CGEventPost` at the session tap.
   Tag it by setting the `kCGEventSourceUserData` field to a magic value.
2. In `tap_callback`, if the user-data field equals the magic value, set an
   `AtomicBool` `probe_seen` and return without touching hotkey state.
3. `HotkeyListener::open` waits up to 1 s for `probe_seen`. Failure returns
   a new `X11Error::HotkeyBlocked` variant with the human message "Input
   Monitoring is not granted to <path>".
4. Repeat the probe after every re-arm (both the timeout callback and the
   once-a-second `CGEventTapIsEnabled` check). Not periodically: a posted
   event counts as user input and would keep the display from sleeping.
   Re-arms are exactly the post-sleep and screen-lock cases, so they are
   covered. A failed probe flips the daemon health state to `HotkeyBlocked`,
   and a later success flips it back.
5. `sunoto-daemon check` runs the same probe and fails if it fails. It stops
   being a false positive.

**Verification.** Run `check` with a binary that has no grant: it must fail
with the reason within about a second (verified 2026-09-17 on the debug and
freshly rebuilt release binaries). Start the daemon from the login item with
grants in place: the log must show "shortcut verified" before "ready".
Existing `selftest hotkey` still passes.

**Status: done on branch `macos-red-flags` (M1).**

### 2.2 Says ready when it is not

**Fix: one health state, shown everywhere.**

1. Add `enum DaemonHealth { LoadingAsr, WarmingPolish, HotkeyBlocked,
   MicUnavailable, Ready }` in a new `apps/daemon/src/health.rs`. The event
   loop owns it and recomputes it whenever `sidecar_ready`,
   `llm_post_asr_warmed`, the hotkey probe, or capture state changes.
2. The overlay gets a new `state{name, detail}` message. The Swift pill
   shows a grey dot with a caption for every non-ready state, and the red
   dot only while recording.
3. A press during a non-ready state shows that state's caption instead of
   being dropped. The press is still not a recording, but the user sees why.
4. The "Sunoto ready for dictation" log line moves to the `Ready`
   transition and fires only there.

**Verification.** Cold start: the pill appears at once with "loading speech
model", then "warming polish", then disappears. Pressing during each state
shows the caption.

**Status: done on branch `macos-red-flags` (M1), pending a live cold-start
check after the daemon is restarted from the login item.**

### 2.3 Homebrew install

**Constraint.** `brew services` uses launchd, and launchd is what makes the
tap inert. So the install must produce an app bundle that a Login Item
starts, not a launchd agent. That is what `install-macos.sh` builds by hand
today; the formula automates it.

**Fix, stage 1: a Homebrew tap with a formula.**

1. Create `homebrew-sunoto` with `Formula/sunoto.rb`. Dependencies: `rust`
   (build), `python@3.12`. Build steps: `cargo build --release`, `swiftc`
   the overlay, and `virtualenv_install_with_resources` for `parakeet-mlx`
   and `llama-cpp-python` into `libexec/venv`.
2. The formula installs `sunoto-daemon`, `sunoto-overlay`, and a new
   `sunoto` CLI wrapper into `bin`. `sunoto` is the user-facing command.
3. `sunoto setup` (new subcommand, see §2.4) builds `~/Applications/Sunoto.app`,
   downloads the models into `~/Library/Application Support/sunoto/models`
   with a progress bar, registers the Login Item, and starts the app.
   The ASR model is about 600 MB and is required. The LLM model is about
   3 GB and the setup asks before downloading it; without it the app runs
   with deterministic polish only.
4. `sunoto status`, `sunoto restart`, `sunoto log`, `sunoto uninstall`
   round out the CLI so no one needs `ps`, `kill`, or `tail`.
5. `install-macos.sh` becomes a thin wrapper that calls `brew install` and
   `sunoto setup`, and the README shows three commands.

**Known limit of stage 1.** Ad-hoc signatures change on every upgrade, so
`brew upgrade sunoto` invalidates the TCC grants. `sunoto setup` detects
this with the probe from §2.1 and walks the user through re-granting. It is
one guided click per upgrade, not a mystery.

**Stage 2: a signed cask.** With an Apple Developer ID, the same bundle is
signed and notarized, shipped as a cask, and grants survive upgrades. The
formula and cask share the bundle layout, so stage 2 is a signing step and a
cask file, not a rewrite. Needs the owner's decision on an Apple Developer
account.

### 2.4 Permissions made easy

**Root cause.** Two files need grants (the bash login item and the bare
daemon) because the login item is a separate process that stays the
daemon's parent.

**Fix: one process, one grant, guided.**

1. Make the daemon binary itself the app's executable
   (`Sunoto.app/Contents/MacOS/sunoto-daemon`). Launch Services then gives
   the daemon the GUI context directly and the bash wrapper goes away. The
   env file is replaced by the daemon reading its config as it does now.
   Only "Sunoto" appears in the permission lists.
2. `sunoto setup` checks each permission with the real API and opens the
   exact pane when missing: `CGPreflightListenEventAccess` for Input
   Monitoring, `AXIsProcessTrusted` for Accessibility, and
   `AVCaptureDevice.authorizationStatus` for Microphone. It polls until
   granted, then runs the §2.1 probe and a 1 s capture test before printing
   "ready".
3. The pane URLs are `x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent`,
   `?Privacy_Accessibility`, and `?Privacy_Microphone`.
4. The same checks run at daemon start and feed the health state, so a
   missing grant after an upgrade is shown in the pill, not discovered by
   pressing a dead hotkey.

**Verification.** Fresh user account: `brew install`, `sunoto setup`,
three grants, one dictation, under five minutes, no terminal beyond the two
commands.

### 2.5 Clipboard overwrite

**What was investigated.**

- macOS paste is `pbcopy` then Cmd+V. The previous contents are lost.
- X11 already saves and restores, so the daemon has the pattern.
- `pbpaste` only round-trips plain text. Rich text, images, and files on
  the clipboard would be lost by a text-only restore.
- There is no event for "the target has consumed the paste". Apps read the
  pasteboard synchronously while handling Cmd+V, so a fixed delay after
  posting the key event is the only option. The X11 path uses 400 ms.
- Clipboard managers (Maccy, Raycast, Paste) will record every dictated
  phrase unless the item is marked with the `org.nspasteboard.TransientType`
  UTI, which those managers honour.

**Fix.**

1. Replace `pbcopy`/`pbpaste` with `NSPasteboard` calls through the raw
   `objc_msgSend` bindings the crate already uses for `NSWorkspace`.
   Snapshot every item's types and data before writing.
2. Write the dictated text with two types: `public.utf8-plain-text` and
   `org.nspasteboard.TransientType`.
3. Post Cmd+V, wait 300 ms, then restore the snapshot only if the
   pasteboard `changeCount` is still ours. If the user copied something in
   that window, leave it alone.
4. If the focus-moved fallback parks text on the clipboard on purpose, do
   not restore. That is the one case where overwriting is the feature.
5. Setting `clipboard_restore` default `true`, for the rare app that reads
   the pasteboard late.

**Verification.** Copy an image in Preview, dictate into TextEdit, paste
into Preview again: the image is back. Same with a file in Finder. Dictate
into VS Code, Chrome, Terminal, Slack, and Zed. A clipboard manager shows no
dictated entries.

### 2.6 Password fields

**Fix, folded into the Accessibility work.**

1. Before insertion, ask `AXUIElementCreateSystemWide` for
   `kAXFocusedUIElementAttribute`, then its `AXRole` and `AXSubrole`.
2. If the role is `AXSecureTextField`, do not insert or paste. Show
   "password field, nothing inserted" and leave the transcript in the log.
3. Bonus from the same call: `AXFocusedApplication` gives the frontmost
   app's bundle id without Screen Recording permission. Use it for the
   app-aware polish style and for the focus token, and stop walking
   `CGWindowList`. App-aware styling stops degrading silently.

**Verification.** Focus the Safari password field on a login page and
dictate: nothing is typed and the pill says why. Focus the username field:
normal insertion.

### 2.7 Transcripts in the log

**Kept as requested.** One small change so the switch exists:

1. Add `log_transcripts: bool` to settings, default `true`.
2. When false, the session lines log character count and a short hash in
   place of the text.
3. Flip the default to `false` in the release that ships the signed cask.

### 2.8 Always-on microphone and LLM keepalive

Two separate mechanisms are behind this flag, and both can be narrowed
without losing what the owner wants.

**Microphone.** The pre-roll exists so the first word is not lost. The
cost is the orange mic indicator in the menu bar all day and a little
energy.

1. Measure first: add a timer around `AudioDeviceStart` on the M1 Pro. If
   cold start is under 100 ms, the loss on a first press after idle is a
   fraction of a syllable, since people hold the key before speaking.
2. Add `capture_idle_stop_secs` default 120. After that long with no
   session, stop capture and drop the pre-roll. On the next press, start
   capture immediately and begin the session with whatever arrives. Every
   session after that gets the full 300 ms pre-roll until the next idle
   period.
3. Health state shows "mic starting" for that first press so the user
   knows to keep holding.
4. `0` keeps today's always-on behaviour for anyone who prefers it.

**LLM keepalive.** The 1 s ping exists because ASR evicts the LLM's GPU
working set during recording and the first prefill after that costs about
3 s. Pinging while idle does nothing useful for that problem.

1. Add `keepalive_start` and `keepalive_stop` messages to the polish
   sidecar protocol. The loop thread sleeps on an `Event` until started.
2. The daemon sends `keepalive_start` on press and `keepalive_stop` 20 s
   after the polish result. Recording always lasts long enough for at least
   one ping before release.
3. Idle GPU work goes to zero. First-polish latency after idle is
   unchanged because the pings run for the whole recording.

**Verification.** Idle for three minutes: no orange dot, no GPU activity in
Activity Monitor. Press and dictate: transcript is complete, polish latency
within the current numbers. `bench --post-asr-llm` p50 unchanged.

### 2.9 Modularize `daemon.rs`

**Rule.** Behaviour-preserving moves only, one module per commit, tests and
clippy green after each. No new features ride along.

Target layout for `apps/daemon/src/`:

| Module | Moves from `daemon.rs` | Lines |
| --- | --- | --- |
| `events.rs` | `DaemonEvent`, `ControlCommand`, `ModeHotkeyEvent` | ~100 |
| `health.rs` | new: `DaemonHealth` and its transitions | ~120 |
| `overlay.rs` | `spawn_overlay`, `UiFront`, writer thread, suppression rules | ~250 |
| `control.rs` | `spawn_control_thread`, command parsing, polish and plan replies | ~200 |
| `sidecars.rs` | ASR spawn, respawn backoff, `handle_sidecar_loss`, warm-up handoff | ~200 |
| `session.rs` | press, release, partial, final, timeout, and the timing log | ~600 |
| `insertion/mod.rs` | `UiCommand`, `UiReport`, `StreamSession`, the UI thread loop | ~250 |
| `insertion/macos.rs` | `insert_macos` ordering | ~60 |
| `insertion/x11.rs` | `insert_x11` ordering | ~60 |
| `system_dispatch.rs` | the System-mode arm of the final handler | ~150 |
| `daemon.rs` | `run`, thread spawning, the `match` that delegates to the modules | ~400 |

Two cross-crate moves:

- `WaylandUiAdapter` leaves the daemon for `crates/sunoto-linux/src/wayland.rs`.
- `sunoto-desktop` gains a `DesktopAdapter` trait (focus, insert, clipboard,
  bubble) that the three adapters implement, replacing the name-matched
  `cfg` re-export. The macOS `X11Error` becomes `DesktopError`.

## 3. Sequence

Each milestone is shippable on its own. Estimates are working days for one
person who knows the code.

| Milestone | Items | Days | Why this order |
| --- | --- | --- | --- |
| **M1 Truthful** | 2.1 probe, 2.2 health states, `check` fix | 3 | Smallest change that removes the most common bug. Everything after it can report its own failures. |
| **M2 Extract** | 2.9 for `overlay.rs`, `insertion/`, `health.rs`, `events.rs` only | 2 | The next two milestones edit exactly these parts. Extract before editing so the diffs stay readable. |
| **M3 Safe insertion** | 2.6 AX focus + secure field, 2.5 pasteboard save/restore | 3 | Both sit on the new AX and NSPasteboard bindings. Ship together. |
| **M4 Quiet idle** | 2.8 capture idle stop, keepalive window, 2.7 setting | 2 | Independent of the rest. Measure before committing to the idle timeout. |
| **M5 One process** | 2.4 daemon as the app executable, `sunoto setup` with live checks | 3 | Prerequisite for the formula. Removes the bash login item and one of the two grants. |
| **M6 Brew** | 2.3 tap and formula, model download, `sunoto` CLI, README | 4 | Everything a normal user touches. |
| **M7 Finish the split** | rest of 2.9, `DesktopAdapter` trait, Wayland move | 4 | Safe to do last because M1 to M6 have added the tests that catch regressions. |
| **Later** | signed cask, transcript logging default off | needs Apple account | |

Total about 21 working days for M1 through M7.

## 4. What is not in this plan

- Linux behaviour is left as is. The `DesktopAdapter` trait in M7 must not
  change X11 or Wayland semantics.
- System mode is not touched, and stays off by default.
- Spoken punctuation and number handling are product features, not red
  flags, and belong in a separate plan.
- Replacing the Python sidecars with native Swift or Rust inference is
  attractive for packaging but is a rewrite, not a fix. Revisit after M6 if
  the bundled Python runtime proves painful.

## 5. First step

Start M1 with the probe in `crates/sunoto-macos/src/hotkey.rs`. It is
contained, testable by revoking a permission, and fixes the bug the project
has hit most often.

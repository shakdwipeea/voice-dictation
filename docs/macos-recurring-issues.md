# macOS recurring issues & verified fixes

This file documents the macOS problems that have bitten us **more than once**.
Each entry has the symptom, the real root cause, and the verified fix — so we
don't re-debug from scratch next time. AGENTS.md's "macOS operations" section
points here.

When a macOS symptom matches one of these, read this file BEFORE changing code.

---

## 1. Inert CGEventTap — "Ctrl+F1 does nothing" (HIGHEST FREQUENCY)

This has recurred **many** times. It is almost always a TCC/permission +
code-signature problem, NOT a logic bug.

### Symptom

- You hold Ctrl+F1; nothing happens. No `session N: recording` line in the log.
- The log may still show `ASR sidecar ready: ... Hold Ctrl+F1 to dictate.`
- Before 2026-09-17, `./target/release/sunoto-daemon check` said
  `global hotkey grab: ok` even with an inert tap. It now posts a probe event
  through the tap and fails with the reason when nothing comes back, and the
  running daemon logs `shortcut is not receiving events` and shows
  "hotkey blocked" in the pill instead of "ready".

### Proof the daemon is healthy (run this first)

Send a synthetic press through the control socket:

```sh
(echo "press"; sleep 1; echo "release") \
  | nc -U -w3 /var/folders/*/T/sunoto-$(whoami)-daemon.sock
```

If the log then shows `session N: recording` → `sidecar accepted`, the **entire
daemon→sidecar→audio pipeline is fine** and the ONLY thing broken is the
physical-keyboard event tap. Stop here; it's §1, not the ASR.

### Root causes: unstable signing and concurrent requests

- The CGEventTap (`crates/sunoto-macos/src/hotkey.rs`) calls
  `CGEventTapCreate` with `kCGEventTapOptionListenOnly`. On macOS 10.15+,
  capturing keyboard events requires **Input Monitoring** TCC permission
  (requested via `CGRequestListenEventAccess`).
- `CGEventTapCreate` can return **non-null** even without the permission, but
  macOS then **immediately disables the tap** — it receives zero events. This
  is the "inert tap". Since 2026-09-17 the listener proves delivery with a
  tagged, no-op `flagsChanged` event posted through the tap at startup and
  after every re-arm; `check`, `selftest`, and the daemon's health state all
  use that verdict, so "ready" is only ever reported when events flow.
- TCC grants are bound to the app's code-signing identity. Before 2026-09-19
  the bundle was ad-hoc signed, so every rebuild changed its identity and
  silently invalidated the previous grant even when the toggle still showed
  on.
- Input Monitoring, Accessibility, and Microphone requests used to originate
  concurrently from three worker threads. macOS can suppress requests made
  off the main thread early in launch, and the installer's timed relaunch loop
  could stack or repeat prompts.
- Terminal commands also requested access under Terminal's identity, creating
  misleading extra entries in Privacy & Security.

### Fix: stable local signing + one request site + onboarding

`sunoto-daemon setup` creates a ten-year self-signed code-signing identity
named **Sunoto Local Code Signing** in the user's login keychain and trusts it
for code signing. Every installed bundle, including a prebuilt release bundle,
is re-signed with that same identity. The first upgrade from an ad-hoc build
resets the old Sunoto records and asks once; subsequent rebuilds and reinstalls
keep the same TCC identity and grants.

The native onboarding panel requests Input Monitoring, Accessibility, and
Microphone one at a time. Only the next missing row is actionable; each click
is handled by the daemon's main loop and opens only that service's Settings
pane. CoreAudio stays closed until the Microphone step. The panel remains open
after all checks pass; Done completes setup and relaunches the daemon once to
apply keyboard grants. Its close button postpones onboarding. `check`, `selftest`, and
`insert` only report permission state and never prompt. Setup no longer opens
panes or restarts the app on a timer.

For automatic startup, install `Sunoto.app` (since 2026-09-17 the daemon is
the bundle's own executable; the bash `sunoto-login` wrapper is gone):
```sh
bash install-macos.sh                 # builds, then sunoto-daemon setup
target/release/sunoto-daemon status   # live health over the control socket
tail -f "$HOME/Library/Logs/sunoto/daemon.log"
```

`Sunoto.app` is an `LSUIElement` application started by Launch Services, so
the daemon itself is the responsible GUI process. Follow the onboarding panel
and grant Accessibility, Input Monitoring, and Microphone to the single entry
named **Sunoto**. `setup` watches the app's own delivery and capture probes and
returns when all checks report ready.

Two facts learned live on 2026-09-17 that the tooling now handles:

- **A toggle that is on can still deny for an old ad-hoc build.** Setup resets
  Sunoto's own records once when it migrates to the stable identity. It does
  not reset them on later reinstalls.
- **Accessibility grants apply to processes started after the grant.** The
  running app keeps reporting blocked until it is relaunched, which is why
  the daemon watches permission state and relaunches on a real grant event.
  `restart` waits for the old process to exit before `open` because Launch
  Services ignores an app it still considers quitting.

The working manual development launch remains:
```sh
pkill -f "sunoto-daemon run"
nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 &
tail -f /tmp/sunoto-bare.log   # wait for "ASR sidecar ready"
```

The legacy launchd plist (`com.earendil-works.sunoto.plist`) has been
deleted from the repo; **a launchd-launched tap is inert due to TCC context**.
`sunoto-daemon setup` boots out and removes any copy still installed.

Verify the installed app's signing identity:
```sh
security find-identity -v -p codesigning | grep 'Sunoto Local Code Signing'
codesign -dvvv "$HOME/Applications/Sunoto.app" 2>&1 | grep '^Authority='
```

### Inert-tap recovery (code-level, already in place)

`hotkey.rs` now (a) re-arms the tap inside the `kCGSessionEventTapTimeout`
callback, and (b) periodically re-arms via `CGEventTapIsEnabled` (~4×/s) so a
screen-lock/sleep disable doesn't strand the hotkey. These help when the tap
is *validly* disabled by the system, but they CANNOT fix a missing TCC grant
(the system re-disables faster than we re-arm — that loop in the log IS the
permission problem, not a code bug).

### Diagnostic logging (currently compiled in)

`hotkey.rs` emits `[hotkey-diag]` lines:
- `tap is_enabled=1 (created ok)` — healthy.
- `tap is_enabled=0 (created ok)` then repeated `tap disabled by system;
  re-arming` — **TCC grant missing/stale**. Go to the Fix above.
- `event #N type=… keycode=… flags=…` — the tap is receiving keyboard events
  (first 8 only). keycode `122` (0x7a) = F1; flags `0x40000` = Control.

Once stable, these diag lines can be removed, but leave them until the issue
has not recurred for a while.

### Do NOT do

- Do not run the daemon via launchd and expect the hotkey to work — the
  launchd TCC context disables the tap. Use the GUI Login Item or `nohup ...
  &` from a terminal.
- Do not reset Sunoto's TCC records on every reinstall. Stable signing is what
  allows the grants to survive upgrades.
- `sunoto-daemon check` now fails on an inert tap; if it passes, the tap is
  delivering events in that launch context. Note that the context matters:
  a grant for the login-item launch does not cover a terminal launch.
- Do not replace the local signing identity unless you intend to grant access
  again. Rebuilding the app with the same identity is safe.

---

### Onboarding reappears after 120 seconds with Microphone already enabled

On 2026-09-19, the live log showed successful capture, followed by
`microphone released after 120s idle`, and then a new onboarding request.
The publisher treated `capture_wanted == false` as permission denied, even
though the daemon had intentionally released an authorized microphone.
Clicking Microphone was immediately undone by the expired idle timer.

The daemon now preserves successful capture evidence through intentional idle
and reopening. Only a real capture stop clears that evidence. Done and the
permission rows use the same decision. Initial model warmup no longer marks
Microphone Allowed without a successful capture; it only defers automatically
opening onboarding during normal startup. Idle release requires an active
capture and completed onboarding, and a microphone request refreshes the idle
timer. A regression test covers initial startup, capture, idle, reopen, and
capture failure.

The earlier wrong-first Accessibility prompt also required guarding
`CGEventTapCreate` itself with both permission preflights: creating the tap can
implicitly trigger macOS permission UI even without an explicit request.

## 2. launchd crash-loop (KeepAlive respawn storm)

### Symptom

`~/Library/Logs/sunoto/daemon.log` shows `daemon starting` repeating every
~10s with no `sidecar ready` between them. The daemon is crashing on startup
and launchd `KeepAlive: true` respawns it.

### Root cause (historical)

`TapHandle::drop` called `CFRunLoopStop` from a **different thread** than the
one running the CFRunLoop → PAC trap crash. The fix removed `CFRunLoopStop`;
the `stop` flag + `CFRunLoopWakeUp` exits the worker loop within ~0.25s.

### Fix / guard

- **Never reintroduce `CFRunLoopStop` in the hotkey `Drop` impl.** The
  `stop` AtomicBool + `CFRunLoopWakeUp` is sufficient and crash-free.
- To stop a live crash-loop: `launchctl bootout gui/$(id -u)/com.earendil-works.sunoto`
  then investigate before re-bootstrapping.
- After a crash loop the tap can come up inert (see §1); a clean restart fixes
  the tap state, but verify the TCC grant separately.

---

## 3. Empty transcript every time

### Symptom

`session N: ASR backend returned an empty transcript` for every session, even
with real speech. `rms` in the log is healthy (thousands), so the mic is fine.

### Root cause (historical)

`SidecarServer` converts wire i16 → float32 in [-1, 1) before calling
`engine.accept_audio`. The offline `_OfflineBufferEngine.accept_audio` did
`int(s)` on the float32, truncating every sample to 0 → silent WAV → empty
transcript.

### Fix / guard

- `accept_audio` must invert with `int(round(s * 32768.0))`, clamped to
  [-32768, 32767].
- The unit test `test_accept_audio_converts_sidecar_float32_to_i16` (in
  `tests/phase1/test_nemotron_offline_sidecar.py`) guards this. Do not weaken
  it; if it fails, the float32 contract was broken again.

### Also check (not a bug)

Low `rms` (hundreds, vs thousands for speech) → genuinely silent input. Check
System Settings → Sound → Input and the selected mic. This is correct ASR
behavior, not the float32 bug.

---

## 4. Transcribes but nothing appears (insertion)

### Symptom

`final transcript: "..."` is correct but no text lands in the focused app.

### Root cause / Fix

- macOS CGEvent per-character unicode typing
  (`CGEventKeyboardSetUnicodeString`) is unreliable across Cocoa apps — many
  ignore it. The macOS insertion path MUST **paste via clipboard first**
  (`pbcopy` + Cmd+V) and fall back to direct typing, mirroring Wayland's
  paste-first ordering. Healthy log line: `inserted via Pasted`. If you see
  `inserted via Typed` with no text, the paste-first ordering was lost.
- The focus target must be the real app (e.g. `Notes`), not `Window Server`.
  `frontmost_window_number` must skip system owners (`Window Server`, `Dock`,
  `SystemUIServer`); otherwise text targets the menubar/Dock and vanishes.

---

## 5. Config silently reverts / "stopped working" after restart

### Symptom

You set `backend`/`asr_device` in
`~/Library/Application Support/sunoto/config.json`, but after a daemon restart
it's back to a different value, and behavior changed.

### Root cause

- `install-macos.sh` only writes config when the file does NOT already exist.
  Re-running it does NOT overwrite your config. So install is not the culprit.
- The real cause is usually: the daemon was restarted (manually or by launchd
  KeepAlive after a crash) and you're observing a **different daemon instance**
  or a stale log. The config file itself is not rewritten by the daemon at
  runtime except via `config init` (only run by install when absent).

### Fix / guard

- After ANY daemon/sidecar/overlay change, restart cleanly and confirm you're
  reading the **real** log: launchd → `~/Library/Logs/sunoto/daemon.log`;
  manual `nohup` → the file you redirected to (we use `/tmp/sunoto-daemon.log`
  or `/tmp/sunoto-bare.log`). Do not confuse the two.
- Verify the running config: `python3 -c "import json;print(json.load(open('$HOME/Library/Application Support/sunoto/config.json'))['backend'])"`
- macOS recommended: `backend = "parakeet_mlx_streaming"` with `asr_device`
  unset (it is the config-init default on macOS). It uses parakeet-mlx + MLX on
  Apple GPU/Metal and streams live partials, then a direct-PCM final. The stable
  no-partials alternative is `parakeet_mlx_offline`. The older
  `nemotron_offline` CPU backend still works but is much slower; the streaming
  `nemotron` backend on CPU measured ~7.4s turnaround in live testing and
  cannot keep up — do not use it on macOS.

---

## 6. "Stopped working" right after a restart = warmup window

### Symptom

Daemon was just (re)started; Ctrl+F1 does nothing for ~30s, then works.

### Root cause

The recommended Parakeet-MLX sidecar is much faster than Nemotron CPU, but it
still has a short startup warmup while MLX loads the model and runs the first
transcription. On the M1 Pro benchmark, cached load + warmup was ~2.6s. The
older offline Nemotron sidecar takes **16–32s** to warm (torch import + model
load + warmup). During that window, Ctrl+F1 is silently ignored
(`push-to-talk ignored while ASR sidecar is loading` in the log). This is NOT a
bug — but it feels like "stopped working" if you don't wait for
`ASR sidecar ready: ... Hold Ctrl+F1 to dictate.`

### Fix / guard

- Always wait for the `ASR sidecar ready` line before judging hotkey behavior.
- `tail -f ~/Library/Logs/sunoto/daemon.log` and watch for it.
- (Future improvement: show a "warming up…" overlay state during sidecar load
  so the silent ignore is visible. Not yet implemented.)

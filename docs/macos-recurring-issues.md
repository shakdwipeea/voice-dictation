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
- `./target/release/sunoto-daemon check` says `global hotkey grab: ok` (lies —
  see below).

### Proof the daemon is healthy (run this first)

Send a synthetic press through the control socket:

```sh
(echo "press"; sleep 1; echo "release") \
  | nc -U -w3 /var/folders/*/T/sunoto-$(whoami)-daemon.sock
```

If the log then shows `session N: recording` → `sidecar accepted`, the **entire
daemon→sidecar→audio pipeline is fine** and the ONLY thing broken is the
physical-keyboard event tap. Stop here; it's §1, not the ASR.

### Root cause: TCC grant vs code-signature mismatch

- The CGEventTap (`crates/sunoto-macos/src/hotkey.rs`) calls
  `CGEventTapCreate` with `kCGEventTapOptionListenOnly`. On macOS 10.15+,
  capturing keyboard events requires **Input Monitoring** TCC permission
  (requested via `CGRequestListenEventAccess`).
- `CGEventTapCreate` can return **non-null** even without the permission, but
  macOS then **immediately disables the tap** — it receives zero events. This
  is the "inert tap". `check` only verifies the tap is *created*, not that
  events flow, so `check` gives a **false positive**.
- TCC grants are bound to the binary's **code signature (cdhash)** at grant
  time. The daemon is **adhoc-signed**, so **every `cargo build` produces a new
  cdhash and invalidates the previous grant**.
- Two executable identities exist:
  - **Bare binary** `target/release/sunoto-daemon`
    (cargo adhoc id `sunoto_daemon-…`). Its TCC grant is **path-based** and
    survives rebuilds on macOS 14 — this is the one that WORKS.
  - **App bundle** `target/release/Sunoto.app/…/sunoto-daemon`
    (id `com.earendil-works.sunoto`). Its grant is **bundle-id + cdhash**
    based and breaks every rebuild.
- The app bundle runs as `LSBackgroundOnly`/`LSUIElement` under launchd, and
  **background launchd agents do not get interactive TCC prompts**. So once the
  bundle-id grant is stale (or reset with `tccutil`), it **cannot be re-granted
  via a prompt** — the prompt is suppressed. System Settings toggling also
  fails to bind to the new cdhash reliably for adhoc bundles.

### Fix: run the BARE binary from a terminal (nohup), NOT via launchd

Two things matter, and BOTH are required:

1. **Run the bare binary** `target/release/sunoto-daemon`, not the app
   bundle. The bare binary's **path-based** TCC grant survives `cargo build`
   rebuilds on macOS 14; the app-bundle (bundle-id) grant is cdhash-bound and
   breaks every rebuild.
2. **Launch it from a terminal** (e.g. `nohup target/release/sunoto-daemon
   run > /tmp/sunoto-bare.log 2>&1 &`), **NOT via launchd**.
   launchd agents run with **no responsible process** (no parent app), so TCC
   treats them strictly: the tap is created but macOS **immediately disables
   it** (`[hotkey-diag] tap disabled by system; re-arming` repeats forever,
   zero events). A process launched from Terminal inherits Terminal's GUI/TCC
   session context and the tap stays enabled (`tap is_enabled=1`).

So the working macOS dev launch is:
```sh
pkill -f "sunoto-daemon run"
nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 &
tail -f /tmp/sunoto-bare.log   # wait for "ASR sidecar ready"
```

The launchd plist (`com.earendil-works.sunoto.plist`) and `install-macos.sh`
point at the bare binary (still better than the app bundle), but **the
launchd-launched tap is inert due to TCC context** — launchd is only useful
for auto-starting the sidecar warmup, not for a working hotkey. Until a GUI-
context launchd workaround is found, **use the nohup-from-terminal launch**
for actual dictation.

To get the bare binary's TCC grant in place (one-time, per machine):

1. Build: `cargo build --release -p sunoto-daemon` (produces the bare binary).
2. Run it once in the foreground so macOS prompts:
   `target/release/sunoto-daemon run`
   (or `bash install-macos.sh`, which opens the Privacy pane).
3. In the TCC prompts that appear, click **Open System Settings** and enable
   **Input Monitoring** AND **Accessibility** for `sunoto-daemon`
   (`/Users/.../voice-dictation/target/release/sunoto-daemon`).
4. If no prompt appears (already dismissed once), add the binary manually in
   System Settings → Privacy & Security → **Input Monitoring** (and
   Accessibility) via the **+** button, selecting the bare binary file.
5. Restart: `launchctl kickstart -k gui/$(id -u)/com.earendil-works.sunoto`.

Verify the grant:
```sh
sqlite3 /Library/Application\ Support/com.apple.TCC/TCC.db \
  "SELECT service,client,auth_value FROM access WHERE client LIKE '%sunoto-daemon%';"
# expect kTCCServiceListenEvent and kTCCServiceAccessibility with auth_value=2
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

- Do not run the app bundle (`Sunoto.app`) as the daemon on macOS dev.
- Do not run the daemon via launchd and expect the hotkey to work — the
  launchd TCC context disables the tap. Use `nohup ... &` from a terminal.
- Do not `tccutil reset ListenEvent com.earendil-works.sunoto` expecting a
  re-prompt — the launchd/background context suppresses it.
- Do not trust `sunoto-daemon check` alone to rule out this issue.
- Do not rebuild the app bundle and assume TCC still applies.

---

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

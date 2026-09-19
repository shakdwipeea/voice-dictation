# macOS permissions fix — status & remaining work (2026-09-19)

Tracks the permanent fix for the three recurring installation/permission
symptoms (see `docs/macos-recurring-issues.md` §1):

1. **TCC prompt windows sometimes never appear.**
2. **Prompts sometimes appear multiple times / stacked.**
3. **After granting full access, the app stays stuck at "hotkey blocked".**

Root causes identified:

- **Ad-hoc code signing.** Every build changes the cdhash; TCC binds the
  bundle's grants to (bundle id + cdhash), so upgrades invalidate grants
  silently (toggle shows ON, system denies). This is symptom 3, and it
  forces the reset-and-reprompt cycles behind symptoms 1 and 2.
- **Scattered, concurrent permission requests.** Three TCC requests fired
  from three background threads at startup (hotkey thread:
  `CGRequestListenEventAccess`; UI thread: `AXIsProcessTrustedWithOptions`
  plus a redundant `CGRequestPostEventAccess`; capture thread: mic
  prompt), re-fired by the installer's blind 15 s relaunch loop. macOS
  shows a prompt at most once per record state and can drop requests made
  off the main thread early in launch. Terminal commands (`check`,
  `selftest`, `insert`) also prompted under the *terminal's* TCC identity,
  adding a confusing second entry to the panes.

Chosen direction (user, 2026-09-19): **self-signed stable signing
identity per machine** (not Developer ID) + **full flow fixes** (single
request site, onboarding window, event-driven relaunch).

---

## Implemented — follow-up defects found during live use

The initial checks below did not establish that every permission lifecycle
worked. Follow-up testing found premature event-tap creation opening
Accessibility first, and idle microphone release reopening onboarding after
120 seconds despite an existing grant. Both have fixes in the working tree;
see `docs/macos-recurring-issues.md` §1. Fresh-grant, denial, and revocation
flows still need a complete interactive regression pass before calling the
permission implementation finished.

All of the following is in the working tree (uncommitted). Verified:
`cargo check --offline -p sunoto-ipc -p sunoto-macos -p sunoto-linux -p sunoto-desktop -p sunoto-daemon`
passes; `swiftc -O services/macos/sunoto-overlay.swift` builds.

### 1. Single, user-driven main-thread permission request site

- New `crates/sunoto-macos/src/permissions.rs`:
  - `request_input_monitoring()` and `request_accessibility()` are called
    separately by the daemon main loop after the matching onboarding click.
  - `permission_preflights()` moved here from `hotkey.rs`.
- `HotkeyListener::open` (`crates/sunoto-macos/src/hotkey.rs`) no longer
  calls `CGRequestListenEventAccess` — preflight only.
- `UiAdapter::open` (`crates/sunoto-macos/src/insertion.rs`) no longer
  calls `request_accessibility_with_prompt` / `CGRequestPostEventAccess`
  (the post-event request is redundant — same TCC service as
  Accessibility). `CGRequestPostEventAccess` removed from `ffi.rs`.
- Consequence: `sunoto-daemon check` / `selftest` / `insert` no longer
  prompt under the terminal's identity; they only report state.
- Linux: no-op permission request functions in
  `crates/sunoto-linux/src/x11/linux.rs` and `stub.rs` (keeps the
  `sunoto-desktop` facade surface identical).

### 2. Daemon startup + onboarding driver

- `apps/daemon/src/daemon.rs`:
  - `run()` only preflights at startup. Missing permissions never trigger
    automatic dialogs.
  - CoreAudio stays closed during first-run onboarding until the Microphone
    row is clicked, preventing its prompt from racing the keyboard grants.
  - New edge-triggered onboarding publisher: once a second it reads
    `permission_preflights()` plus mic state and sends
    `OverlayRequest::Onboarding { listen, accessibility, microphone }`
    **only when the state changed** while onboarding is active.
    macOS-only (`cfg!(target_os = "macos")`).
  - Microphone access is verified by successful capture and retained during
    intentional idle/reopening. Initial `Starting` is not a grant; model
    warmup only defers automatically opening the panel. `Unavailable` clears
    capture evidence. Done uses the same microphone decision as the rows.
  - Overlay `Ready` resets the published state so a respawned overlay
    gets a fresh copy.

### 3. Onboarding panel in the macOS overlay

- `crates/sunoto-ipc/src/lib.rs`: new
  `OverlayRequest::Onboarding { listen, accessibility, microphone }`
  plus typed `permission_action` / `onboarding_done` events and round-trip
  serialization tests.
- `services/macos/sunoto-overlay.swift`: new `OnboardingController`
  — titled floating panel listing Input Monitoring / Accessibility /
  Microphone as aligned status rows. Only the next
  missing row's **Allow** button is enabled and deep-links to
  `Privacy_ListenEvent` / `Privacy_Accessibility` / `Privacy_Microphone`.
  Activates the app only on first show. It remains open when all three are
  granted; **Done** becomes enabled and completes setup. The standard close
  button postpones setup without triggering more requests.
  The Linux GTK overlay ignores unknown message types, and the daemon
  gates sending to macOS anyway.

---

## Completed implementation

### A. Stable self-signed signing identity

Goal: `setup` creates (once per machine) a self-signed code-signing
certificate in the login keychain, signs `~/Applications/Sunoto.app`
with it, so TCC grants bind to the stable certificate hash and survive
every rebuild/upgrade. First upgrade after this lands re-prompts once;
never again after that.

The working route on macOS 26 is a LibreSSL-generated certificate with the
Code Signing extended-key-usage, imported as PKCS#12 into the login keychain,
followed by user-level `security add-trusted-cert -p codeSign`. The earlier
import failures were caused by the certificate not being trusted for code
signing; the key/certificate pair was present but
`find-identity -p codesigning` only reports valid identities. `certtool` was
rejected because its public options cannot create the required Code Signing
extended-key-usage on a clean machine.

| Approach | Result |
| --- | --- |
| `security import id.p12` (LibreSSL or OpenSSL 3 `-legacy` p12) into custom keychain without trust | "1 identity imported", but `find-identity -v` → **0 valid identities** |
| Same into the **login** keychain without trust | same failure (cert + key both present, still not a valid identity) |
| `SecItemCopyMatching(kSecClassIdentity)` with explicit `kSecMatchSearchList=[file keychain]` | **found it (count=1)** — the items are there; only the legacy pairing is blind to them |
| Same query over the **default** search list | `-25300` (not found) |
| Manual `SecItemAdd` of key/cert via Swift | `-25299` duplicate (ValueRef-only adds don't compute primary keys properly) |
| `security add-trusted-cert -p codeSign` against the custom keychain | hung on interactive authorization |
| `certtool c k=…` driven via printf | full prompt sequence cracked (label → algo → size → OK → usage → sig-algo → OK → 6 RDN fields → OK); ends with "cert stored in Keychain", but `find-identity -v` on the custom keychain still shows 0 |
| LibreSSL Code Signing certificate + PKCS#12 import + user-domain `add-trusted-cert` | **works**; `find-identity`, real `codesign`, and strict verification pass |

Implemented in `apps/daemon/src/setup.rs`: create-or-verify, fingerprint-based
selection, installed-bundle re-signing, ad-hoc fallback with a warning, and a
marker that resets old TCC records only when the signing identity changes.
`packaging/macos/build-app.sh` uses the identity when available; CI remains
ad-hoc and setup re-signs release bundles after installation.

The setup selects the valid identity by SHA-1 fingerprint, not common name, so
stale same-name test certificates cannot make `codesign` ambiguous.

### B. Removed the blind 15 s relaunch loop

`watch_until_ready` now keeps only crash recovery and one-time pane opening.
Real grant events drive daemon self-relaunch; setup guidance points to the
onboarding panel.

## Verification complete

### C. Verification (passed 2026-09-19)

- `cargo test --workspace --offline` passed.
- `cargo clippy --workspace --offline --all-targets -- -D warnings` passed.
- `python3 -m unittest discover -s tests/ui` passed.
- The optimized Swift overlay build passed.
- `scripts/macos-port/verify-all.sh` passed all 40 automated checks (five
  generic GUI checks remain marked manual by that script).
- First live install migrated the old ad-hoc app, showed one onboarding flow,
  detected the grants, self-relaunched from PID 65787 to PID 65874, and
  reported ready.
- A second rebuild and reinstall kept the same designated requirement
  (`certificate leaf = B7B77719D4A88C3F509F8539561F6DC096CBB22A`), did not
  require another grant, and returned directly to `hotkey: verified` and
  `ready`.

### D. Docs (complete)

- `docs/macos-recurring-issues.md` §1: rewrote the root-cause section
  (stable self-signed identity + single request site + onboarding
  panel); drop obsolete guidance once verified.
- `AGENTS.md` "macOS operations": updated permission-grant description.
- `docs/desktop-configuration.md`: updated the macOS permissions
  walkthrough (onboarding panel, what the user sees).

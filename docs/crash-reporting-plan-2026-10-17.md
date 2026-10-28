# Opt-in crash reporting (Sentry): plan

**Created:** 2026-10-17. Line numbers are at `64340b2b`.
**Status:** plan only. Nothing is implemented yet.
**Scope:** the Rust daemon (`apps/daemon`), macOS release builds first. Linux
compiles the same code but has no release pipeline, so it ships with
reporting unavailable (D7). The Python sidecars and the Swift overlay never
touch the network. The daemon reports on their behalf.

## 1. Goal

Find out when Sunoto breaks on someone else's Mac, without asking them for
logs and without sending anything they said. A report tells us:

1. that the daemon panicked, and where in our code;
2. that a helper (speech engine, AI editor, overlay) died or timed out;
3. that a dictation failed (engine timeout or error, nothing inserted);
4. that the shortcut stopped arriving, an update failed, or setup got stuck;
5. which Sunoto version, macOS version and Mac it happened on.

**Not in this version:** performance tracing, usage numbers (the daily
summary in `docs/local-history-plan-2026-10-16.md` §11), session replay,
feedback forms, native crashes that are not Rust panics (§9 phase 5), and
Linux release builds.

## 2. What exists today

**Logging and privacy**

- Logs are local only. `logging::{info,warn,error}` write timestamped lines
  to stderr (`apps/daemon/src/logging.rs:5`). The bundle launch points stderr
  at `~/Library/Logs/sunoto/daemon.log` (`main.rs:121-142`).
- The only network calls are `curl` subprocesses in the updater
  (`update.rs:194-200`, `:261-268`). There is no HTTP crate.
- Promises we must keep:
  - no dictated content in normal logs, and no network request during
    dictation (`docs/product-usability-and-release-plan-2026-07-15.md`
    §4.4, `:208`);
  - analytics only if explicitly opted in (`:321`);
  - "saved locally first, sent only if the user opted in", with a preview of
    exactly what would be sent (`docs/local-history-plan-2026-10-16.md`
    §11).

**Crashes and failures**

- **No panic hook.** There is no `set_hook` or `catch_unwind` anywhere.
- **Panics in the joined threads are swallowed.** Shutdown ignores their
  join results (`daemon.rs:1918-1923`).
- **Only two threads have names:** `history-writer` and `sunoto-update`.
  Panic messages from the rest read `thread '<unnamed>'`.
- **Nothing restarts the app after a crash.** The Login Item has no
  KeepAlive (`setup.rs:1228-1249`), and there is no crash-loop detection or
  "last run crashed" marker. A panicked app stays down until the next login
  or a manual open.
- **Sidecar exit codes and signals are thrown away.** `sunoto-ipc` reports
  only `Closed` (`crates/sunoto-ipc/src/lib.rs:241-243`), and `Drop` kills
  and waits without keeping the status (`:260-268`). The same is true of the
  LLM polish sidecar (`llm_polish.rs:651-659`).
- **There is no sidecar "ready" timeout.** Health can stay `loading_asr`
  forever (`daemon.rs:688-691` is the only ready handling).
- **Update failures collapse into one string.** Every check, download,
  verify and unpack error becomes `"update check failed: {error}"`
  (`update.rs:391`).
- **A failed rollback is silent.** If the rollback rename (`update.rs:512`)
  or the relaunch (`:517`) fails, `?` returns early, with no log line and no
  `record_outcome(false)`.

**Things we can build on**

- Every dictation ends in one classified outcome: `history::Status`
  (`history.rs:133`), with `AsrTimeout`, `AsrError`, `InsertError` and
  `Abandoned`.

**Release builds**

- Release builds have no debug info. There is no `[profile.release]`, so
  Cargo defaults apply (`debug = 0`, `strip = "debuginfo"`), and nothing
  runs `dsymutil`. `packaging/macos/build-app.sh:50` runs a plain
  `cargo build --release`.
- The repository is public, so uploading source context to Sentry exposes
  nothing new.

## 3. Decisions for the owner

| # | Question | Recommendation | Why |
| --- | --- | --- | --- |
| D1 | Service | **Sentry SaaS**, free Developer plan to start. | It groups crashes and symbolicates Apple stack traces, and it is free at our volume. We use only its public envelope endpoint, so GlitchTip or self-hosted Sentry work unchanged if we move. |
| D2 | Default | **Off.** Asked once by setup; no answer means off. | "Only if explicitly opted in." |
| D3 | SDK | **`sentry` crate 0.49, `default-features = false`, features `backtrace` and `debug-images`, with our own transport.** | We get proper stack frames and the image list that Sentry needs for symbolication, without the SDK's HTTP, TLS and tokio stack (no `reqwest`, no `ureq`). |
| D4 | Delivery | **Write each report to disk first, send later with `curl`** on a background thread. | It survives the crash that produced it, works offline, and lets `crash-reports preview` show the exact bytes. It also uses the same network pattern as the updater. |
| D5 | Panic message | **Not sent.** We send the location (`file:line`) and the stack; the message stays in the local log. | Panic messages can carry formatted values, for example `unwrap()` on an error that holds text. |
| D6 | Which builds report | **Only CI release builds.** The DSN is compiled in from a repository variable. Other builds can send only with an environment override. | No noise from dev builds, and every report matches uploaded symbols. |
| D7 | Linux | **Same code, no DSN**, until there is a Linux release pipeline. | Nothing to symbolicate against yet. |
| D8 | Where to ask | **In terminal setup, next to the LLM download question** (`setup.rs:615-618`, `ask_yes_no` at `:651-671`). This runs before the app starts and before the permission panel. | It is the existing opt-in pattern, and it comes before the permission steps, so setup-stuck reports work for first installs. A question in the Swift panel (`services/macos/sunoto-overlay.swift:1865`) is open question 1. |

## 4. Design

### 4.1 Files

| File | Change |
| --- | --- |
| `apps/daemon/src/crash_reports.rs` (new) | `init`, panic hook, `report(Report)`, `shutdown()`, the allowlist, the spool transport, the sender thread, rate limits. |
| `apps/daemon/src/crash_reports_cli.rs` (new) | `sunoto-daemon crash-reports …` (§4.8). Works on the spool directly, like `history_cli.rs`. |
| `apps/daemon/src/settings.rs` | `crash_reports: Option<bool>` in `Settings` (`:117-264`); default `None` (`:266-314`). |
| `apps/daemon/src/main.rs` | A new `crash-reports` arm in `dispatch` (`:151-212`). Call `crash_reports::shutdown()` before `std::process::exit` (`:112`), so an error that ends the process still reaches the spool. |
| `apps/daemon/src/daemon.rs` | `crash_reports::init` at the top of `run` (`:116`), which covers both the bundle launch (`main.rs:148`) and `run` (`main.rs:182`). Call sites in §4.3. `shutdown()` after `"shutting down"` (`:1902`). Name every spawned thread. |
| `crates/sunoto-ipc/src/lib.rs` | `exit_status(&mut self) -> Option<ExitStatus>` (a non-blocking `try_wait`) on the sidecar handle. |
| `crates/sunoto-macos/src/hotkey.rs` | `hotkey_block_reason` (`:276-288`) returns an enum (`NotGranted` / `NoEvents`) that implements `Display` for today's text. Count tap re-arms (§4.3). |
| `apps/daemon/src/setup.rs` | The question (§4.9), flags, and a line in `print_status` (`:1399-1455`). |
| `apps/daemon/src/update.rs` | Typed failure stages (§4.3), and the silent-rollback fix. |
| `apps/daemon/Cargo.toml` | `sentry = { version = "0.49", default-features = false, features = ["backtrace", "debug-images"] }`. Fetch it once with network; `--offline` builds work again after that. |
| `Cargo.toml` | `[profile.release]` debug info (§5). |
| `.github/workflows/release.yml` | Compile in the DSN, check symbol UUIDs, upload the dSYM (§5). |
| `README.md`, `docs/desktop-configuration.md` | The privacy note, the setting, and the CLI. |

### 4.2 What a report may contain (the allowlist)

`before_send` does not delete fields from the SDK's event. It **builds a new
event** and copies only the fields below, so anything a later SDK version
adds is dropped by default.

| Field | Content |
| --- | --- |
| `event_id`, `timestamp`, `level`, `platform` | As the SDK sets them (`platform: "native"`). |
| `release` | `sunoto@0.1.11` (`CARGO_PKG_VERSION`). |
| `environment` | `release`. |
| `tags` | `kind` and `subkind` (fixed strings, §4.3); `process` (`daemon`/`setup`/`updater`); `asr_backend` (one of the known backend names, else `other`); `overlay_style`; `llm_polish` (`on`/`off`); `launch` (`app`/`bare`). |
| `contexts.os` | `name: "macOS"`, plus `version` and `build` from `kern.osproductversion` and `kern.osversion`. |
| `contexts.device` | `arch`; `model` (`hw.model`, for example `MacBookPro18,3`); `chip` (`machdep.cpu.brand_string`); memory in whole GB. Read once by `init` with one `sysctl -n …` call at startup, never on the latency path. |
| `contexts.sunoto` | The event's numbers only: durations in ms, exit code, signal, counts, uptime in seconds. |
| `exception` | Panics only: `type: "panic"` and `value: "at apps/daemon/src/x.rs:123"`. Each stack frame keeps `function`, `instruction_addr`, `in_app`, `lineno`, and `filename` relative to the workspace; `abs_path` is removed. |
| `debug_meta.images` | Panics only, and only the images a frame points into. `code_file` is cut to the file name, so `/Users/<name>/Applications/…` never leaves the Mac. |
| `fingerprint` | `[kind, subkind]`, so each failure kind groups into one issue. |

**Never in a report:**
- dictated text, audio, history records, or the clipboard;
- word lists and snippets;
- focused app ids or names, or window titles;
- any path under the home folder, the user name, or the hostname
  (`server_name`);
- breadcrumbs (`max_breadcrumbs: 0`);
- free-form error strings. Sidecar error messages, curl stderr and
  `io::Error` text are reduced to an enum or an `io::ErrorKind` name before
  they reach `report`.

**The type does the work.** `Report` is an enum whose variants hold only
integers, booleans and our own `Copy` enums. **It has no `String` field**, so
a future call site cannot pass text by accident. A unit test serializes
every variant and checks the field names against this table.

**Sentry still sees the sender's IP address** at the network level. We turn
off "Store IP addresses" and keep server-side data scrubbing on in the
project, and the privacy note says this plainly.

### 4.3 Events

Paths are under `apps/daemon/src/` unless they name a crate.

| `kind/subkind` | When | Numbers in `contexts.sunoto` | Call site |
| --- | --- | --- | --- |
| `panic` | Any thread panics, including the joined threads whose panics shutdown swallows and the FFI callbacks that abort (`tap_callback`, `hotkey.rs:137`; CoreAudio `io_proc`, `sunoto-audio/src/macos.rs:595`). | thread name, uptime | The hook installed by `init` (`daemon.rs:116`). |
| `asr_sidecar/exited` | The speech engine closes when we did not stop it. | exit code, signal, phase (`warming`/`idle`/`recording`/`finishing`), uptime, restarts in the last hour | Noted at `Closed` (`daemon.rs:1145-1160` → `handle_sidecar_loss`, `sidecars.rs:45-73`). Reported at respawn (`daemon.rs:1883-1899`), where `exit_status()` reads the old process before it is dropped. The backoff of 500 ms or more means the status is ready, so nothing blocks. |
| `asr_sidecar/respawn_failed` | A respawn errors. | `io::ErrorKind`, attempt | `daemon.rs:1894` |
| `asr_sidecar/start_failed` | The first spawn fails, and `run` returns an error. | `io::ErrorKind` | `sidecars.rs:37`, `:40` via `daemon.rs:216`. Written to the spool by `shutdown()` before `main.rs:112` exits. |
| `asr_sidecar/not_ready` | **New check:** no ready message 180 s after a spawn. Report only; behaviour does not change. | waited s | A deadline set at spawn, cleared at `daemon.rs:688-691`, checked in the loop's timer pass. |
| `dictation/asr_timeout` | The transcribe watchdog fires. | recorded ms, timeout ms | `daemon.rs:1838-1859` |
| `dictation/asr_error` | The engine returns an error for a session. The message is not sent. | recorded ms | `daemon.rs:1114-1119` |
| `dictation/insert_error` | Insertion returns `Err`. | error enum | `daemon.rs:1578-1585` |
| `dictation/abandoned` | A new dictation begins while one is still open. | — | `history.rs:364` (the stale draft). Seen only while history is on, which is the default. |
| `insert/paste_failed` | The macOS paste failed and we typed instead. | — | `insertion/macos.rs:73-76` (UI thread; `report` is a non-blocking `try_send`, §4.5) |
| `insert/clipboard_only` | Paste and typing both failed; the text is left on the clipboard. | — | `insertion/macos.rs:78-80` |
| `insert/streaming_fallback` | Streaming typing failed; the result was committed by clipboard. | — | `insertion/mod.rs:315-325` |
| `ui/fatal` | The UI backend cannot open, and the daemon stops. | — | `insertion/mod.rs:255-260` |
| `polish/unavailable` | The polish sidecar failed to spawn or warm up; polish is off for this run (it is never respawned). | stage (`spawn`/`warmup`) | `daemon.rs:233`, `:1951-1958` |
| `polish/skipped` | One request failed, and the rule-based text was used. | cause (`timeout`/`exited`/`error`/`empty`), waited ms | `daemon.rs:1029-1031`. The causes come from `llm_polish.rs:644`, `:539`/`:646`, `:513` and `:470`; return them as an enum instead of a string. |
| `overlay/exited` | The overlay helper exits and is respawned. | exit code, signal, uptime | `daemon.rs:1305`; respawn failure at `:1877` |
| `hotkey/inert` | A `Blocked` verdict whose reason is `NoEvents` (Accessibility granted but no key events: recurring-issues §1). `NotGranted` is normal before onboarding and is not reported. | minutes since the last key event | `daemon.rs:395-399` |
| `hotkey/tap_rearmed` | macOS disabled the tap and we re-armed it. | how (`timeout_callback`/`poll`), count this hour | The callback (`hotkey.rs:146-156`) only bumps an atomic counter. The 1 s poll (`:503-521`) sends a new `HotkeyEvent::Rearmed` through the existing channel, and the daemon reports it. |
| `update/failed` | Download HTTP error, signature check, unpack or bundle check failed. **Not** the check step's network-unreachable cases (curl exit 6, 7 or 28): laptops are often offline. | stage (`check`/`download`/`verify`/`unpack`), curl exit code, HTTP status | `update.rs:391`. Wrap errors with their stage at `:202-205` and `:167-180` (check), `:270` (download), `:223-256` (verify), and `:308-334` (unpack). |
| `update/start_failed` | The installer could not start. | — | `daemon.rs:1789-1792` |
| `update/install_failed` | The new setup failed and the old version was restored. | stage (`setup_exit`/`setup_timeout`) | `update.rs:504-506`, `:541`, `:547`. This runs in the `apply-update` process (`main.rs:199-201`), which writes to the spool. The restored app sends it. |
| `update/rollback_failed` | Restoring the old app failed. | stage (`rename`/`relaunch`) | `update.rs:512`, `:517`. Today these return silently; log, report and `record_outcome(false)`. |
| `setup/permission_wait` | **New check:** the onboarding panel has been open 5 minutes without every grant. Once per launch. | permission (`accessibility`/`microphone`), waited s | Beside the 1 s grant poll (`daemon.rs:1705-1731`) |
| `setup/not_ready` | Setup gave up waiting for the app. | waited s, health name | `setup.rs:1353-1356` (setup process, spooled) |
| `setup/app_not_answering` | Setup relaunched an app that did not answer for 12 s. | — | `setup.rs:1291` |
| `test` | `sunoto-daemon crash-reports test`. | — | `crash_reports_cli.rs` |

Dictation outcomes go through one helper: `finish_session` in `daemon.rs`
calls `history.finish(...)` and then `crash_reports::session_outcome(status, …)`.
It replaces the current `history.finish` calls (`daemon.rs:360`, `:1005`,
`:1084`, `:1117`, `:1556`, `:1583`, `:1849`), so the dictation rows above
work whether history is on or off. `session_outcome` ignores the success
statuses (`Inserted`, `ClipboardOnly`, `SecureField`, `Empty*`,
`Cancelled`).

### 4.4 Rate limits

Kept in `crash-reports/state.json`:

- at most one report per `(kind, subkind)` per 10 minutes;
- at most 5 per kind and 20 in total per day (panics count too);
- after a `429`, nothing is sent until the `Retry-After` time (one hour if
  the header is missing);
- the spool holds at most 50 files, and unsent files older than 7 days are
  deleted.

So a flapping tap or a sidecar restart loop (respawns retry forever with a
5 s backoff cap, `sidecars.rs:24-26`) costs a handful of events, not our
monthly quota.

### 4.5 Spool and sending

**Where:** `~/Library/Application Support/sunoto/crash-reports/` (`pending/`
plus `state.json`). `uninstall --purge` already removes that folder.

**Transport:** `SpoolTransport` implements `sentry::Transport`;
`send_envelope` serializes with `Envelope::to_writer`.
- While panicking (`std::thread::panicking()`), or after `shutdown()` has
  begun, it writes the file synchronously (temp file + rename), because the
  process is about to exit.
- Otherwise it hands the envelope to the sender thread through a bounded
  channel with `try_send`, so the event loop, UI thread and audio path never
  wait on disk or network. A full channel drops the report and counts it.

**Sender thread** (`crash-reports`, daemon process only): it writes queued
envelopes to `pending/`, then sends every pending file, oldest first. Before
each send it re-reads `crash_reports` from the config file, so
`crash-reports off` takes effect without a restart.

```
curl --silent --show-error --max-time 15 -o /dev/null -w '%{http_code}' \
  -H 'Content-Type: application/x-sentry-envelope' \
  -H 'X-Sentry-Auth: Sentry sentry_version=7, sentry_key=<public key>, sentry_client=sunoto/0.1.11' \
  --data-binary @pending/<file> <dsn.envelope_api_url()>
```

What happens to the file after the send:

| Result | File |
| --- | --- |
| `2xx` | Deleted. |
| `429` | Kept; sending waits for the backoff (§4.4). |
| Other `4xx` | Deleted (resending will not help). |
| `5xx` or network error | Kept for the next try: the next start, or after one hour. |

**Setup and updater processes** spool only and never send; the app sends
their files on its next start.

**Panic hook:** it is installed in front of the existing hook, which still
runs, so the panic stays in the local log. An `AtomicBool` stops a panic
inside reporting from reporting itself.

**Off means off:** unless `crash_reports` is `true`, `init` returns early.
There is no client, no hook, no thread and no `crash-reports/` folder.
Turning reports off deletes `pending/`.

### 4.6 DSN

- **Compiled in:** `option_env!("SUNOTO_SENTRY_DSN")`. `release.yml` sets it
  from the repository variable `SENTRY_DSN`. A DSN only allows sending
  events, so it is not a secret.
- **Test override at run time:** `SUNOTO_CRASH_REPORT_DSN` (for the
  installed app, set with `launchctl setenv` like `SUNOTO_UPDATE_FEED`).
  `SUNOTO_CRASH_REPORT_SEND=0` spools without sending.
- **No DSN and no override:** `status` says
  `crash reports: not available in this build`, and setup does not ask.

### 4.7 Settings

| Key | Default | Meaning |
| --- | --- | --- |
| `crash_reports` | unset | Unset means never asked, and is treated as off. It becomes `true` or `false` once the user chooses. |

`setup --update` never asks and never changes it, the same as the LLM
question (`setup.rs:287-291`). Older installs call newer setups with
`--update` (AGENTS.md, "Releases and automatic updates").

### 4.8 Command line

```
sunoto-daemon crash-reports status     on/off/unavailable, reports waiting, last sent
sunoto-daemon crash-reports on | off   off also deletes unsent reports
sunoto-daemon crash-reports preview    prints every waiting report exactly as it will be sent
sunoto-daemon crash-reports test       queues a kind=test report, to check the whole path
sunoto-daemon crash-reports clear      deletes unsent reports
```

`sunoto-daemon status` gets one more line, from a `crash_reports` field in
the status payload (`daemon.rs:1630-1685`): `crash reports: on (2 waiting)`,
`crash reports: off`, or `crash reports: not available in this build`.

### 4.9 Setup question

The question goes after the LLM download question and uses the same
`ask_yes_no`. It has flags `--crash-reports` and `--no-crash-reports`
(parsed next to `--with-llm`, `setup.rs:200-201`). With no terminal, it
prints a note and leaves the setting unset.

```
Send crash reports? If Sunoto crashes or a dictation fails, it can send a
short technical report: the Sunoto and macOS versions, your Mac's model and
chip, and where in Sunoto's code the problem happened. It never sends what
you say, your history, or which apps you use. You can change this later with
`sunoto-daemon crash-reports on|off`. [y/N]
```

## 5. Release workflow: readable stack traces

1. **Debug info in release builds** (workspace `Cargo.toml`):

   ```toml
   [profile.release]
   debug = "line-tables-only"
   split-debuginfo = "packed"
   ```

   On macOS, `packed` makes Cargo run `dsymutil` and write
   `target/release/sunoto-daemon.dSYM`. The binary keeps its symbol names,
   and the file and line tables go in the dSYM.
   - Measure the build-time cost (the `dsymutil` step) and the binary size
     before and after.
   - If local builds (`install-macos.sh:53`) feel slower, move this to a
     `[profile.dist]` that only `build-app.sh` uses.

2. **`release.yml`.** Give the existing build step the DSN, then add two
   steps between "Build Sunoto.app" (`:28-29`) and the minisign step
   (`:33`):

   ```yaml
   - name: Build Sunoto.app
     env:
       SUNOTO_SENTRY_DSN: ${{ vars.SENTRY_DSN }}
     run: bash packaging/macos/build-app.sh
   - name: Check the debug symbols match the shipped binary
     run: |
       shipped="$(dwarfdump --uuid dist/Sunoto.app/Contents/MacOS/sunoto-daemon | awk '{print $2}')"
       symbols="$(dwarfdump --uuid target/release/sunoto-daemon.dSYM | awk '{print $2}')"
       echo "binary $shipped, dSYM $symbols"
       test -n "$shipped" && test "$shipped" = "$symbols"
   - name: Upload debug symbols to Sentry
     if: startsWith(github.ref, 'refs/tags/v')
     env:
       SENTRY_AUTH_TOKEN: ${{ secrets.SENTRY_AUTH_TOKEN }}
       SENTRY_ORG: ${{ vars.SENTRY_ORG }}
       SENTRY_PROJECT: ${{ vars.SENTRY_PROJECT }}
     run: |
       brew install getsentry/tools/sentry-cli
       sentry-cli debug-files check target/release/sunoto-daemon.dSYM
       sentry-cli debug-files upload --include-sources --wait target/release/sunoto-daemon.dSYM
   ```

   - `build-app.sh` copies the binary (`:92`) and then signs the bundle
     (`:98-104`). Signing does not change the binary's UUID, and the check
     step proves that on every build.
   - The upload comes before "Attach to release", so a release never goes
     out without its symbols.
   - Pin the `sentry-cli` version when implementing.

3. **One-time Sentry setup:**
   - a project `sunoto` (platform: native);
   - an organization auth token as the secret `SENTRY_AUTH_TOKEN`;
   - the repository variables `SENTRY_DSN`, `SENTRY_ORG` and
     `SENTRY_PROJECT`;
   - project settings: "Store IP addresses" off, data scrubber and default
     scrubbers on, spike protection on;
   - client option `in_app_include = ["sunoto"]`, so grouping follows our
     frames and not the standard library's.

## 6. Privacy rules (each one tested)

- With the setting unset or `false`, there is no network request, no
  `crash-reports/` folder, and no extra thread.
- No report contains the dictated text, `$USER`, the hostname, or `$HOME`.
- `preview` shows byte-for-byte what will be sent.
- Turning reports off deletes everything still waiting, and stops sending
  without a restart.
- The README privacy note lists §4.2 in plain words, and says that Sentry
  sees the IP address but does not store it.

## 7. Testing

**Unit tests (`cargo test`):**
- **The allowlist.** Start from an event with `user`, `server_name`,
  `request`, `extra`, `breadcrumbs`, a `message` holding a canary string,
  and frames whose `abs_path` is under `/Users/x`. Only the §4.2 fields
  survive, and the canary is gone.
- **Panics:** the event has the location and no message, and `code_file`
  and `filename` contain no home path.
- **`Report`:** every variant serializes to allowed field names only.
- **The rate limiter:** the per-kind window, the daily caps, and the `429`
  backoff.
- **The spool:** atomic writes, and the 50-file and 7-day limits.
- **`update.rs`:** each failure stage maps to its subkind, and curl exits
  6, 7 and 28 at the check stage are not reported.
- **Settings:** unset means off, and `init` creates nothing.

**End-to-end privacy test** (`tests/phase2`, stdlib `unittest`):
1. Run a fake Sentry on `127.0.0.1` with `http.server`, recording each
   request body.
2. Start the daemon with the mock backend,
   `SUNOTO_CRASH_REPORT_DSN=http://key@127.0.0.1:<port>/1`, and reports on.
3. Run a dictation whose mock transcript is a canary phrase, into a
   disposable target window (AGENTS.md, "Runtime safety").
4. Cause failures: kill the ASR sidecar with `SIGKILL`, force a watchdog
   timeout with a tiny `final_timeout_ms`, and run `crash-reports test`.
5. Assert that the expected kinds arrived, and that no body contains the
   canary, `$USER`, the hostname, `$HOME` or `/Users/`.
6. Repeat with reports off, and assert that zero requests arrive.

**Panic path:** `crash-reports test --panic`, in debug builds only, runs a
child process that panics on a named thread. Check that the report is in
`pending/` before the child exits, and is sent on the next start. Repeat
with the panic inside an `extern "C"` callback, which aborts.

**Release check:** after the first tagged release, run
`crash-reports test --panic` on an installed app, with
`SUNOTO_CRASH_REPORT_DSN` pointing at the real project. In Sentry, confirm
that the frames show our function names and `file:line`.

**Gates:** `cargo clippy --workspace --offline --all-targets -- -D warnings`
and `make test` pass.

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| A later change sends text by accident | No `String` in `Report`; the allowlist builds a new event; the canary test runs in `make test`. |
| A flapping tap or a crash loop floods Sentry | The §4.4 limits, plus Sentry spike protection. |
| Reporting slows a dictation | Normal reports are a non-blocking `try_send`; disk and curl work happens on the sender thread. Only a panic or `shutdown()` writes synchronously. |
| A panic inside the reporter | The `AtomicBool` guard. The previous hook still runs, so the local log keeps the panic. |
| Symbols do not match the shipped binary | The UUID check step fails the release. |
| `setup --update` breaks on old installs | It never asks and never touches the setting. |
| Offline laptops produce update noise | Network-unreachable check failures are not reported. |
| Users read "crash reports" as spying | Off by default, a plain question, `preview`, and a short allowlist in the README. |

## 9. Phases

1. **Local only.**
   - The setting, `crash_reports.rs` with the spool transport, the
     allowlist, the panic hook, `shutdown()` in `main.rs`, and named
     threads.
   - The `status`, `preview`, `test` and `clear` commands.
   - No DSN and no sending yet. All unit tests.
2. **Events.**
   - `exit_status()` in `sunoto-ipc`, the `BlockReason` enum, the re-arm
     counter, the update stages and the rollback fix.
   - The `finish_session` helper, every call site in §4.3, and the rate
     limits.
   - Check each event with `preview` on the dev machine.
3. **Symbols.** The `[profile.release]` change and the `release.yml` steps.
   This can land on its own, before or after phase 2.
4. **Sending and setup.**
   - The curl sender and backoff, the setup question, and the README and
     `desktop-configuration.md` changes.
   - The Sentry project settings and the end-to-end privacy test.
   - Then tag a release and run the release check (§7).
5. **Later.**
   - **Native crashes.** At startup, read new
     `~/Library/Logs/DiagnosticReports/sunoto-daemon-*.ips` files. Send the
     exception type, the signal, and the crashed thread's frames as image +
     offset only. The control socket file that an unclean exit leaves
     behind (`control.rs:35-38`) is a cheap "last run died" signal to pair
     with it. SIGTERM already shuts down cleanly (`daemon.rs:102-114`).
   - **Sidecar crashes.** A typed `fatal` NDJSON message in `sunoto-ipc`,
     with the Python exception class and code location, sent before the
     sidecar exits.
   - **A setup question in the onboarding panel,** for installs that never
     see a terminal.
   - **The numbers-only daily summary** from
     `docs/local-history-plan-2026-10-16.md` §11.

## 10. Open questions

1. Should the Swift onboarding panel also ask, for people who open the app
   from the zip without running setup in a terminal? Until it does, they
   stay off.
2. Should existing installs be asked once after an update (on a later
   launch, never inside `setup --update`), or only when they run setup
   again?
3. Who on the team gets access to the Sentry project, and is the plan's
   default event retention enough?
4. Is `hw.model` worth sending? It is coarse, and it helps with notch-only
   overlay bugs. Recommendation: yes.
5. Separate from reporting: nothing restarts Sunoto after a crash (no
   KeepAlive, no crash-loop guard). Reports will tell us how often that
   happens. Should recovery be its own plan?

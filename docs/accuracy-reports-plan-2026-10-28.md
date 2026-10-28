# Accuracy reports (Sentry): plan

**Created:** 2026-10-28.
**Status:** steps 1 and 2 implemented and tried live 2026-10-29
(`apps/daemon/src/diag.rs`, `apps/daemon/src/report.rs`, the overlay's
`ReportPanelController`). Step 3 implemented 2026-10-29
(`apps/daemon/src/report_send.rs`, §4 step 3 "As built"); a sample report
reached the `sunoto` Sentry project. The re-transcribe check (needs kept audio, open
question 1) is not started.
**Scope:** macOS. Builds on `docs/crash-reporting-plan-2026-10-17.md`
(same Sentry project, same opt-in, same save-to-disk-then-send transport).

## 1. Goal

Some users say transcription gets worse when many windows are open. We want
to find out why on their Macs, without asking them for log files and without
sending anything they said unless they choose to.

## 2. What we suspect

| # | Suspect | Why | Not visible today because |
| --- | --- | --- | --- |
| S1 | Word list reads busy windows | When the text box is short (<300 chars) we read the whole focused window, up to 1,500 elements (`crates/sunoto-macos/src/accessibility.rs:106`). Those words get the strongest bonus, 7.0 (`services/asr/word_boost.py:28`). | The log counts words per source only, not whether the window was read or how often the bonus changed a word. |
| S2 | GPU shared with other apps | Browsers, video and Electron apps share the Apple GPU with MLX; MLX emits `<unk>` runs when the GPU is shared. | The sidecar's retry is not tied to the dictation. |
| S3 | Mic changes | A Meet or Zoom tab can switch a Bluetooth headset to call quality or change the default input. | Mic name, type and sample rate are not logged per dictation. |
| S4 | Audio lost under load | The CoreAudio callback ignores the input timestamp (`crates/sunoto-audio/src/macos.rs:595`). | Gaps leave no trace. |

## 3. The flow

1. Every dictation saves a record of counts (§4) with its history entry.
2. The user clicks **This was wrong** on an item in Recent Dictations.
3. A small panel lists what will be sent, with one **Send** button.
   Dictated text and audio are included only if the user ticks a box.
4. The report is written to disk, then uploaded in the background.
   If the Mac is offline it is sent later.
5. Sentry groups it; the panel shows a short report code (`SUN-7F3A`) the
   user can quote if they write to us.

## 4. Steps

Each step ships on its own.

### Step 1: per-dictation record

Add a `diag` object to the history `Record` (`apps/daemon/src/history.rs:227`).
Counts and fixed names only, never words, titles or paths.

| Area | Fields |
| --- | --- |
| Word list | words per source (screen / project / everywhere); whether the whole window was read; elements and characters read; read time; tokens where the bonus changed the pick (sidecar reports it); near-miss fixes applied |
| Audio | mic type (built-in / USB / Bluetooth / other); sample rate; share of clipped samples; timestamp gaps; mic changes during the dictation; whether another app was using the mic (`kAudioDevicePropertyDeviceIsRunningSomewhere`) |
| System | on-screen window count (`CGWindowListCopyWindowInfo`, count only); GPU use (IOKit `IOAccelerator` "Device Utilization %"); memory pressure level; thermal state |
| ASR | decode time; pieces; `<unk>` retries; whether the pause decode was reused |

Also log a one-line summary of these counts in `daemon.log`, so the record
is useful before anything is sent.

Privacy fix in the same step: the word-list log line includes the project
folder name (`SessionWords::summary`, `apps/daemon/src/vocabulary.rs`). Drop
it from the log.

Gathering must stay off the latency path: read system values on the
existing `word-list` thread or once per dictation after insertion.

### Step 2: "This was wrong" and the send panel

- Menu bar: each Recent Dictations item gets **This was wrong…**
  (`apps/daemon/src/menu_bar.rs`, overlay `MenuBarController`).
- Panel in the overlay (`services/macos/sunoto-overlay.swift`): a short list
  of what is included, two checkboxes off by default (**Include the text**,
  **Include the audio**), **Send** and **Cancel**.
- Optional: **Transcribe again without the word list** in the same panel.
  Needs the audio, so it works only for dictations whose audio was kept
  (an opt-in "keep audio of my next 10 dictations"). Its result goes in the
  report as "same" or "differs by N words".

### Step 3: send to Sentry

- Reuse the crash-reporting transport: write the envelope to the spool,
  send with `curl` on a background thread, rate-limit.
- Event `kind = accuracy`, `subkind = flagged`, tags: Sunoto version, macOS
  version, Mac model, ASR backend, mic type, word list on/off.
- The `diag` record goes in `contexts.sunoto`. Text and audio, when ticked,
  go as attachments, never in tags or the event body.
- The panel shows the report code once the report is spooled; it does not
  wait for the network.
- Same opt-in as crash reports. If the user has not opted in, the panel
  asks once.

**As built (2026-10-29).** Crash reporting was never implemented, so there
was no transport to reuse; `report_send.rs` is a small one in the same
shape (spool, `curl`, background thread), with no `sentry` crate.

- **Opt-in:** a new setting `send_reports` (unset / `true` / `false`), meant
  to be the shared opt-in when crash reports land. The panel has **Send**
  and **Keep on This Mac**; the first time, a note says what Send does and
  that the choice is remembered. After that the last choice is the default
  button. Send saves to `pending/` and wakes the sender; Keep saves to
  `kept/`, which nothing uploads. A build with no DSN shows only **Save**.
- **Envelope:** one event (`platform: other`, `level: info`,
  `release: sunoto@X.Y.Z`, `fingerprint: [accuracy, flagged]`), tags `kind`,
  `subkind`, `report_code`, `sunoto_version`, `macos_version`, `mac_model`,
  `asr_backend`, `mic`, `word_list`, `text_attached`; `contexts.sunoto` is
  the report's allowlisted `dictation` object; `contexts.os` and
  `contexts.device.model`. No `user`, `server_name`, `request`, `extra` or
  breadcrumbs. The ticked text is a `text.json` attachment. Each report
  carries a random `event_id` saved with it, so a resend is the same event.
- **Sending:** the `sunoto-reports` thread starts only when a DSN exists and
  `send_reports` is `true` (30 s after launch, or at once from the panel),
  re-reads the setting before each run, and sends oldest first. 2xx deletes
  the file; 429 waits for `Retry-After` (an hour if missing); another 4xx
  moves it to `failed/`; 5xx or no network keeps it and tries again after
  1 minute, then 2, 4, 8 ... up to hourly.
  At most 20 uploads a day (`state.json`).
- **DSN:** compiled in from `SUNOTO_SENTRY_DSN`, which `release.yml` sets
  from the repository variable `SENTRY_DSN`; `SUNOTO_REPORT_DSN` overrides
  it at run time for tests.
- **Sentry project:** `antash-mishra/sunoto` (EU region, created
  2026-10-29). A sample report (`send_a_sample_report`, an ignored test in
  `report_send.rs`) arrived as SUNOTO-1 with the expected tags and
  contexts, and with `user.geo` filled in from the sender's IP: "Prevent
  Storing of IP Addresses" must be on in the project's Security & Privacy
  settings.
- **Tried live 2026-10-29** on the installed app: Send (SUN-4C0C arrived
  with counts only, `text_attached: no`); Keep on This Mac with the text
  ticked (SUN-7E3B stayed in `kept/`, never in Sentry); offline, with the
  app's HTTPS sent to a closed local proxy port (SUN-1E2E waited in
  `pending/`, then went 60 s after the proxy came up, with no restart).
  `SENTRY_DSN` is set as a repository variable.
- **Still to do before shipping:** turn on "Prevent Storing of IP
  Addresses" in the project. Reports saved to `pending/` by step 2 builds are sent once the user
  chooses Send.

### Step 4: find the cause

In Sentry, compare flagged dictations against the suspects:

| If flagged dictations mostly have… | Then |
| --- | --- |
| whole-window reads, many screen words, or "differs without word list" | S1: lower the walk limits or the screen bonus |
| high GPU use or `<unk>` retries | S2 |
| Bluetooth mic, another app on the mic, or mic changes | S3 |
| timestamp gaps | S4 |

## 5. What is never sent

- Dictated text or audio, unless ticked for that one report.
- Word-list words, window titles, app window contents, file paths, user
  name, hostname.
- Free-form error strings.

Sentry still sees the sender's IP address; turn off "Store IP addresses" in
the project, as in the crash-reporting plan.

## 6. Test before shipping

- Unit test: serialize a `diag` record and check its field names against §4.
- Unit test: a report without the boxes ticked holds no text field.
- Manual: flag a dictation with the network off, turn it back on, and check
  the report arrives in Sentry with the shown code.
- Manual: dictate with many windows open and with the word list off, and
  compare the records.

## 7. Open questions

1. Keep audio for the re-transcribe check: how many dictations, and for how
   long? (Proposed: next 10, deleted after 24 h or once sent.)
2. Does the send panel live in the onboarding-style Swift panel or in a new
   small window?
3. Do we want an "always send when I mark a dictation" setting?

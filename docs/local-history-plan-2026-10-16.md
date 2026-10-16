# Local dictation history: plan

**Created:** 2026-10-16. **Updated:** 2026-10-16. Audio was moved to a later
version, so this version saves text and numbers only. Added the pill's Fix
and Improve buttons (§6). Improve runs on our own small grammar model, so it
works on every device; Apple's model is optional.
**Status:** step 1 (text history) is implemented in `apps/daemon/src/history.rs`
and `history_cli.rs`; the other steps are not yet. D4 (`log_transcripts`
off) waits until the Improve test (step 2) has read its sample dictations
from the log.
**Scope:** the daemon (`apps/daemon`), macOS first. Linux gets the same text
history for free because the code path is shared; the undo signal is
macOS-only. System mode is out of scope.
**Plain-language summary:** `docs/local-history-plan.html`.

## 1. Goal

Keep a private, on-device record of every dictation's text and timings,
so we can:

1. measure speed and failures on real use, per version and backend;
2. turn real dictations into test sets for `sunoto-daemon eval`;
3. learn the user's vocabulary (word-list suggestions);
4. collect "what the AI editor wrote" against "what the user fixed" for the
   LLM polish;
5. add two buttons to the pill after a paste. **Fix** lets the user type
   what they really said, so Sunoto learns from it. **Improve** fixes
   spelling and grammar in place, without loading Sunoto's own AI model.

**Not in this version:** saving audio, a history window for users, and
observability for the team. All three are in §11.

## 2. What exists today

- `log_transcripts` defaults to `true` (`apps/daemon/src/settings.rs:289`),
  so `daemon.log` already holds transcripts and timings. They are mixed with
  other lines and have no fixed format.
- `sunoto-daemon eval` reads `{description, kind, dictionary, snippets,
  cases: [{id, raw, expected, tags}]}` (`apps/daemon/src/eval.rs:24`).
- `docs/product-plan.md:60` and `:185` expect an optional history.
  `docs/product-usability-and-release-plan-2026-07-15.md` §4.4 asks for no
  dictated text in normal logs and one action that removes local history.
- Every field below is already in the event loop in
  `apps/daemon/src/daemon.rs` (§4.3).
- The shortcut's event tap already sees key presses
  (`crates/sunoto-macos/src/hotkey.rs:112`), so the undo signal is cheap.
- `uninstall --purge` already removes `~/Library/Application Support/sunoto`
  (`apps/daemon/src/setup.rs:803`).

## 3. Decisions for the owner

| # | Question | Recommendation | Why |
| --- | --- | --- | --- |
| D1 | Storage | **JSONL, one file per day** | Uses `serde_json`, which we already have. Appending only, so a crash costs at most one line. Old days expire by deleting files. Revisit SQLite (as `product-plan.md:185` assumed) when a history window needs search. |
| D2 | Default | **On** (owner decision, 2026-10-16; first recommended off). Setup prints that history is on, where it is kept, and how to turn it off. | Was: don't record without showing the user, and turn it on once there is a history window and a setup choice. |
| D3 | Audio | **Not in this version** (owner decision, 2026-10-16) | Text covers goals 1–4. Audio is in §11. |
| D4 | `log_transcripts` default | **Flip to `false`** when this ships | Keeps the user's words in one place, and closes usability plan §4.4. |
| D5 | Undo signal | **Yes. macOS only, 10 s after a paste, Cmd+Z only** | The tap already receives the key. Nothing else is read or stored. |
| D6 | Improve engine | **Our own small grammar-correction model** (about 60M parameters), loaded only while Improve is in use. Apple's model is optional. There is no rules layer. The exact model is picked by the test in §8 step 2. | Owner: our AI editor (Phi-4 mini, 3.8B) costs too much memory, and Apple's model is missing on many Macs. A small model behaves the same everywhere, and we can retrain it with Fix data. Every dictation already goes through the tidy-up rules, so a second rules layer would mostly repeat them. |
| D7 | How Improve replaces text | **Select the pasted text and paste over it. Fall back to the clipboard. Never ⌘Z.** | In some apps ⌘Z undoes more than the paste, or nothing at all (terminals). |

## 4. Design

### 4.1 Files

```
macOS: ~/Library/Application Support/sunoto/history/dictations/YYYY-MM-DD.jsonl
Linux: $XDG_DATA_HOME/sunoto/history/dictations/YYYY-MM-DD.jsonl
       (default ~/.local/share/...)
```

- The directory is `0700` and files are `0600`.
- `SUNOTO_HISTORY_DIR` overrides the location (for tests).
- Dates are UTC, like the log. There is no `chrono` dependency, so add a
  small days-to-date function with tests.

### 4.2 Record (schema v1)

There is one line per event. The main event is `dictation`. Later events
(§5) refer to it by `id`. Lines are only ever appended.

```json
{
  "v": 1, "type": "dictation", "id": "19a0f1c2b3d-42", "ts": 1760600000123,
  "app_version": "0.1.11", "platform": "macos", "status": "inserted",
  "target": { "id": "com.tinyspeck.slackmacgap", "name": "Slack" },
  "style": "prose",
  "asr": { "backend": "parakeet_mlx_offline", "model": "…", "profile_ms": 560 },
  "sound": { "ms": 5230, "rms": 812, "peak": 15020 },
  "text": {
    "raw": "Um, send the email to priya on Tuesday, actually Wednesday.",
    "polished": "Send the email to priya on Wednesday.",
    "llm": null,
    "inserted": "Send the email to priya on Wednesday."
  },
  "polish_trace": [ { "stage": "corrections", "before": "…", "after": "…" } ],
  "llm": { "status": "off" },
  "insertion": { "method": "pasted", "streamed": false },
  "timing_ms": { "recorded": 5230, "asr_turnaround": 240, "polish": 1,
                 "insert": 38, "release_to_insertion": 284 }
}
```

- `sound` holds statistics only. No samples are kept.
- `id` is `<unix ms hex>-<session_id>`, because `session_id` restarts at 1
  on every launch.
- `status` is one of `inserted`, `clipboard_only`, `secure_field`,
  `empty_transcript`, `empty_result`, `cancelled`, `asr_timeout`,
  `asr_error`, `insert_error`, `abandoned`.
- `timing_ms` has the full breakdown from the existing log line (also
  `release_to_finish_sent`, `release_to_final`, `dispatch`, `insert_wait`).
- Each record is about 1–3 KB, so 100 dictations a day is about 6 MB a month.
- Readers skip lines they can't parse and ignore unknown fields. A breaking
  change bumps `v`.

### 4.3 Where each field comes from (`daemon.rs`, at `dc6cf6d5`)

| When | Where | Fields |
| --- | --- | --- |
| Press starts a session | `:467`–`:510` | `id`, `ts`, `asr.*` |
| Focus at release | `FocusClass`, `:1436` | `target` |
| Final transcript | `:730`–`:760` | `text.raw`, `sound` (from `SessionAudioStats`), `empty_transcript` |
| Tidy-up rules | `:880`–`:912` | `style`, `polish_trace`, `text.polished` |
| LLM polish | `:913`–`:1020` | `llm.*`, `text.llm` |
| Insert dispatched | `:1021`–`:1040` | `text.inserted`, `empty_result` |
| Insertion report | `DaemonEvent::Ui`, `:1449` | `insertion`, `status`, `timing_ms`. **The record is written here.** |
| Esc | `:339` | `cancelled` |
| Sidecar error | `:1045` | `asr_error` |
| Watchdog | `:1795` | `asr_timeout` |

- **Carrying the record:** add `history: Option<HistoryDraft>` next to
  `timing` / `audio_stats`. Each capture point is a one-line call. Each
  place that clears `timing` on failure also finishes the draft.
- **Safety net:** a new press with a draft still open writes the old one as
  `abandoned`.
- **Shared timing math:** move the breakdown math at `:1452`–`:1494` into
  `SessionTiming::breakdown()` in `session.rs`, used by the log line and the
  record.
- **To check:** whether streamed LLM insertion (`InsertStreamEnd`) still
  produces a `UiReport` for the session. If not, finish the record when it
  is dispatched.

### 4.4 Writer thread

`apps/daemon/src/history.rs` follows the overlay pattern (`overlay.rs:22`).

- A bounded channel of 16. The loop calls `try_send`; if the channel is
  full, the record is dropped and counted. The loop never waits on disk.
- The thread opens the day file in append mode, does one `write_all` per
  line, and closes it. This keeps CLI deletes safe while the daemon runs.
- I/O errors are logged and otherwise ignored. History never takes the
  daemon down.

### 4.5 Retention

At startup and then once a day, delete day files older than
`history_retention_days`. `0` keeps them forever.

### 4.6 Privacy rules (each one tested)

- `SecureField`: write only `id`, `ts`, `status`, `target` and timings. No
  text.
- `target.id` in `history_exclude_apps`: same as above. The default list is
  the password managers: `com.1password.1password`,
  `com.agilebits.onepassword7`, `com.bitwarden.desktop`,
  `com.apple.Passwords`, `com.apple.keychainaccess`.
- System mode is never recorded. The history code makes no network calls.
- `sunoto-daemon status` shows `history: on (312 dictations)` or
  `history: off`.

### 4.7 Settings

| Key | Default |
| --- | --- |
| `history_enabled` | `true` (D2) |
| `history_retention_days` | `90` |
| `history_exclude_apps` | password managers (above) |

### 4.8 Command line

The CLI works on the files directly, with no control-socket changes.

```
sunoto-daemon history status | list [-n 20] | show <id|last>
sunoto-daemon history fix <id|last> "…"     record what it should have said
sunoto-daemon history forget <id|last>      rewrite one day file via temp + rename
sunoto-daemon history clear
sunoto-daemon history stats [--since 30d]
sunoto-daemon history suggest [--accept 1,2]  word-list suggestions (§7.3)
sunoto-daemon history report [--since 30d]    writes and opens history/report.html (§7.4)
sunoto-daemon history export --format eval|jsonl [--since 30d] [--corrected-only] -o FILE
```

## 5. Knowing what the user meant

Each of these appends an event line that points at a dictation `id`.

| Clue | Event | How |
| --- | --- | --- |
| Fix button (§6.1) | `{"type":"correction","id":…,"text":…,"source":"fix_button"}` | The user edits the pasted text in a small panel |
| Manual fix | `{"type":"correction","id":…,"text":…,"source":"cli"}` | `history fix last "…"` |
| Improve accepted or cancelled (§6.2) | `{"type":"improve","id":…,"text":…,"accepted":true}` | Recorded when the user presses Replace or Cancel |
| Undo after paste (macOS) | `{"type":"signal","kind":"undo",…}` | For 10 s after a paste, the tap callback watches only for keycode `0x06` with Cmd and stores the time. |
| Said it again | `{"type":"signal","kind":"redictate","similarity":…}` | Next dictation within 20 s in the same app, with raw text similarity above 0.6 |

Dictations with no clue are exported as `assumed`, never as `corrected`.

## 6. The pill's two buttons: Fix and Improve

After a paste, the pill shows the check mark and two buttons for 6 s. They
stay while the pointer is over the pill.

```
 ┌───────────────────────────────────────┐
 │  ✓ Pasted      [ Improve ]  [ Fix ]   │
 └───────────────────────────────────────┘
```

Both act on the last dictation only. The pill is a non-activating `NSPanel`
(`makePillPanel`, `services/macos/sunoto-overlay.swift:1679`) that ignores
the mouse (`:1691`). It accepts clicks only while the buttons are showing,
and a click doesn't take focus from the app the text went into. The
permission panel (`:1375`) already uses buttons on a non-activating panel.

### 6.1 Fix: teach Sunoto what you said

1. Click **Fix**. A small panel opens with the pasted text, ready to edit.
   This panel takes keyboard focus.
2. Edit it and press Enter. The daemon appends a `correction` event (§5),
   and focus returns to the previous app. **The app's text is not changed.**
3. When the same correction keeps coming back, `history suggest` (§7.3)
   offers a word-list entry. Later, the pill can ask directly: "Always
   write Priya?"

Fix needs history to be on. When history is off, the button is hidden.

### 6.2 Improve: fix spelling and grammar in place

1. Click **Improve**. The pill shows "Improving…".
2. The daemon sends the pasted text to the grammar helper (§6.3) and gets a
   corrected version back.
3. The daemon checks the result and protects the word list (§6.4). The pill shows the new text with
   the changed words highlighted, plus **[Replace] [Cancel]**. If nothing
   changed, it says "Looks right already".
4. **Replace** puts the new text in place of the pasted text (§6.5).
5. With history on, the choice is recorded as an `improve` event (§5). That
   also measures how useful Improve is.

Improve works with history off.

### 6.3 Correction engine: our own small grammar model

**Decision (D6):** Improve uses a small model trained only to correct
grammar, shipped with Sunoto.
- It is **not** the AI editor's Phi-4 mini, which has 3.8B parameters and
  is too heavy.
- There is **no rules layer** in front of it. Every dictation already goes
  through the tidy-up rules before it is pasted.

**Why:**
- **It works everywhere Sunoto runs.** That means every Apple Silicon Mac,
  with or without Apple Intelligence, and later Linux. It behaves the same
  on all of them.
- **It is small.** About 60× fewer parameters than Phi-4 mini, and it is
  in memory only while Improve is being used.
- **We can retrain it.** Fix-button corrections are exactly the
  "wrong → right" pairs these models learn from (§11).
- **Others do the same.**
  - Google's Pixel keyboard grammar model is 20 MB and runs in about 22 ms
    ([Google Research](https://research.google/blog/grammar-correction-as-you-type-on-pixel-6/)).
  - Grammarly ships an on-device model for offline correction
    ([Grammarly](https://www.grammarly.com/blog/engineering/on-device-models-scale/)).

**Candidates.** The test in §8 step 2 picks one. Sizes and scores are
published figures, not yet measured by us.

| Candidate | Size | Notes |
| --- | --- | --- |
| `gec-t5_small` (T5-small trained on cLang-8, Apache 2.0) | ~60M parameters, ~120 MB at fp16 | Reported F0.5 of 60.7 on CoNLL-14. First choice if it holds up on dictation text. |
| A T5-base grammar model | ~220M parameters | Better quality, about 4× the memory |
| A ~0.5B instruction model (e.g. Qwen2.5-0.5B-Instruct), quantized | ~300–400 MB | More flexible, but more likely to rewrite. The guard rails matter more. |
| Apple's on-device model (optional) | managed by macOS | Only on Macs with Apple Intelligence on. Used only if it is clearly better there. |

**Ruled out:**
- **The macOS spell checker.** Tested 2026-10-16 with
  `tools/improve/improve_probe.swift`. It fixes names like "john" → "John"
  but only flags grammar.
- **A rules layer such as Harper.** It would repeat the tidy-up rules.
- **Cloud models.** The text would leave the Mac.
- **The AI editor's model.** Too much memory.

**Test results, 2026-10-16** (`gec-t5_small`, M1 Pro CPU,
`tools/improve/grammar_probe.py` on the 91 cases in `grammar_cases.jsonl`):

| | Beam 5 | Greedy |
| --- | --- | --- |
| Mistakes fixed (30) | 24 | 24 |
| Clean sentences left alone (50) | 44 | 44 |
| Technical sentences left alone or blocked (11) | 11 | 11 |
| Warm call p50 / p95 | 183 / 412 ms | 94 / 195 ms |

- **Use greedy decoding.** The quality is the same at half the time.
- **Missed mistakes.** "Who's" → "Whose", "Then you" → "Thank you" (a
  mishearing, not grammar), and "Me and him".
- **Wrong changes to clean text.** 3 of the 50 clean sentences had their
  meaning changed, e.g. "The build is green" → "The building is green" and
  "It is" → "It was". This is why Improve shows a preview with Cancel.
- **Code gets broken, and the guard rails catch it.** The tokenizer breaks
  code and paths ("agents.md" → "agents. md", "--workspace" →
  "-- workspace"). The stricter rule in §6.4 blocks every such case.
- **Memory.** Loading the obvious way (random-init model, then
  `load_state_dict`) holds two copies of the weights: about 975 MB.
  Initializing on the `meta` device and assigning bf16 safetensors loads in
  0.16 s and uses about 380 MB. Of that, about 350 MB is PyTorch plus
  Transformers. MLX by itself adds about 7 MB to a Python process.
- **Runtime.** Ship it on MLX, which Sunoto already bundles, to stay under
  300 MB. That target is an estimate until the T5 port to MLX is measured.

**How it runs: a grammar helper**

- **The sidecar.** A new Python sidecar,
  `services/improve/grammar_sidecar.py`, uses typed NDJSON like the others.
  - Request: `{"type":"improve","session_id":…,"text":…}`.
  - Reply: `{"type":"improved","session_id":…,"text":…,"ms":…}`.
- **Runtime.** On macOS it uses MLX, which Sunoto already ships for the
  speech model; `mlx-examples` has a reference T5. On Linux it runs the same
  model on the CPU. The test confirms whether MLX or ONNX Runtime is the
  better fit.
- **Loaded only when needed.**
  - The daemon starts the helper when the pointer moves onto the pill's
    buttons, so loading overlaps the click.
  - The model is unloaded after 60 s without use.
  - The process exits after 5 minutes idle.
  - Nothing runs while you are only dictating.
- **Sentence by sentence.** Long dictations are corrected one sentence at a
  time, because these models are trained on sentences.
- **Download.** The model is downloaded by `setup` and verified by size and
  SHA-256, like the Parakeet bf16 model (`model_dir_complete` in
  `settings.rs`), or on first use (§12).
- **Settings:**
  - `improve_enabled`: default `true` once it ships.
  - `improve_engine`: `"grammar_model"` (default) or `"apple"`.
  - `improve_model`: a path override.

**Apple's model, optional.** If the test shows it is clearly better,
`improve_engine: "apple"` uses it on Macs where
`SystemLanguageModel.default.availability` is `.available`.
- The Swift overlay helper calls it, wrapped in
  `#if canImport(FoundationModels)` and `if #available(macOS 26, *)`.
- Every other Mac falls back to the grammar model.
- Its context is 4K tokens.

### 6.4 Guard rails and the word list

The daemon applies these to every result, whichever engine produced it,
before the preview is shown:

- **Numbers.** The digits must be identical, using the same rule as `eval`.
- **Code, names and links.** Some words must come back unchanged and in
  the same order. That covers any word with a digit, a symbol
  (`. / @ : _ # $ % & * = + < > [ ] { } | ~`), a leading dash, or a capital
  after its first letter. Examples: file names, URLs, emails, command flags,
  versions, `macOS`, `PyTorch`. Tested: this blocked all 5 technical
  sentences the model broke.
- **Size of the change.** No more than 40% of the words may change. Above
  that, the pill says "Couldn't improve this safely" and shows no Replace
  button.
- **The word list is protected.** Every word-list term (the `written` side
  of `polish.dictionary`) that appears in the pasted text must appear
  unchanged in the result. If the model changed its spelling or
  capitalization, for example "PyTorch" → "Pytorch", it is put back.

### 6.5 Replacing the pasted text

The daemon already knows the last insertion: the text, the target app and
window, and the time.

1. **Safety first.** The same checks as a normal paste apply: focus is
   revalidated, and password fields are never touched.
2. **Preferred: Accessibility.** If the text just before the caret in the
   focused field equals what Sunoto pasted, set `AXSelectedTextRange` to
   that span and paste the new text over it, using the existing pbcopy +
   ⌘V path.
3. **Otherwise, select with the keyboard.** This applies only if no key was
   pressed since the paste and focus hasn't changed. The tap keeps just a
   "typed since paste" flag. Select with Shift+← for the pasted length, then
   paste.
4. **Otherwise, use the clipboard.** Put the new text on the clipboard; the
   pill says "Improved text copied: press ⌘V".

### 6.6 Where the code goes

- **`services/macos/sunoto-overlay.swift`:** the buttons, the Fix panel
  and the preview. It also calls Apple's model, but only when
  `improve_engine` is `"apple"`. Coordinate with the uncommitted changes in
  this file.
- **`services/improve/grammar_sidecar.py` (new):** loads and unloads the
  grammar model and corrects text sentence by sentence.
- **`crates/sunoto-ipc`:** two sets of new messages.
  - Overlay messages, daemon to overlay: `ShowActions {session_id,
    fix_available}`, `ImprovePreview {session_id, text}`,
    `ImproveFailed {session_id, reason}`.
  - Overlay messages, overlay to daemon: `PillHovered`,
    `ImproveClicked {session_id}`, `ImproveAccepted {session_id}`,
    `ImproveCancelled {session_id}`, `FixSaved {session_id, text}`.
  - The grammar sidecar protocol.
- **`apps/daemon`:**
  - keeps the last insertion;
  - starts and stops the grammar helper;
  - runs the guard rails and the word-list protection (§6.4);
  - performs the replacement (§6.5) in the insertion module;
  - writes the history events.
- **Unchanged:** the AI editor (LLM polish) sidecar. Improve never loads it.

## 7. How improving works

All analysis runs on the Mac, inside the `sunoto-daemon` binary (Rust, no
new dependency), so users get the user-facing part too. Python is only for
team experiments.

### 7.1 The loop

Each change follows the same path: the diary is analyzed, and the analysis
produces one of three outputs. Someone acts on it, the change is re-tested
on the diary, and it ships only if it is better.

| Output | Who acts | What changes | How it is re-tested |
| --- | --- | --- | --- |
| Word-list suggestion (`history suggest`) | the user approves | `polish.dictionary` in their config | Preview: how many past dictations it would change, and how many of those match the user's fixes |
| Improvement report (`history report`) | the team, dogfooding | tidy-up rules, AI editor prompt/mode, defaults, bugs | `history export --format eval`, then `sunoto-daemon eval` before and after |
| Test set (`history export`) | the team | `tests/corpus/`, from the team's own dictations only | from then on, `eval` runs in `make test` |

### 7.2 Metrics

- **Fix rate:** inserted dictations that later got a correction or an undo,
  divided by all inserted dictations. It is the main accuracy measure
  without audio. Lower is better.
- **Zero-edit rate:** the existing `eval` metric, the share of test cases
  where the polished text equals the expected text.
- **Paste time:** `release_to_insertion`, p50 and p95.
- **Failure rate** per `status`.
- **Stage harm rate:** among dictations where a tidy-up stage changed the
  text, the share the user then fixed or undid. It is compared against the
  overall fix rate. A stage well above it is suspect.

### 7.3 Word-list suggestions (`history suggest`)

1. For each correction, align `text.inserted` with the corrected text word
   by word, using a token-level edit-distance alignment.
2. Keep substitutions of 1–3 words. Skip changes that touch digits (the
   same rule `eval` uses) and changes that are only punctuation. Longer
   rewrites go to the report instead.
3. Count each (spoken, written) pair. Suggest it when it was fixed at least
   3 times and in at least half the dictations containing the spoken form.
4. Preview: run every past `text.raw` through the polish pipeline with the
   user's config plus the candidate entry. Count the dictations it would
   change, how many of those match a fix, and how many conflict.
5. Nothing is applied automatically. `--accept` adds entries through
   `Settings::save_changes`.

```
$ sunoto-daemon history suggest
1. priya   → Priya    fixed 6× · would change 15 past dictations: 6 match your fixes, 0 conflict
2. get hub → GitHub   fixed 3× · would change 4 past dictations: 3 match your fixes, 0 conflict
Add with: sunoto-daemon history suggest --accept 1,2
```

### 7.4 Improvement report (`history report`)

This writes `history/report.html`, a static local page that opens in the
browser. It covers a period (`--since 30d`) and puts each version found in
the diary side by side:

- **Speed:** paste time p50/p95 per version and backend. Flagged when p95
  is more than 20% worse than the previous version.
- **Failures:** count per `status`, and the apps with the most
  `clipboard_only` results (paste failed).
- **Accuracy:** fix rate per version, and the 10 most-fixed phrases.
- **Tidy-up rules:** for each stage, how often it changed text and its harm
  rate. Suspect cases are listed with before, after and the fix.
- **AI editor** (when on): accepted or skipped, latency, and the fix rate
  when it changed the text versus when it left it alone.

Before observability exists, this is the team's dashboard, built from our
own Macs.

### 7.5 Changing a rule safely (team)

1. Pick cases from the report.
2. Run `history export --format eval --corrected-only --since 30d -o real.json`
   and check each expected text by hand.
3. Change the rule. Run `sunoto-daemon eval --corpus real.json` before and
   after. It already fails if a change breaks text that was correct or
   alters digits.
4. If it's better, add the approved cases to `tests/corpus/`, so
   `make test` guards them from then on. Only the team's own dictations go
   into the repo.

The AI editor follows the same path: `export --format jsonl --corrected-only`
gives (llm input, correction) pairs for the existing llm-polish eval loops
(`docs/llm-polish-*-eval-loop*.md`).

Comparing speech models needs audio, so it is in §11.

## 8. Phases

All phases must pass `cargo test --workspace --offline`,
`cargo clippy --workspace --offline --all-targets -- -D warnings`, and
`make test`.

1. **Text history (about 1–2 days).** `history.rs` (record, draft, writer,
   day files, UTC date, retention); settings; `status` line;
   `list|show|forget|clear`; the `breakdown()` refactor; hooks at every row
   of §4.3; docs (`desktop-configuration.md`, README privacy note); D4.
   *Exit:* 20 real dictations on macOS give 20 records with correct statuses,
   including one Esc and one password field. `release_to_insertion` is
   unchanged against 10 dictations before the change.
2. **Improve experiment (about 1 day; can run first, needs nothing
   else).**
   1. Collect about 50 real dictations from the team's own
      `~/Library/Logs/sunoto/daemon.log`. Its `final transcript:` lines are
      there because `log_transcripts` is on. Add the error sentences from
      `tools/improve/improve_probe.swift`.
   2. Write `tools/improve/grammar_probe.py`. It runs one candidate model on
      a file of sentences and prints each output, the time, and the peak
      memory.
   3. Download `gec-t5_small` (a few hundred MB including tokenizer files)
      and run it. Try the larger candidates from §6.3 only if it falls
      short.
   4. For comparison, run `improve_probe.swift` on a Mac with Apple
      Intelligence on.
   5. Mark each result better, same or worse. Count changed numbers, names
      and file names. Measure the first click (including the model load),
      warm calls, and memory while loaded, using `footprint`, not `ps`.

   *Go if:* at least 80% of results are better or the same, none change the
   meaning, a warm call takes under 500 ms, the first click takes under
   2 s, and memory while loaded stays under 300 MB. Otherwise, try the next
   candidate.
3. **Pill buttons (about 3–5 days).** Improve (§6.2–§6.6) works without
   history. Fix (§6.1) needs step 1.
   *Exit:* on TextEdit, Notes, Slack, Chrome and Terminal, Replace swaps
   exactly the pasted text. Where it can't, it falls back to the clipboard.
   It never touches other text.
4. **Fixes, clues and export (about 2–3 days).** `fix`, the undo and
   redictate signals, `export --format eval|jsonl`.
   *Exit:* an exported corpus runs through `sunoto-daemon eval`.
5. **Improve tools (about 2–3 days).** `history suggest` with preview and
   `--accept` (§7.3); `history stats` and `history report` (§7.4).
   *Exit:* one real change made from the data, such as a word-list entry or
   a rule fix, re-tested as in §7.5.

## 9. Testing

Unit tests in `history.rs`, using a temp `SUNOTO_HISTORY_DIR`:

- the record round-trips, unknown fields are ignored, and a truncated last
  line is skipped;
- the date function at month ends, leap days and the year boundary;
- retention deletes only files older than the limit;
- `SecureField` and excluded apps produce records with no text;
- each `status` produces exactly one record, and an open draft becomes
  `abandoned`;
- a full channel doesn't block;
- `forget` removes exactly one line;
- `export --format eval` parses with `eval.rs`;
- Improve guard rails: changed digits, changed file names or URLs, and
  over 40% of words changed are each rejected. A word-list term the model
  changed ("Pytorch") is restored to its written form ("PyTorch");
- the grammar sidecar protocol, run with a stub model so no download is
  needed (like the overlay protocol tests that run without GTK). It covers
  load on the first request, unload after idle, and sentence splitting;
- the replace decision: AX span matches → select and paste; typed since
  paste → clipboard; focus changed → clipboard; password field → nothing.

Manual check on macOS:

1. Set `history_enabled: true`.
2. `pkill -f "Sunoto.app/Contents/MacOS/sunoto-daemon"; open ~/Applications/Sunoto.app`,
   then wait for `ASR sidecar ready`.
3. Dictate into a throwaway TextEdit document.
4. Run `history list`.
5. Check that file modes are `0600`.
6. Compare the timing lines before and after.

## 10. Risks

| Risk | Mitigation |
| --- | --- |
| Someone is recorded without realizing it | Setup says history is on, where it is kept and how to turn it off; shown in `status`; README privacy note |
| Saving slows down pasting | Writer thread with `try_send`, verified with the timing breakdown |
| A failure path is never recorded | `abandoned` safety net, a test per status |
| Format changes break old data | `v` field, unknown fields ignored, files only appended |
| Fixes are biased toward obvious errors | `corrected` vs `assumed` tags |
| Improve changes the meaning | Guard rails (§6.4), a preview before anything changes, and Cancel |
| Replace overwrites the wrong text | AX span check, the "typed since paste" flag, the clipboard fallback, and no ⌘Z |
| The small model isn't good enough on dictation text | Test before building (§8 step 2), with a ladder of candidates (§6.3). Retrain on Fix data later (§11). |
| Memory while Improve is in use | The model is loaded only when the pointer is on the pill and unloaded after 60 s; measured in the test |
| The model isn't downloaded yet, or the user is offline on first use | The button shows "Improve isn't ready" and `status` says why |

## 11. Later versions

- **Audio (opt-in).** WAV at 16 kHz mono 16-bit, the same samples the
  sidecar gets. The daemon already sees every sample (pre-roll `:494`,
  frames `:610`), so it can buffer them while recording (32 KB/s, capped
  at 10 min) and write them on the writer thread. Keep 14 days, 2 GB at
  most. A `keep` command exempts clips from deletion. Exclude from Time
  Machine. It enables:
  - replaying audio through other speech models and measuring the error
    rate;
  - a NeMo manifest export;
  - testing a new model on the user's own clips before switching to it;
  - a play button in the history window.
- **More clues.** An opt-in experiment that re-reads the text field a
  minute after the paste.
- **Teach the grammar model.** Fine-tune it on Fix-button pairs
  ("pasted" → "what you meant"), starting with the team's own data. Users'
  data is used only if they send it. Ship the result as a model update,
  re-tested on the test set first.
- **Improve on Linux.** The same grammar helper running on the CPU.
- **History window for users.** Search, copy, paste again, delete. A choice
  at setup, then on by default. Decide on SQLite here (D1).
- **Observability for the team (next feature, own plan).**
  - A daily, numbers-only summary built from these records: counts, paste
    times, failures by status, fix/undo rates, version, model, OS and chip,
    health events.
  - Saved locally first, sent only if the user opted in, to a small endpoint
    we run.
  - Never sends text or app ids. `telemetry preview` shows exactly what
    would be sent.

## 12. Open questions

1. Is a 90-day retention right, or should the developer machine keep
   history forever?
2. Should release installs offer history at setup now (off pre-selected),
   or wait for the history window?
3. When should the grammar model (about 120 MB) be downloaded?
   - **At setup:** it is always ready, but costs disk for people who never
     use Improve.
   - **On the first Improve click:** no cost for people who never use it,
     but that first click needs internet.

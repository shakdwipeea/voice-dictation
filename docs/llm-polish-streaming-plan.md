# LLM Polish: Streaming Decode + Streaming Insertion + TTFT

**Status:** proposed plan, 2026-06-30
**Companion doc:** [`docs/llm-polish-implementation-and-latency.md`](llm-polish-implementation-and-latency.md)

## 0. Goal

Today the LLM polish path is **batch**: the sidecar runs one
`create_chat_completion` call, returns the whole polished text in one
`{"type":"polished",...}` event, and only then does the daemon dispatch a
single atomic `UiCommand::Insert { text }` to the UI thread (which pastes or
types the whole string at once). The full polish latency (prefill + decode)
elapses before a single character reaches the focused app.

We want:

1. **Stream the LLM decode** — the sidecar emits `{"type":"polish_chunk",...}`
   deltas as tokens are generated, instead of holding the whole output.
2. **Paste progressively** — the daemon forwards the deltas to the UI thread,
   which types/appends them into the focused window as they arrive, so the
   user sees the polished text appear token-by-token.
3. **Log the LLM TTFT** (time-to-first-token) — preflick of decode visible to
   the user end-to-end, captured per call and surfaced in the daemon timing
   breakdown.

Everything is gated behind a new setting `llm_polish_stream_insert`
(default **off**) so the validated batch-paste path stays canonical until a
live dictation gate passes (mirrors how every prior LLM-polish change shipped).

---

## 1. Why streaming is worth it here

§4.7 of the latency doc showed generation is **already fast** (~26 ms/tok,
8–18 tokens/utterance), so per-utterance decode is ~0.2–0.5 s. Streaming does
not reduce total completion time, but it **overlaps decode with insertion**:

- Batch: `TTFT + N*ms_per_tok + paste_latency` is incurred **before** the user
  sees anything (release→first-char ≈ polish gap + paste).
- Stream: the user sees the first decoded character at
  `TTFT + ms_per_tok` ≈ a couple hundred ms, and the rest streams while the
  app renders. The tail (last token → paste commit) collapses.

On macOS the dominant insertion is a **clipboard paste** (atomic, one shot),
so streaming *insertion* must use the **typing** path (`insert_direct`,
CGEvent keystrokes). That is a deliberate trade-off (typing is the existing
fallback; some apps ignore CGEvent typing), accepted for the streaming path
only and reverted-on-failure to the reliable clipboard commit.

The prefill cold-ramp problem (§4) is **already solved by keepalive**; streaming
does not change it. Streaming is orthogonal and additive.

---

## 2. Protocol changes (sidecar → daemon, NDJSON)

### 2.1 New event: `polish_chunk`

Emitted zero or more times **between** the `polish` request and the terminal
`polished` event, only in streaming mode.

```jsonc
{
  "type": "polish_chunk",
  "session_id": 12,
  "sequence": 1,            // monotonic per session, 1-based
  "delta": "Please send",   // CLEANED text fragment (post-prefix-strip)
  "ttft_ms": 187            // present only on sequence==1
}
```

- `delta` is **already cleaned**: the `EDIT: ` (or `EDIT:\n`) prefix is
  consumed by the sidecar and not forwarded; what the daemon receives is the
  final text fragment that will end up in the polished `text` field (modulo the
  content-loss guard edge case — see §5).
- `sequence` lets the daemon detect gaps / reorder robustly (present protocol
  is in-order over a pipe, but cheap insurance).
- `ttft_ms` (only on the first chunk) = wall time from
  `create_chat_completion(stream=True)` call start to the first non-empty
  content delta. Mirrored onto the terminal `polished` event's diagnostics.

### 2.2 Extended `polished` event

Adds (all `#[serde(default)]`):

```jsonc
{
  "type": "polished",
  "session_id": 12,
  "text": "...",
  "ttft_ms": 187,           // NEW (None when streaming disabled / no tokens)
  "streamed": true,         // NEW (whether chunks were emitted)
  "stream_chunks": 4,      // NEW (count, for log parity)
  ...existing fields...
}
```

### 2.3 Decision semantics during streaming

The constrained mode grammar already restricts the model to `OK` or
`EDIT: <text>`. The streaming parser is a tiny state machine on the raw delta
stream:

1. Accumulate raw deltas until the first newline or the `OK`/`EDIT:` prefix is
   disambiguated.
   - If the model emits `OK` (clean) → emit **zero** `polish_chunk` events;
     the `polished` event's `text` is the raw transcript (deterministic output)
     and `streamed` is `false`. The daemon takes the **fast path**: it pastes
     the deterministic-polished text exactly as today (one atomic insert).
   - Else (`EDIT:` prefix seen) → strip the prefix, then forward every
     subsequent raw delta as a cleaned `polish_chunk`.

This means streaming insertion only ever runs when the LLM actually edits.
The common "clean, no edit" case costs exactly one decoded token and inserts
the way it does today.

---

## 3. Sidecar implementation (`services/polish/llm_polish_sidecar.py`)

Add `constrained_payload_streaming(llm, transcript, label, session_id)`:

```python
def constrained_payload_streaming(llm, transcript, label, session_id):
    started = time.time()
    max_tokens = constrained_max_tokens(transcript)
    begin_cache_request(llm)
    reset_llama_timings(llm)
    ttft_ms = None
    raw_acc = []          # full raw output for diagnostics
    cleaned_acc = []      # cleaned deltas forwarded to the daemon
    sequence = 0
    prefix_state = _PrefixState()   # strips "EDIT: " / detects "OK"

    response = run_chat_completion_stream(
        llm, constrained_messages_for(transcript), max_tokens,
        grammar=constrained_output_grammar(),
    )
    for chunk in response:
        delta = (chunk.get("choices", [{}])[0]
                    .get("delta", {}).get("content") or "")
        if not delta:
            continue
        if ttft_ms is None:
            ttft_ms = round((time.time() - started) * 1000)
        raw_acc.append(delta)
        cleaned, decision, done = prefix_state.feed(delta)
        # OK fast path: decision decided -> STOP streaming, no chunks emitted.
        if decision == "OK":
            break
        if cleaned:
            sequence += 1
            emit({
                "type": "polish_chunk", "session_id": session_id,
                "sequence": sequence, "delta": cleaned,
                **({"ttft_ms": ttft_ms} if sequence == 1 else {}),
            })
            cleaned_acc.append(cleaned)
        if done:
            break

    raw = "".join(raw_acc)
    # Reuse the exact same cleaner so streaming/byte-stream yields what batch does.
    cleaned_full, decision_label, malformed = clean_constrained_output(raw, transcript)
    # Apply the same content-loss guard as the batch path (see §5).
    text, guard_reverted = _apply_guard(transcript, cleaned_full, label)
    streamed = decision_label == "EDIT" and not malformed and not guard_reverted
    stream_chunks = sequence
    perf = llama_timings(llm)
    emit(polished_terminal_payload(...))   # carries ttft_ms, streamed, stream_chunks
```

`_PrefixState` is a small class:

```python
class _PrefixState:
    """Strips the 'EDIT: ' / 'EDIT:\\n' prefix and detects the 'OK' fast path."""
    def __init__(self):
        self.buf = ""
        self.prefix_done = False
        self.decision = None  # None | "OK" | "EDIT"
    def feed(self, delta):
        # Returns (cleaned_delta_or_None, decision_or_None, done_bool)
        ...
```

- `run_chat_completion_stream` is `run_chat_completion` with `stream=True`; the
  returned object is a generator of chunk dicts.
- Keepalive **stays exactly as-is**: the background thread's `run_chat_completion`
  keeps using the non-streaming call (its output is discarded anyway).
- The keepalive `threading.Lock` still serializes the (streaming) polish call vs.
  keepalive pings — `with _llm_lock:` wraps the whole streaming loop. **llama.cpp
  streaming is not separately thread-safe**; the existing lock already covers it.

Gate: sidecar streams iff `SUNOTO_LLM_POLISH_STREAM` env is `"1"`/`"true"`
(push the setting). When off, `polish()` uses the existing
`constrained_payload` (unchanged). The `one_pass_minimal` / `two_step` modes
remain non-streaming for now (only `constrained_one_call` streams).

---

## 4. Rust client (`apps/daemon/src/llm_polish.rs`)

### 4.1 Protocol

Add a `PolishChunk` variant to `LlmPolishEvent`:

```rust
PolishChunk {
    session_id: u64,
    sequence: u64,
    delta: String,
    #[serde(default)]
    ttft_ms: Option<u64>,
},
```

Add to `LlmPolishDiagnostics` (and the `Polished` event fields):
`ttft_ms: Option<u64>`, `streamed: Option<bool>`, `stream_chunks: Option<u64>`.

### 4.2 Streaming callback API

Replace the blocking `polish(session_id, input, timeout_ms)` with a variant
that accepts an on-chunk closure:

```rust
pub fn polish<F>(
    &mut self,
    session_id: u64,
    input: &str,
    timeout_ms: u64,
    mut on_chunk: F,
) -> Result<LlmPolishOutcome, String>
where
    F: FnMut(&str),
```

The existing call sites pass `|_| {}` (no-op) → behavior identical to today.
The recv loop, on a `PolishChunk { session_id, delta, .. }` matching the
current session, calls `on_chunk(&delta)`. Stale chunks (other session) are
ignored. The terminal `Polished` event carries `ttft_ms` etc. as before.

**No daemon-loop restructuring is needed** — the reader thread already funnels
all events through one mpsc channel; `polish()` consumes chunks inline and
hands them to the caller's closure synchronously, then returns on `polished`.

### 4.3 Tests

- `parses_polish_chunk_event`: round-trip a `polish_chunk` JSON with
  `ttft_ms` on sequence 1, omitted on sequence 2.
- `polished_event_carries_ttft`: a `polished` event with `ttft_ms`, `streamed`,
  `stream_chunks` parses into the diagnostics struct.
- (existing) batch-path tests unchanged.

---

## 5. Content-loss guard interaction (important)

The batch path applies `drops_content_unsafely` **after** the model finishes
and may revert to the raw transcript. With streaming insertion, the LLM's
output has already been typed into the focused app by the time the guard runs
— we cannot un-type it.

Resolution:

- The sidecar runs the guard on the **full accumulated raw** before emitting
  the terminal `polished`. If the guard fires, it sets `streamed=false` and
  `text=<raw transcript>` on the terminal event AND emits a sentinel
  `polish_chunk { "delta": "", "guard_revert": true }` so the daemon can
  react. In practice the daemon's reaction is best-effort: it cannot un-type,
  but it logs `llm polish content-loss guard fired after streaming; typed
  text already on-screen`. The guard is conservative (≥3 uncounterparted
  content words) and rarely fires; the typing-paste trade-off is documented as
  the accepted edge.
- Mitigation / future: when `llm_polish_stream_insert` is on, the sidecar
  could *buffer* the first decoded chunk and emit it only once the guard won't
  trivially fire (e.g. once the model has emitted ≥ a few tokens of content).
  Out of scope for v1; tracked in §8.

---

## 6. Daemon wiring (`apps/daemon/src/daemon.rs`)

### 6.1 New `UiCommand`s

```rust
pub enum UiCommand {
    CaptureFocus,
    ShowBubble(BubbleKind, String),
    HideBubble,
    Insert { session_id: u64, text: String },
    // NEW: progressive typing stream.
    InsertStreamChunk { session_id: u64, first: bool, delta: String },
    InsertStreamEnd { session_id: u64, final_text: String, streamed_ok: bool },
    Shutdown,
}
```

### 6.2 UI thread streaming state

The UI thread keeps a small per-session streaming context:

```rust
struct StreamCtx {
    session_id: u64,
    focus_token: Option<String>,   // copied from focus_at_release on first chunk
    focus_ok: bool,               // focus still on the dictation target
    typed: String,                // for clipboard-fallback accumulation
    mode: StreamMode,             // Typing | ClipboardFallback
}
```

Flow:

- `InsertStreamChunk { first: true, delta, .. }`:
  - On `first`, run the same `focus_at_release` comparison as `insert_macos`/
    `insert_x11`. If the focus moved → `mode = ClipboardFallback`
    (accumulate `delta` into `typed`, do **not** type). If focus intact →
    `mode = Typing` and type the first delta.
  - On subsequent chunks: `Typing` mode types the delta; `ClipboardFallback`
    accumulates and does nothing visible (committed at `End`).
- `InsertStreamEnd { final_text, streamed_ok, .. }`:
  - `Typing` mode + `streamed_ok`: nothing more to do — text already on screen.
    (Optional: nothing; we trust the typed text.)
  - `ClipboardFallback` or `!streamed_ok`: paste `final_text` once via the
    reliable clipboard path (the standard `Insert` logic), replacing any
    partial typed chars is not possible, so this is the documented fallback.
- Robustness: if an `InsertStreamEnd` never arrives (sidecar died), the UI
  thread has a `session_id`-keyed `Option<StreamCtx>`; stale contexts are
  cleared on the next session's start.

### 6.3 Daemon hot path

In the post-ASR block (around `apps/daemon/src/daemon.rs:996`):

```rust
if settings.llm_polish_enabled && !raw_text.trim().is_empty()
    && let Some(client) = llm_polish.as_mut()
{
    ui.show(BubbleKind::Transcribing, "polishing...");
    let llm_input = output.clone();
    let stream_insert = settings.llm_polish_stream_insert;

    match client.polish(session_id, &llm_input, settings.llm_polish_timeout_ms,
        |delta: &str| {
            if !stream_insert { return; }
            let is_first = !streaming_started.replace(true);
            let _ = ui_tx.send(UiCommand::InsertStreamChunk {
                session_id, first: is_first, delta: delta.to_string(),
            });
        })
    {
        Ok(outcome) => {
            // ...existing logging + add ttft/streamed...
            if stream_insert && outcome.diagnostics.streamed == Some(true)
                && outcome.diagnostics.stream_chunks.unwrap_or(0) > 0
            {
                // Streaming typed the text; finalize without a full Insert.
                let _ = ui_tx.send(UiCommand::InsertStreamEnd {
                    session_id,
                    final_text: outcome.text.clone(),
                    streamed_ok: true,
                });
                timing.polish_done_at = Some(Instant::now());
                timing.insert_dispatched_at = Some(Instant::now());
                continue_to_timing_report = true; // skip the standard Insert
            } else {
                output = outcome.text;
                // ...fall through to existing sanitize + Insert path (incl. OK fast path)
            }
        }
        Err(error) => { ...unchanged... }
    }
}
```

The OK fast path (no chunks emitted) falls through to the standard
`UiCommand::Insert` of the deterministic-polished text — the user sees no
behavior change vs today for clean transcripts.

### 6.4 TTFT in the timing breakdown

Extend `SessionTiming` / the "inserted via ...; timing breakdown:" log line:

```
... polish {polish_ms}ms (ttft {ttft_ms}ms, streamed {stream_chunks}chunks) ...
```

`ttft_ms` is read from `outcome.diagnostics.ttft_ms`. When streaming is off it
is `None` and the segment is omitted (or shown as `?`).

### 6.5 Keepalive note

Streaming a single polish still holds `_llm_lock` for the whole loop.
Keepalive's `trylock` keeps skipping during a stream (a real call is in
flight). No code change; documented because the lock now holds for the
decode duration (slightly longer than the batch `run_chat_completion`, but
both are bounded by `dynamic_tokens`).

---

## 7. Settings (`apps/daemon/src/settings.rs`)

New field:

```rust
/// Progressive LLM-polish insertion: stream decoded tokens to the focused
/// window as they arrive (typing path), instead of one atomic paste after
/// the full completion. Default off — the validated clipboard-paste path is
/// canonical. Streaming paste uses CGEvent typing, the existing macOS
/// fallback; some apps may drop characters, in which case the daemon logs a
/// warning and the final clipboard commit is skipped (typed text is kept).
pub llm_polish_stream_insert: bool,
```

- `Default`: `false`.
- `llm_polish_command()` pushes `SUNOTO_LLM_POLISH_STREAM` = `"1"` iff this is
  `true`.
- `validate()`: only warns if `llm_polish_stream_insert && llm_polish_mode !=
  "constrained_one_call"` (streaming only implemented for the constrained mode;
  other modes fall back to batch silently). No hard error.
- Config JSON field `llm_polish_stream_insert`.

Tests:
- default is false; `llm_polish_command` env omits `SUNOTO_LLM_POLISH_STREAM`.
- with it true, env contains `SUNOTO_LLM_POLISH_STREAM=1`.

---

## 8. Platform adapters

- **macOS** (`crates/sunoto-macos/src/insertion.rs`): reuse `insert_direct`
  (CGEvent keystrokes per char) for chunk typing. No new API needed; the UI
  thread calls `adapter.insert_direct(&delta)` per chunk.
- **Linux/X11**: `insert_direct` paths newline/tab; `insert_via_clipboard` on
  unsupported chars. For streaming, treat an `UnsupportedCharacter` mid-stream
  as "switch to ClipboardFallback for the rest + commit the full final at end."
- **Wayland**: typing insertion falls back to clipboard typically — streaming
  insertion degrades gracefully to "accumulate + paste-at-end", which equals
  today's behavior. No regression.

No new public adapter methods; the existing typed-insertion API is reused.
The `UiBackend::insert_direct`-equivalent (`UiAdapter::insert_direct`) is
already exposed on both platforms.

---

## 9. Validation plan (the gate before flipping default-on)

1. Unit: Rust protocol tests parse `polish_chunk` + extended `polished`; the
   sidecar `_PrefixState` strips `EDIT:` and detects `OK` deterministically.
   Python unittest added under `tests/phase2` (stdlib unittest, no model load
   — pure parser test).
2. Offline sidecar smoke (no daemon): run the sidecar against a transcript
   with `SUNOTO_LLM_POLISH_STREAM=1` and capture stdout; assert ordered
   `polish_chunk` events precede the terminal `polished`, and `ttft_ms`
   appears on sequence 1 only.
3. Live macOS dictation (5 sessions, a disposable text target focused):
   - `llm_polish_stream_insert=false`: behavior unchanged, `ttft_ms` logged
     (best-effort; may be absent if streaming disabled — see §10).
   - `llm_polish_stream_insert=true`: text appears progressively; check no
     dropped-char apps; confirm `ttft_ms` and `stream_chunks` in the timing
     breakdown; confirm the OK fast path still does a single paste.
4. Content-loss guard: feed a transcript known to trip the guard (the §6.4
   "not done the testing" example) and confirm the daemon logs the
   post-stream guard revert warning (typed text already on screen is
   accepted).
5. Promote to default-on only after the 25-session gate from the latency
   doc passes under streaming.

---

## 10. Out of scope / follow-ups

- **Streaming for `one_pass_minimal` / `two_step`**: only the constrained mode
  streams (it owns the OK/EDIT prefix grammar the parser depends on). Extending
  is straightforward but unvalidated.
- **Speculative / draft decoding** (latency doc §4.7): still not needed;
  streaming insertion already overlaps decode with paste.
- **Guard-after-stream buffering** (§5 mitigation): defer until a real
  post-stream guard fire is observed.
- **Per-app paste-vs-type selection** for streaming: today streaming always
  types. An allow-list (paste for known-GUI-app, type for terminals) is a later
  knob.
- **Linux/CUDA streaming validation**: the sidecar code is platform-neutral,
  but live streaming on Linux (real ASR backend) is gated behind the latency
  doc's Linux validation.

---

## 11. Implementation order (this PR)

1. `settings.rs`: add `llm_polish_stream_insert` + env push + tests.
2. `llm_polish.rs`: protocol variants + diagnostics fields + streaming
   `polish<F>` callback + tests.
3. `llm_polish_sidecar.py`: `_PrefixState`, `constrained_payload_streaming`,
   `polish()` dispatch, ttft, terminal payload extension.
4. `daemon.rs`: `UiCommand` variants, UI-thread `StreamCtx`, hot-path wiring,
   timing-breakdown TTFT log.
5. `tests/phase2/test_llm_polish_stream_parser.py`: `_PrefixState` unit test.
6. `cargo test --workspace --offline` + `cargo clippy ... -D warnings` + the
   new Python suite; `make test`.
7. Update `docs/llm-polish-implementation-and-latency.md` §11 follow-ups to
   reference this streaming work.

# LLM Polish — Implementation & Latency Resolution

**Branch:** `llm-polish-research`
**Status:** enabled by default on macOS + Linux (commit `df3e6db0`, 2026-06-30)
**Last updated:** 2026-06-30

This document explains what LLM polish is, how it was wired into the daemon
architecture, the correctness/quality changes, and the latency investigation that
made it fast enough to run on every transcript.

---

## 1. What LLM polish does

After ASR produces a transcript and the **deterministic** polish pass
(`sunoto-polish`) handles mechanical cleanup (filler removal, repeat collapsing,
token-period handling), an opt-in **LLM polish** pass runs. The LLM's job is
narrowly scoped:

> **Merge mid-utterance self-corrections and rewordings ONLY.** It does *not*
> fix grammar, tense, agreement, punctuation, capitalization, or word order —
> those stay exactly as ASR produced them. The deterministic pass owns
> mechanical cleanup; the LLM owns only disfluency merges that a regex can't
> express (restarts, partial-overlap rewordings, mixed false starts).

The ASR artifact "police" (for "polish"), lowercase days/names, wrong verb
forms — all are preserved verbatim. Only superseded/abandoned speech is dropped.

## 2. Architecture

```
ASR sidecar ──transcript──▶ daemon event loop
                              │
                              ▼
                    deterministic polish (sunoto-polish)   ← mechanical, always on
                              │
                              ▼
                    LLM polish sidecar (Python, llama.cpp)  ← opt-in, now default-on
                              │  (NDJSON over stdout/stdin)
                              ▼
                    daemon: insert into focused window
```

Key files:

| Layer | File | Role |
| --- | --- | --- |
| Rust client | `apps/daemon/src/llm_polish.rs` | spawns sidecar, frames NDJSON protocol, `polish()` / `warmup()`, diagnostics structs, timeout handling |
| Rust settings | `apps/daemon/src/settings.rs` | `llm_polish_*` fields, `llm_polish_command()` (model path + mode + keepalive env), `validate()` |
| Rust daemon | `apps/daemon/src/daemon.rs` | spawn on startup, **post-ASR warmup handoff**, hot-path `client.polish()` before insertion, timing breakdown |
| Python sidecar | `services/polish/llm_polish_sidecar.py` | `load_model()`, `polish_payload()` per mode, background **keepalive thread**, warmup, NDJSON emit |
| Python prompt/once | `services/polish/llm_polish_once.py` | system prompts, few-shots, `clean_*_output()`, `validate()`-removed → `drops_content_unsafely` guard, `word_tokens` |

### 2.1 Daemon → sidecar protocol (NDJSON)

- Daemon spawns `llm_polish_sidecar.py` with stdin/stdout pipes.
- **Warmup:** daemon sends `{"type":"warmup","texts":[...]}`; sidecar runs the
  full completion path on canned texts, replies `{"type":"warmed","requests":[...]}`.
- **Polish:** daemon sends `{"type":"polish","session_id":N,"transcript":"..."}`;
  sidecar replies `{"type":"polished","session_id":N,**payload}` where `payload`
  includes `text`, `raw_output`, `latency_ms`, `output_mode`, `decision_label`,
  usage, cache diagnostics, and `llama_perf` (`prompt_eval`/`eval` per-token timings).
- **Timeout:** `polish()` waits `llm_polish_timeout_ms`; on timeout returns an
  error, daemon logs `llm polish skipped` and inserts the deterministic-polished
  text. (Empty-output → raw-transcript fallback lives in the sidecar.)

### 2.2 Post-ASR warmup handoff

The LLM is lazily warmed **after the ASR sidecar reports ready**, not at daemon
start. This avoids two heavy GPU models loading simultaneously during startup and
avoids warming the LLM (which would idle 16–32s while ASR loads) — see §4 for why
idle is the enemy.

Flow in `daemon.rs`:
1. `ASR sidecar ready` event arrives.
2. If `llm_polish_enabled` and not already warmed → push-to-talk is ignored,
   overlay shows "LLM polish still warming...".
3. Daemon calls `client.warmup(WARMUP_TEXTS, timeout)`.
4. On success: log `LLM polish post-ASR warmup complete`, allow push-to-talk.
5. On failure: disable LLM polish for this run (`llm_polish = None`), proceed.

## 3. Configuration

`Settings::default()` (applies to fresh `config init` and `Settings::default()`):

| Field | Default | Meaning |
| --- | --- | --- |
| `llm_polish_enabled` | **`true`** | opt into the LLM pass (was `false` until `df3e6db0`) |
| `llm_polish_model` | `"phi4_mini"` | named profile resolved to a repo-relative GGUF |
| `llm_polish_model_path` | `None` | override path (wins over `llm_polish_model`) |
| `llm_polish_mode` | `"constrained_one_call"` | dispatch mode (see §5) |
| `llm_polish_timeout_ms` | `30000` (live) / `10000` (default) | per-call watchdog |
| `llm_polish_keepalive_secs` | `1.0` | heartbeat interval (0 disables) |

Model resolution (`settings.rs::llm_polish_command`): an explicit
`llm_polish_model_path` always wins; otherwise `llm_polish_model` is resolved to
`models/llm-polish-hf/<profile>/...gguf` relative to the repo root and pushed as
`SUNOTO_LLM_POLISH_MODEL_PATH`. The mode and keepalive interval are pushed as
`SUNOTO_LLM_POLISH_MODE` / `SUNOTO_LLM_POLISH_KEEPALIVE_S`.

## 4. Latency: root cause and resolution

### 4.1 The bench-vs-live gap (the central surprise)

In the bench harness (`llm-polish-bench/bench_corpus_phase1.py`, `--post-asr-llm`
mode in `apps/daemon/src/bench.rs`), repeating the same audio N times produced
LLM polish p50 ≈ 234 ms. In **live dictation**, the same model showed
1.7–3.9 s per call, release→insertion 2.3–4.7 s. The bench was *flattered ~4–7×*
because repeating the same audio caused a full prompt-cache hit.

### 4.2 Generation is NOT the bottleneck

The llama.cpp `llama_perf` timings (`prompt_eval` per token vs `eval` per token)
proved decode was **always fast** (~26 ms/tok) regardless of live or bench. The
dominant cost was **suffix prefill** — re-evaluating the (cached prefix +
fresh transcript + few-shot) prefix on each new utterance. Bench hid this because
the suffix was identical across iterations.

### 4.3 Root cause: Metal GPU cold-ramp on prefill

A controlled probe (`llm-polish-bench/probe_idle_latency.py`,
`probe_idle_latency2.py`) measured suffix-prefill latency as a function of idle
time after a polish call:

| Idle before next call | Suffix prefill per token |
| --- | --- |
| warm (immediately) | ~13 ms/tok |
| ~30 s idle | **131–157 ms/tok (~10× cold)** |

Metal down-clocks the GPU cores when no work is submitted for a few seconds;
the next prefill stalls until they spin back up. The decode path is cheap (few
tokens) and was unaffected — only the prefill-heavy suffix was hit.

### 4.4 ASR ↔ LLM Metal GPU contention

A second probe (`llm-polish-bench/probe_asr_llm_contention.py`) proved ASR work
**colds the LLM prefill path ~50×**. ASR (MLX, burst or streaming) saturates the
GPU during transcription; when it releases, the LLM prefill is cold-ramped:

| Experiment | LLM prefill latency |
| --- | --- |
| EXP2: idle polish after ASR, no keepalive | 4025 ms ❌ |
| EXP7/EXP8: ~1 s keepalive **during** ASR | 194 ms ✅ |

The fix is keepalive: a tiny prefill every ~1 s during and after ASR **prevents
the cold entirely**.

### 4.5 Phase A — Keepalive heartbeat

The sidecar emits a 1-token completion on a monotonic counter suffix every
`keepalive_secs` (`SUNOTO_LLM_POLISH_KEEPALIVE_S`, default `1.0`; `0` disables).
A fresh counter token each ping forces a genuine prefill (warmth comes from
prefilling new tokens, not from a cache hit on a static prompt).

`KEEPALIVE_MAX_TOKENS = 1` (only one token generated — we don't care about the
generated text, only the prefill warmth). Keepalive pings emit **nothing on
stdout** (protocol safety — they must never be parsed as a polished event).

### 4.6 Phase B2 — Background-thread keepalive (final design)

Original keepalive ran in the sidecar's `select`-loop preamble, but ASR work
happens on the daemon side and could span the whole inter-ping window, starving
the heartbeat. Replaced with a **background thread** + `threading.Lock`:

```
keepalive_loop (bg thread)          polish()/warmup() (main stdio loop)
   │                                  │
   │  trylock (non-blocking)          │  with _llm_lock (blocking)
   │  └─ skip if locked               │     └─ runs llama.cpp call
   │     (a real call is warming      │
   │      the GPU itself)             │
   └─ keepalive(llm) if acquired       │
```

- `trylock`: keepalive **skips** when a real polish/warmup is in flight (that
  call is warming the GPU itself — pinging would only pile latency onto
  dictation, and llama.cpp isn't thread-safe so concurrent calls are forbidden).
- Blocking acquire on the main path: a polish arriving mid-ping waits at most
  the remaining ping wall time (bounded).
- Shutdown: `threading.Event` (`_keepalive_stop`) set on sidecar exit; the sleep
  is sliced so shutdown is responsive.
- `llama.cpp is NOT thread-safe` — the `threading.Lock` is **mandatory**, even
  though keepalive and polish are both CPU/GPU-bound work.

### 4.7 Why not speculative decoding / partials pipelining?

Generation is already fast (~26 ms/tok, 8–18 tokens/utterance). Speculative
decoding (Phase 6) and pipelining partial transcripts (Phase 7) **were not
needed** and were abandoned — the prefill cold-ramp was the only real cost, and
keepalive resolved it.

### 4.8 Latency results

Live dictation sessions (`/tmp/sunoto-daemon.log`), background-thread keepalive
at 1.0 s:

| Session | LLM polish gap |
| --- | --- |
| S5 | 866 ms |
| S6 | 956 ms |
| S7 | 1090 ms |
| S8 | 2774 ms (a real cold — see the content-loss bug below) |
| S9 | 753 ms |

3/4 sessions showed collision gaps of 0–10 ms; 6/9 sessions under the 0.9 s
budget. The remaining outliers (S8-class) were correctness failures, not
latency failures — see §6.

## 5. Dispatch modes (`SUNOTO_LLM_POLISH_MODE`)

| Mode | Path | Notes |
| --- | --- | --- |
| `constrained_one_call` (default) | single completion: LLM emits `OK` (clean) or `EDIT: <merged>` | fast path: clean text → 1 token; disfluent → reworded merge |
| `one_pass_minimal` | single completion, free-form | older, less-validated |
| `two_step` | decision completion → optional rewrite completion | used in early benchmarks; more expensive |

Allowed modes enforced in `Settings::validate()`. The mode is **always pushed
via env** so the live daemon runs the same path used in benchmarks (without it
the sidecar silently falls back to `one_pass_minimal`).

## 6. Correctness: prompt iterations and the validator

### 6.1 Prompt narrowing (Phase 5) — the biggest accuracy+latency win

The original prompt was vague (`"Remove fillers, repeats, false starts..."`).
Rewritten as `CONSTRAINED_SYSTEM_PROMPT` to **define disfluency by structure**
(redundant/abandoned/re-spoken speech), not by enumerated cue words:

> *"Judge disfluency by this STRUCTURE — redundancy/abandonment — NOT by whether
> a cue word is present... If there is NO such redundancy, the text is CLEAN,
> even if it has grammar, tense, agreement, capitalization, or word-order
> errors: output OK."*

This made clean text emit `OK` (1 token) → fast path, and narrowed the LLM's
authority to merge-only. Few-shots (`CONSTRAINED_REPAIR_FEW_SHOT`) cover: pure
repetition, redundant phrase, false start, no-cue disfluency, retraction,
unequal-reword pronoun preservation, restart cues, and local correction.

### 6.2 Partial-overlap restart teaching

Live session 1 ("Can you check if we are doing a good task? Sorry, sorry, can
you check if Check the logs...") revealed the LLM kept the superseded first
clause as a *separate sentence* and harmonized wording between the two attempts.
Added a partial-overlap restart few-shot and strengthened the restart rule to
make the LLM drop the ENTIRE earlier clause when the speech after the cue
re-opens the request.

### 6.3 The validator removal — too brittle for production

A deterministic validator (`validate()` + ~15 helpers: `drops_content_unsafely`,
`drops_negation_unsafely`, `drops_meaningful_leading_marker`,
`introduces_*_formatting`, …) with hard-coded thresholds (35% content drop),
stopword lists, and the `EXPLICIT_CORRECTION_MARKERS` enumeration (notably
**missing "sorry"**) sat on top of the LLM output. On real speech it blocked
legitimate merges — session 1's correct restart merge was discarded because the
dropped `{task, sorry}` triggered `content_dropped` (sorry not in the marker
list). The partial-overlap restart teaching **could not take effect** while this
gate sat on top.

**Removed wholesale** (`2e08ebed`): deleted `validate()`, the 14 helpers, 6
constant tables, the unused `Counter` import; unwound `hard_unsafe`/`review_flags`/
`validation_rejected` from the Python payloads, the Rust diagnostics structs,
the `Polished` event, the `polish()` match arm, and the daemon's JSON + timing
output (including the `if !hard_unsafe.is_empty()` error early-return).

The LLM is now **authoritative** for the merge — the prompt is the only thing
governing correctness.

### 6.4 The narrow content-loss guard (safety net)

Removing the validator exposed the failure it had been masking: live session 8
("Now, whatever configuration we are using for the LM polish, keep that as the
default version and push the changes, sorry, um, commit the changes first.")
was edited to "Keep that as the default version and push the changes, commit
the changes first." — the **topic clause vanished** and "that" lost its antecedent.

Two causes:

1. **Prompt bug**: the (post-§6.2) restart rule said *"an earlier clause is
   abandoned after sorry/actually... drop the ENTIRE earlier clause."* The LLM
   read a *local* mid-sentence "sorry" (correcting one short phrase —
   "push the changes" → "commit the changes first") as a full restart and
   dropped everything before it. Reworded the rule (`80a7614a`): a correction
   cue that fixes only a short phrase is **LOCAL** (keep the topic clause and
   all surrounding text, drop only the corrected phrase + cue); a **RESTART**
   is rare and requires the speech after the cue to *re-open the request*
   (re-state the topic / repeat opening words). Added a local-correction
   few-shot.
2. **Safety net**: re-added a **single narrow** check, `drops_content_unsafely`,
   in `llm_polish_once.py`. It fires **only** when ≥3 **unique** significant
   content words are dropped with **no counterpart** in the retained text — i.e.
   the topic evaporated rather than being re-stated by a restart. Correction
   cues (sorry/actually/…) are excluded from the count. A true restart re-states
   the request, so its dropped words are either re-stated (absent from the set
   difference) or few (entity swap) — keeping it under the threshold. On a hit
   the sidecar reverts to the raw transcript and logs
   `content-loss guard fired`.

   Verified: session 8 (topic drop) → fires; session 1 restart, partial-overlap
   restart, send-to-bob→alice restart → all pass. The guard is **pure-Python**,
   invisible to the daemon contract — no `hard_unsafe`/`review_flags`/
   `validation_rejected` re-threaded through Rust.

This is deliberately **not** the old heuristic suite — no negation check, no
digit/format detection, no leading-marker rule. The prompt owns those; the guard
is a last-resort sanitizer for one specific, recurring failure mode (topic
clause loss masquerading as a restart).

## 7. Deterministic polish bug: intra-token period mangling

A separate but entangled issue: `sunoto-polish` (`crates/sunoto-polish/src/lib.rs`)
had three cooperating rules that mangled periods — any `.` mid-utterance was
treated as a sentence boundary. This corrupted filenames/domains/versions
("agents.md" → "agents. md" / "agents. Md"), and on session 8 broke the LLM
input itself.

Fixed `0db9add3`:

1. **`normalize()`**: only insert a space after `.` when the next char is
   **uppercase** (real sentence start). Lowercase → intra-token → no split.
2. **`capitalize_sentences()`** (Prose style): no longer capitalizes after `.`
   — only after `!`/`?`.
3. **`detokenize()`** (the hidden third mangler — fires during filler/
   correction removal): won't insert space after `.` before a lowercase word
   (intra-token); keeps space before an uppercase word (sentence start).

Intentional behavior change: a period mid-utterance no longer auto-capitalizes
the next word. 3 new regression tests + 1 updated expectation.

## 8. Benchmarking harness

| File | Purpose |
| --- | --- |
| `llm-polish-bench/bench_corpus_phase1.py` | corpus-driven bench (WAV→ASR→deterministic polish→LLM→JSON) |
| `apps/daemon/src/bench.rs` | `--post-asr-llm` bench mode in the daemon (no insertion/UI) |
| `llm-polish-bench/e2e_phi_polish.py` | end-to-end polish correctness harness |
| `llm-polish-bench/corpus-live-20260630.jsonl` | 10-scenario corpus |
| `llm-polish-bench/probe_*.py` | idle-latency and ASR/LLM contention probes (§4) |
| `llm-polish-bench/sim_live_dictation.py` | simulates live dictation cadence |

llama.cpp runtime: `n_gpu_layers=-1`, `flash_attn=True`, `n_ctx=2048`,
`n_batch=512`, `n_ubatch=512`, `n_threads=8`, `temperature=0.1`, `top_k=50`,
`top_p=0.95`, `repeat_penalty=1.05`, `seed=42`, 512 MiB prompt cache.
Model: `models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf`
(Phi-4-mini Q5_K_M).

## 9. Linux / NVIDIA portability

Keepalive is **platform-neutral** — it's a Python thread + llama.cpp calls, no
macOS APIs. The Linux path may not need it (CUDA's clock behavior differs from
Metal's aggressive down-clock), but the design is safe there too (the `threading.Lock`
guards correctness, not just warmth) and is configurable via
`SUNOTO_LLM_POLISH_KEEPALIVE_S` (set `0` to disable on Linux if profiling shows
no cold-ramp). The default Linux ASR backend is `mock`, so without a real ASR
backend configured the LLM pass runs on mock transcripts — that's a config
choice, not a portability issue.

## 10. Commit history (this branch)

| Commit | Summary |
| --- | --- |
| `df3e6db0` | settings: enable llm_polish by default (macOS + Linux) |
| `80a7614a` | llm-polish: narrow content-loss guard + fix over-broad restart rule |
| `2e08ebed` | llm-polish: remove the deterministic validator (LLM is authoritative) |
| `beeee1ee` | llm-polish: teach partial-overlap restart merges |
| `6438a058` | llm-polish: replace cue-word gate with disfluency-by-structure prompt |
| `1d61eff1` | llm-polish: silence per-ping keepalive log |
| `e8f3eff7` | llm-polish: opt-in LLM polish sidecar + keepalive, ASR/LLM contention fix |
| `0db9add3` | polish: stop splitting/capitalizing intra-token periods |

## 11. Open follow-ups

- **Task-specific finetune of Phi-4-mini** on the merge-only contract — the
  noted future direction. Phi-4-mini Q5 can't reliably separate "false start"
  from "subject-verb disagreement"; mild grammar edits are accepted as a
  tradeoff. A finetune would close the grammar-leakage tail and remove the need
  for the §6.3/§6.4 prompt whack-a-mole.
- **Hallucination guard** (Phase B, after latency) — a prompt rule for
  abandoned/incomplete clauses plus a subsequence check.
- **25-session real-dictation promotion gate** (Phase C) — the default-on
  change shipped on the strength of the structured probes above; a longer
  in-distribution run is the real confidence test.

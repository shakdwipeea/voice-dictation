# LLM-polish prompt eval loop — single-call adjudicator plan

Status: **proposed** (awaiting sign-off before building or running). Supersedes
the evaluator and proposer mechanism of
`docs/llm-polish-prompt-eval-loop-plan-2026-06-30.md` §2.3–§2.5. The loop
shell, failure-driven philosophy, worst-of-K stability, deterministic
companion layer, and "no cloud at dictation time" invariant from that doc
are unchanged; this doc replaces the **multi-call judge + proposer**
mechanism with a **single gpt-5.5 call per round** that adjudicates the
current candidate and produces the next prompt in one structured response.

## 0. What changed (TL;DR)

Driven by the user's direction: drop the cost cap, and make **one gpt-5.5
call per round** that has access to *everything* — deterministic numbers,
the target model's actual outputs, the full round history including prior
LLM verdicts — and both (a) judges the current candidate and (b) decides
whether to propose a new prompt, keep the previous one, or stop because
prior rounds were better.

Concretely, per round there is exactly:

1. **Local-only, deterministic:** the target model (Phi-4-mini, worst-of-K)
   runs the current prompt → outputs + deterministic metrics (exact%,
   content_drop per row, over_edit count, format-conformance, latency).
   Structural hard gates (format, latency) reject obviously-broken
   candidates before any gpt call.
2. **One gpt-5.5 call** (the "adjudicator"): receives the corpus, the
   current prompt, the target's per-row outputs, the deterministic numbers,
   and the round history (prior prompts + their metrics + prior verdicts).
   It returns, in one structured response:
   - per-row verdicts (`content_preserved`, `appropriately_cleaned`,
     `over_edited`, `severity`, `note`),
   - an aggregate verdict vs best-so-far (`pass` | `improvement` |
     `regression` | `neutral`),
   - an action (`promote_and_stop` | `continue` | `stop`),
   - and, if `continue`, the next candidate prompt + few-shot (or no
     proposal if the model judges that the best-so-far cannot be beaten —
     the user's "if they think the previous runs have been better").

No separate `judge.py` + `propose.py`; no `codex exec`; no per-row judge
calls; no cost cap. Deterministic logic stays as a hard pre-filter and as
context fed into the single call.

## 1. Why exact-match is the wrong primary metric (unchanged)

Text cleanup has many valid outputs. `"Um, I went to the store."` →
`"I went to the store."` is correct, but so is keeping a different filler
handling. The current `filler=0%` and `long_utterance=0%` scores are not
"the model fails" — they are "the model's acceptable variant doesn't
byte-match the gold." `exact≥88%` therefore penalizes good-but-different
cleanups and gives zero signal on the genuinely dangerous class: did we
lose the user's **meaning**? Exact-match also has no notion of *severity*;
a dropped "however" (recoverable) and a dropped "email" entity
(catastrophic) both count as one row. The adjudicator distinguishes them.

## 2. Why the judge must be the stronger model (unchanged)

Same-model LLM-as-judge has well-documented self-preference bias: a model
reviewing its own outputs rubber-stamps its own style and would miss the
content-loss class the loop exists to fix. The adjudicator is **gpt-5.5**,
strictly stronger than the 3.8B Phi-4-mini target. (Prior plan §1.3.)

## 3. Architecture

```
seed (live CONSTRAINED_SYSTEM_PROMPT + CONSTRAINED_REPAIR_FEW_SHOT)
  │
  ▼
┌──────────────────────────────────────────────────────────────┐
│ ROUND:                                                        │
│ 1. LOCAL (deterministic): target model runs current prompt     │
│    (worst-of-K). Compute: exact%, per-row content_drop,         │
│    over_edit count, format_conformance (malformed rate),        │
│    latency p50/p95, and the worst-K per-row outputs.            │
│ 2. DETERMINISTIC HARD PRE-FILTER (no gpt call):               │
│    - format_conformance == 1.0   (else reject candidate)       │
│    - latency within budget vs baseline (else reject)           │
│    Rejected candidates skip the gpt call; they count toward     │
│    stagnation and the loop re-proposes from best-so-far.       │
│ 3. ONE gpt-5.5 ADJUDICATOR CALL with full context:              │
│    - corpus (84 rows: id, category, input, gold)               │
│    - current candidate prompt + few-shot                       │
│    - target's worst-K per-row outputs                          │
│    - deterministic numbers (exact%, content_drop per row,      │
│      over_edit, latency)                                       │
│    - round history: prior prompts' metrics + prior verdicts   │
│    Returns one structured JSON:                                │
│      per_row: [{id, content_preserved, appropriately_cleaned,   │
│                 over_edited, severity, note}, ...]             │
│      aggregate: {quality_score, content_drop_violations,        │
│                  verdict: pass|improvement|regression|neutral} │
│      action: promote_and_stop | continue | stop                │
│      next_prompt?: {system_prompt, few_shot, rationale}        │
│        (present iff action==continue)                          │
│ 4. ACT on the verdict:                                          │
│    pass          → PROMOTE next_prompt (if given) or current;   │
│                    edit llm_polish_once.py constants; STOP.     │
│    improvement   → current becomes best-so-far; adopt            │
│                    next_prompt; next round.                      │
│    regression    → revert to best-so-far; adopt next_prompt     │
│                    (different direction); stagnation++.         │
│    neutral       → keep best-so-far; adopt next_prompt;         │
│                    stagnation++.                                 │
│    stop          → loop ends (adjudicator judges best-so-far     │
│                    cannot be beaten); residuals seed            │
│                    future finetune.                              │
│ 5. STAGNATION_ROUNDS(2) reached → STOP regardless.              │
└──────────────────────────────────────────────────────────────┘
```

### 3.1 Deterministic layer (kept — hard pre-filter + context provider)

| check | role |
|---|---|
| format conformance (OK/EDIT malformed flag) | HARD pre-filter gate. The session-8 chatbot-refusal fix stays; a candidate that broke the contract is rejected without a gpt call. |
| empty/malformed → fail closed to transcript | live hot-path guarantee (unchanged, in sidecar). |
| gold-based content-drop tripwire (`content_dropped(expected, out)`) | **context only**: flagged words are passed to the adjudicator as "deterministic check flagged these words dropped on row Y — adjudicate meaning loss." Not a standalone gate. |
| exact-match | reported metric + context; demoted from gate. |
| latency p50/p95 | HARD pre-filter budget vs baseline. |
| worst-of-K (target model) | model-side variance handling (unchanged). |

These are reproducible, free, and fast; they run before the gpt call and
pass their numbers INTO the call as context so the adjudicator reasons
about grounded data, not vibes.

### 3.2 The single adjudicator call (replaces judge + proposer)

`adjudicate(corpus, current_prompt, few_shot, worst_k_outputs, det_metrics, history) -> Adjudication`

where `Adjudication` is:

```python
class RowVerdict(TypedDict):
    id: str
    content_preserved: Literal["yes", "no", "borderline"]
    appropriately_cleaned: float        # 0.0–1.0
    over_edited: bool
    severity: Literal["none", "minor", "major", "critical"]
    note: str                            # one short sentence, "none" if none

class Adjudication(TypedDict):
    per_row: list[RowVerdict]
    quality_score: float                 # 0.0–1.0, see §4.1
    content_drop_violations: int         # rows where content_preserved=="no"
    verdict: Literal["pass", "improvement", "regression", "neutral"]
    action: Literal["promote_and_stop", "continue", "stop"]
    next_prompt: NotRequired[dict]      # {system_prompt, few_shot, rationale}; only if action==continue
```

**Fail-closed rule (critical):** `content_preserved == "borderline"` is
treated as `"yes"` for the violation count. The adjudicator must produce a
clear "dropped real meaning" verdict to register a violation. This
prevents an over-strict judge from blocking good candidates — the failure
mode the prior loop hit (87.5% → 78.1% regression from selection noise).

**Adjudicator system-prompt contract** (medium reasoning effort):

> You are the optimizer for a small-model (Phi-4-mini, 3.8B) voice-dictation
> polish prompt. You receive: the eval corpus, the current prompt, the target
> model's actual outputs on this round, deterministic metrics (exact-match
> %, per-row content-drop flags, over-edit count, latency), and the history
> of prior rounds (their prompts, metrics, and your prior verdicts).
>
> Judge each output on: (a) `content_preserved` — did it keep the speaker's
> intended meaning and all key entities/facts/numbers/emails/verbatim text?
> When uncertain, answer `"borderline"` (treated as pass). (b)
> `appropriately_cleaned` — did it strip genuine disfluency (fillers,
> restarts, redundant repeats) without inventing changes? (c) `over_edited`
> — did it change already-clean or verbatim-required text? Typos, casual
> spellings ("thx", "idk"), lowercase, missing punctuation are CONTENT, not
> errors to fix.
>
> Then decide: is this candidate a `pass` (semantic gate satisfied:
> content_drop_violations==0 and quality its meaning vs best-so-far),
> `improvement` (better than best-so-far but not yet passing),
> `regression` (worse than best-so-far), or `neutral`.
>
> Action: `promote_and_stop` if verdict==pass; `continue` if there is a
> promising next prompt to try (provide it, minimal change, fix the shown
> failures — contrast/restart/verbatim rules apply; system prompt stay
> terse, few-shot small, you may add one targeted example); `stop` if you
> judge best-so-far cannot be beaten (prior rounds were better) and no
> further prompt change is worth trying.
>
> Return ONLY the JSON schema.

Few-shot inside the adjudicator: 4 anchored examples covering (session-8
chatbot-refusal = major violation + regression; dropped "however" = minor +
neutral; "thx"→"Thanks" = over_edited + regression; filler stripped
correctly = none + improvement). Anchoring on real prior failures
calibrates severity and the verdict labels.

### 3.3 Round 0 (baseline)

Before any proposal, run the seed prompt through step 1–3 with an empty
history. The adjudicator returns per-row verdicts + a baseline
`quality_score` + (almost always) `action=continue` with the first
proposed prompt. This establishes the best-so-far and the **baseline
latency** the latency-budget gate compares against.

## 4. The hybrid gate (§2.5 replacement)

Promotion (`action == promote_and_stop`) requires **all** of:

| gate | source | rule |
|---|---|---|
| format_conformance | deterministic | `malformed_rate == 0` (HARD pre-filter) |
| latency | deterministic | `p50 ≤ baseline_p50 + 30ms` AND `p95 ≤ baseline_p95 + 50ms` (HARD pre-filter) |
| content_preserved | adjudicator (HARD) | `content_drop_violations == 0`, fail-closed on borderline |
| quality | adjudicator + deterministic | `verdict == pass` (adjudicator's holistic call, grounded in det metrics + outputs + history) AND `exact_pct ≥ baseline_exact − 3pp` (don't regress raw accuracy while improving judged quality) |

The deterministic content-drop tripwire does not gate on its own. It is
passed into the adjudicator as context; a flagged row forces the adjudicator
to explicitly reason about that row's dropped words. If the adjudicator then
returns `content_preserved: "no"` with severity ≥ major, it counts as a
violation (catches session-8-class single-word drops that fall below the
≥3-word deterministic threshold — the judge is the net, the tripwire is the
prompt to look).

### 4.1 quality_score definition

`quality_score` is computed by the adjudicator as the per-row mean of:
- on rows the gold marks as needing edit: `appropriately_cleaned`,
- on rows the gold marks as OK/verbatim: `1.0` if not over_edited, else `0.0`.

The adjudicator is instructed to compute this consistently across rounds so
the score is comparable. It is reported alongside exact% in every round
dump for human inspection.

## 5. Cost & control (no cap)

Per the user's direction, there is **no cost cap**. The design is naturally
cheap regardless: one gpt-5.5 call per round (the adjudicator), 6 rounds
max ≈ 6 calls plus the round-0 baseline. Worst-of-K target runs are local
and free. No per-row judge calls, no separate proposer call, no `codex exec`
subprocess.

Context size per call: ~84 rows × ~100 tokens (input+gold+output+verdict)
≈ 8–10k, + current prompt (~1k) + history (grows ~1.5k/round, ~10k max) +
instructions + anchored few-shot (~2k) ≈ 20–25k input tokens, ~4–5k output.
Comfortably within gpt-5.5's 200k context. If a corpus grows large enough
that a single call becomes unwieldy, the fallback is to send only the
disagreement rows (deterministic-flagged + adjudicator-flagged last round)
plus a sample of agreements — but the current 84-row corpus needs no such
sharding.

No verdict cache (the combined call gets fresh full context each round;
caching at the row level does not compose with a single-call design and the
cost is negligible without a cap).

## 6. File list (to build after sign-off)

| file | action | purpose |
|---|---|---|
| `llm-polish-bench/openai_client.py` | NEW | `OpenAIClient`: loads key from `llm-polish-bench/.env`, structured-output helper (`response_format` json_schema), retry/backoff, reasoning-effort presets (low/medium). One method: `complete(messages, schema, reasoning_effort)`. |
| `llm-polish-bench/adjudicator.py` | NEW | `Adjudication`/`RowVerdict` schemas, the system-prompt contract, anchored few-shot examples, `build_context()` (assembles corpus + outputs + det metrics + history), `adjudicate()` (one client call → parsed `Adjudication`), fail-closed logic, the `apply_verdict()` action handler (promote/revert/stop + history append). |
| `llm-polish-bench/adjudicator_few_shot.jsonl` | NEW | the 4 anchored graded examples for the adjudicator prompt. |
| `llm-polish-bench/eval_constrained.py` | MODIFY | `evaluate()` returns the worst-K per-row outputs + deterministic metrics dict (it already returns `rows` with `out`/`dropped`/`lat`); add `malformed_rate` and keep worst-run rows for the adjudicator. Worst-of-K unchanged. |
| `llm-polish-bench/optimize_constrained_loop.py` | MODIFY | rewrite the round body: deterministic pre-filter → single `adjudicate()` call → `apply_verdict()` action. Drop the `propose()` codex call, the `objective()`, the `passes_gate()` deterministic-only gate (replaced by adjudicator verdict + det pre-filter). Keep worst-of-K, keep stagnation, keep promotion (write best prompt into `llm_polish_once.py` constants via a `promoted_prompt.json` the human reviews). |
| `.venv-llm-polish-mac` | MODIFY | `pip install openai` (httpx already present). |
| `docs/llm-polish-implementation-and-latency.md` | MODIFY | append §12 noting the adjudicator layer exists and is offline-only (no gpt-5.5 at dictation time). |
| `AGENTS.md` | VERIFY | `.env` already gitignored (confirmed). Note the key + the offline-only invariant in "Runtime safety" so future agents don't commit the key or wire gpt-5.5 into the hot path. |

The Rust daemon, sidecar, and hot path are **untouched**. gpt-5.5 runs only
inside the offline loop; no cloud call exists at dictation time.

## 7. Open questions to resolve before building

1. **gpt-5.5 API shape.** Exact call signature (Responses API vs Chat
   Completions, `reasoning.effort` vs `model_reasoning_effort`, structured
   output via `response_format` json_schema vs function calling) needs a
   5-minute smoke test against the live key before committing the client
   wrapper. Design is API-shape-agnostic; only `openai_client.py` internals
   depend on the answer.
2. **Adjudicator calibration.** Before round 1, run the adjudicator on the
   **baseline** (seed prompt) outputs and eyeball the per-row verdicts on
   the 2 known content-drop rows (restart-01, rp-03) and the `filler` 0%
   rows. If the adjudicator marks the filler rows as
   `appropriately_cleaned: high` with `content_preserved: yes` (no
   violation), the LLM metric is doing its job and the loop's target
   correctly shifts to the real residual failures. If it marks them
   violations, the adjudicator prompt needs more anchoring before round 1.
3. **Self-preference-on-prior-proposal risk.** In round N+1 the adjudicator
   judges the prompt it proposed in round N. Mitigation: the deterministic
   metrics and the actual outputs are ground truth it cannot deny, and the
   history lets it see "my round-N proposal scored worse than round-(N-1)."
   Monitor the first 2 rounds for leniency-on-self; if observed, split
   judging and proposing into two calls (judge the current candidate →
   then a separate propose call that sees the verdict). Default: combined.
4. **Δ for the quality/exact regression floor.** Default exact floor is
   `baseline_exact − 3pp`; reconsider after seeing baseline adjudicator
   score distribution. The point is to allow quality to rise even if exact%
   dips slightly, but not to let exact% collapse.
5. **Promotion requires daemon-architecture cross-check.** Before a
   promoted prompt flips `CONSTRAINED_*` defaults, run
   `bench_daemon_architecture.py` (real daemon latency, ASR-contending) to
   confirm the isolated-loop latency holds under GPU contention. The
   adjudicator's latency is from the isolated loader; the daemon path can
   be slower. (Not wired until a candidate passes.)

## 8. What does NOT happen (invariants preserved)

- **No cloud call at dictation time.** gpt-5.5 runs only inside the offline
  loop. The promoted artifact is a prompt edit in `llm_polish_once.py`;
  dictation still runs Phi-4-mini + llama.cpp locally. (Same invariant as
  prior plan §5.)
- **No removal of deterministic logic.** Format conformance, malformed
  fail-closed, gold-based content-drop tripwire, exact-match, latency, and
  worst-of-K all remain. The adjudicator is added on top, not substituted
  in. The session-8 malformed→fail-closed fix on the hot path is a runtime
  guarantee, untouched.
- **No self-judge.** Phi-4-mini never grades itself. The adjudicator is
  always gpt-5.5, always strictly stronger than the target.
- **No heuristic content-drop guard on the hot path.** Per the user's
  standing instruction, fixes come from the prompt/model side. The
  deterministic tripwire lives in the **eval harness** only.
- **No codex exec dependency.** The loop is direct OpenAI API calls. The
  `codex` CLI is no longer needed (available for ad-hoc research only).
- **No cost cap** (per user direction). The single-call-per-round design
  is cheap enough to not need one.
- **No retraining during the loop.** LoRA/QLoRA on accumulated failures is
  a long-term follow-up (prior plan §1.4), not part of this build.

## 9. Verification plan (before declaring the loop working)

1. **Client smoke:** one `adjudicate()` call on the baseline outputs
   returns a well-formed `Adjudication`; the 2 known content-drop rows are
   flagged `content_preserved: "no"` severity ≥ major, and the `filler`
   0%-exact rows are flagged `content_preserved: "yes"` with
   `appropriately_cleaned: high` (no violation). Confirms the LLM metric
   agrees with the known failure profile.
2. **Fail-closed check:** construct a borderline output (one ambiguous
   word drop) and confirm the adjudicator returns `"borderline"` and it is
   NOT counted as a violation.
3. **Round 1 end-to-end:** baseline → worst-of-K → deterministic pre-filter
   → adjudicator → action. Observe: (a) the gate's content-preservation is
   the adjudicator, not the ≥3-word deterministic rule; (b) the loop runs
   in well under a minute per round (one gpt call + local target eval);
   (c) the failure collection is implicit in the adjudicator's per-row
   notes, which feed the next proposal.
4. **Honest-negative readiness:** the loop earns trust only when it can
   demonstrate both directions on real outputs — a candidate the
   deterministic layer alone would have rejected but the adjudicator
   accepts (loop working), AND a candidate the deterministic layer accepted
   but the adjudicator rejects (safety net working). Both must appear in
   the first few rounds before trusting promotions.
5. **Self-preference audit:** after rounds 1–2, inspect whether the
   adjudicator is systematically lenient on prompts it proposed. If yes,
   split judge/propose (§7.3).

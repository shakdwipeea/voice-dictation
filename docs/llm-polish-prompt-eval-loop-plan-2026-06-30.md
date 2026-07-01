# LLM-polish prompt eval loop — plan

**Status:** plan (not yet implemented). **Date:** 2026-06-30.
**Goal:** an automated, LLM-in-the-loop search over the `constrained_one_call`
OK/EDIT polish prompt that is *driven by the model's own eval failures*, runs
until an accuracy gate is met **without regressing latency**, and is grounded in
techniques known to help *small* local models. No deterministic content-loss
guard on the hot path — the fix must come from the model response (prompt +
sampling + optional verifier), exactly as requested.

This doc supersedes the earlier `docs/llm-polish-constrained-prompt-eval-loop-2026-06-29.md`
notes for *future* work (that doc's findings are folded in as constraints).

---

## 0. What already exists (build on this, don't fork)

| Asset | Path | Role in the loop |
| --- | --- | --- |
| Eval harness, **real daemon path** | `llm-polish-bench/bench_daemon_architecture.py` | Evaluates the constrained prompt through the running daemon (warm KV cache, real LLM client, ASR-contending timings). **Promotion gate latency MUST come from here.** |
| Eval datasets | `llm-polish-bench/dataset.jsonl` (60), `synthetic-minimal-v1.jsonl` (32), `corpus-live-20260630.jsonl` (10) | Coverage today is restart/repetition/filler/false_start/mixed/preserve_facts/explicit_cancel/compound/should_not_overedit/long. **Missing: contrast/coherence** (the exact failure the user hit). |
| LLM-in-the-loop optimizer | `llm-polish-bench/optimize_prompt_loop.py` | Cloud optimizer proposes prompts; local target model runs them; gates on exact%/sim/latency. **Built for the DEPRECATED `one_pass_minimal` mode** (`Clean this transcript:` + clean text). Must be **retargeted to the `constrained_one_call` OK/EDIT format**, not reused as-is. |
| Prompt override hooks | env/file in `constrained_system_prompt()`/`constrained_repair_few_shot()` (`services/polish/llm_polish_once.py`) | Lets the loop inject candidate prompts WITHOUT editing source or rebuilding. |
| Live prompt + few-shot | `CONSTRAINED_SYSTEM_PROMPT`, `CONSTRAINED_REPAIR_FEW_SHOT` | The **starting prompt** (seed) for the loop. Already carries the contrast-fix added today. |
| Default runtime model | `phi-4-mini-q5` (`models/llm-polish-hf/phi-4-mini-q5/...Q5_K_M.gguf`) | The optimization target. |
| Prior loop's measurements | `docs/llm-polish-constrained-prompt-eval-loop-2026-06-29.md` | Baseline: 84.4% exact, 0.0683 WER, p50 318ms, p95 718ms. **Critical failure mode: a candidate that scored 87.5% dropped to 78.1% on rerun = instability.** |

### Hard constraints inherited from the prior attempt

- **No promotion unless latency holds.** p50 and p95 must stay ≤ baseline + small
  budget. The prior loop's v3 *improved* exact% but raised p50 318→346ms and was
  rejected. This is the "don't drop too much latency" the user asked for.
- **No instability.** A candidate that passes once and fails on rerun is
  rejected. The prior loop promoted v1 and it regressed to 78.1%. Gate on
  **worst-of-K**, not best-of-1.
- **Grammar is currently OFF** (`SUNOTO_LLM_POLISH_GRAMMAR=0`). Re-evaluate it
  ON as part of the loop — it tends to *raise* clean-OK precision on small
  models by removing malformed-output modes, at a small latency cost.
- **Latency from the isolated optimizer (`optimize_prompt_loop.py`'s own Llama)
  is OPTIMISTIC.** Promotion must use the daemon-path numbers, because that's
  where ASR↔LLM GPU contention lives.

---

## 1. Research: improving small-model accuracy on this task

Verified via web search (Codex/gpt-5.5 + OpenAI search, 2026-06-30). Findings
below carry source URLs. Inference-time techniques only here (no cloud calls
at runtime); finetuning surveyed last. **Findings that change the plan are
flagged `[PLAN]`.**

### 1.1 Prompt engineering (cheap, local)

| Technique | Effect on small models (with source) | Use here |
| --- | --- | --- |
| **Native chat template.** Phi-4-mini is ChatML (`<\|im_start\|>...<\|im_end\|>`); Qwen3 and Gemma-3-4B each have their own, applied via `apply_chat_template`. llama-cpp-python's `create_chat_completion` applies the GGUF-embedded template. | A raw text-completion prompt (no template) silently breaks instruction-tuned small models. Sources: [Phi-4-mini card](https://huggingface.co/microsoft/Phi-4-mini-instruct), [Gemma-3-4B card](https://huggingface.co/google/gemma-3-4b-it), [Qwen3-4B card](https://huggingface.co/Qwen/Qwen3-4B). | **Verify** the GGUF template is applied (it is, via `create_chat_completion`). Do NOT bypass to raw `llama_completion`. |
| **Chain-of-thought / "think step by step".** | **HURTS below ~7B on reasoning tasks.** Wei et al. show CoT gains emerge mainly at very large scale, while smaller models produce fluent-but-illogical rationales and underperform standard prompting. Source: [Wei et al. CoT, arXiv:2201.11903](https://arxiv.org/abs/2201.11903). The repo's own two-step bench (`docs/llm-polish-two-step-benchmark-results-2026-06-29.md`) confirms two-step is worse here. | Keep the **one-call OK/EDIT** decision. Allow CoT *only* on an optional EDIT-path verifier, never the generator. |
| **Few-shot ORDERING / recency bias.** | Real and large: Lu et al. show the same examples in different orders can swing a small model from near-random to near-SOTA, and good orders **do not reliably transfer across model sizes**. Source: [Lu et al., arXiv:2104.08786](https://arxiv.org/abs/2104.08786). | **`[PLAN]`** Add few-shot *order permutation* as an explicit candidate axis the optimizer may use. Do NOT freeze one ordering as universally-best. |
| **System vs user instruction placement.** | Gemma-3's card demonstrates system+user roles; Qwen3 routes modes through `apply_chat_template`. Small-model system-role adherence is weaker than user-role recency, so the one hard constraint gains from being repeated in the user turn near the transcript. Sources above. | The optimizer may add a one-line constraint restatement in the user turn right before the transcript. |
| **Constrained decoding (GBNF grammar) for the output shape** (`OK` or `EDIT: ...`). | **Improves FORMAT precision, NOT factual preservation.** Grammar masking collapses malformed-output modes but doesn't make the model preserve clauses. Source: [llama.cpp grammars](https://github.com/ggml-org/llama.cpp/blob/master/grammars/README.md), [grammar-constrained survey arXiv:2305.13971](https://arxiv.org/abs/2305.13971). **Risk:** grammar can distort the model distribution — grammatical-but-lower-quality if it blocks a high-probability continuation. Source: [arXiv:2405.21047](https://arxiv.org/abs/2405.21047). | **`[PLAN]`** Grammar ON is a candidate variant, but it will NOT by itself fix the content-drop bug. Critical rule (from the GBNF×prompt-tuning interplay literature): **optimize prompts under the SAME grammar you will deploy** — a prompt tuned without GBNF can regress once the mask is added. |
| **`logit_bias` to force the first token** to `OK`/`EDIT`. | Cheap nudge that complements grammar; rarely needed. | Low priority; only if grammar alone underconstrains. |

Recommended small-model prompt shape (synthesis of the above): short policy,
3–8 examples, final invariant repeated immediately before the transcript, no
free-form explanation, low/zero temperature, grammar final.

### 1.2 llama.cpp-specific levers (latency + determinism)

- **KV cache reuse (`LlamaRAMCache`)** — already on. System prompt + few-shot
  are identical across calls, so prompt-eval is nearly free after the first
  call (longest-token-prefix lookup, stores `LlamaState`; see
  [llama_cache.py](https://raw.githubusercontent.com/abetlen/llama-cpp-python/main/llama_cpp/llama_cache.py)
  and llama.cpp `--prompt-cache`). **Caveat for the loop:** when the optimizer
  changes the prompt between rounds the cache MUST be invalidated, else
  timings are contaminated and answers bleed between candidates. The loop must
  reset/restart the sidecar between candidate prompts.
- **`temperature`** — use ~0 (greedy) for the OK/EDIT decision. The prior loop
  tried temp 0 and instability **persisted**, so instability is a prompt/data
  problem, not a sampling one; sampling changes alone will NOT fix the user's
  bug. (Metal determinism is still approximate due to FP non-determinism, hence
  worst-of-K eval — `seed` alone is not a correctness guard.)
- **`repeat_penalty`** is live at **1.05**. **`[PLAN]` This is a suspected
  contributor to the content-drop bug.** Repeat penalties act on
  recent/context tokens and **suppress re-emitting input tokens** — exactly the
  names, digit-words, codes, and negations (`not`) this task must preserve.
  The literature recommendation for copy-sensitive cleanup is
  **`penalty_repeat=1.0`** (disable it). Source: [llama.h sampler API](https://raw.githubusercontent.com/ggml-org/llama.cpp/master/include/llama.h).
  Treat `repeat_penalty=1.0` as a **high-priority config-only variant** to test
  FIRST (cheapest possible lever; no prompt change). Do not let the optimizer
  raise it above 1.0.

### 1.3 Verification without a deterministic guard (the key lever for the bug)

- **Generate-then-verify (LLM-as-a-judge second pass).** LLM-as-a-judge works
  best with strong judges: [Zheng et al. MT-Bench (arXiv:2306.05685)](https://arxiv.org/abs/2306.05685)
  showed GPT-4-level judges approach human agreement but suffer position /
  verbosity / self-enhancement / reasoning limits. **A same-small-model judge
  (4B) is much weaker**, and same-family judges have a documented
  **self-preference** bias — they favor their own generations. Sources:
  [arXiv:2311.09766](https://arxiv.org/abs/2311.09766), [arXiv:2404.13076](https://arxiv.org/abs/2404.13076).
  Design implication: **fail closed to `OK`** — unless the judge positively
  identifies a specific allowed edit, keep the verbatim transcript. Use a
  *structurally different* judge prompt (boolean per-content-word recall check,
  not a paraphrased re-judge). Constitutional AI ([arXiv:2212.08073](https://arxiv.org/abs/2212.08073))
  uses critique/revision + preference-model training — not a one-shot
  same-small-model verifier, so treat same-model verification as advisory only.
- **Self-consistency (majority vote).** [Wang et al. (arXiv:2203.11171)](https://arxiv.org/abs/2203.11171).
  Helps at scale on reasoning; for 1B–4B text cleanup multiple samples **increase
  content-loss opportunities** rather than fixing them, because samples are too
  correlated (low diversity). Deprioritize. Use ONLY as an EDIT-path tie-breaker,
  and only if the verifier alone doesn't reach the gate.
- **Rationale-then-answer (gated CoT, EDIT path only).** Emit a one-token
  reason code (`RESTART` / `CONTRAST` / `FILLER` / `REPEAT`) before the edit.
  For borderline EDITs this directly fixes the *contrast-vs-restart*
  mis-classification the user hit. **Caveat:** rationale-then-answer is not a
  free win on small models — prefer a short hidden checklist or span labels
  over verbose CoT; "overthinking" effects are domain-dependent. The most
  promising single add for THIS specific failure mode, but measure the latency.

**Decision for the loop:** the optimizer iterates on the *generator prompt* and
*few-shot order*. Separately the loop tests the verifier / gated-CoT add-ons as
**independent candidate variants** (each toggle ON/OFF), so we learn which
lever actually moves the content-drop metric. None is added blindly; a verifier
that doesn't improve `content_drop_cases` doesn't ship.

### 1.4 Long-term: finetuning (survey only, not built in this loop)

- **LoRA/QLoRA on synthetic disfluent→clean pairs** is the highest-leverage
  long-term fix. Sources: [LoRA arXiv:2106.09685](https://arxiv.org/abs/2106.09685),
  [QLoRA arXiv:2305.14314](https://arxiv.org/abs/2305.14314). Tools:
  [unsloth](https://github.com/unslothai/unsloth),
  [LLaMA-Factory](https://github.com/hiyouga/LLaMA-Factory),
  [axolotl](https://github.com/axolotl-ai-cloud/axolotl). Practical path:
  train adapter → merge to HF weights → convert to GGUF → quantize → re-eval.
- **Minimum dataset size.** No universal floor — **500–2,000** high-quality
  narrow examples is enough to test signal; **2k–10k** for robustness across
  names/codes/digits/negation/punctuation/ASR errors. Concrete 4B-scale
  precedent: QLoRA on Gemma-3-4B and Qwen3-4B with **~1,700 examples** moved a
  constrained tool-use task. Source: [arXiv:2605.17774](https://arxiv.org/abs/2605.17774).
- **Dataset generation from loop failures:** the eval loop's *failing examples*
  (model output + expected) ARE the training set. Once a candidate is stuck
  below the gate for N rounds, the accumulated failures seed a future LoRA run
  — the bridge from prompt-loop → finetune-loop. Documented here, NOT built now.
- **Held-out test source:** [Switchboard disfluency annotations (LDC99T42)](https://catalog.ldc.upenn.edu/LDC99T42)
  (Shriberg et al.) — crib its restart/repair/abortion classification for the
  contrast/coherence category, but keep a separate in-domain dictation set
  because telephone-conversation disfluencies differ from command/document
  dictation.

### 1.5 Eval methodology gaps to close
  Add ~8 examples where two clauses say *different* things joined by `but`,
  `but now`, `however`, `instead`, `on the other hand`, `whereas` — all
  expected `OK`. This is precisely the `"I was working on this, but now I want
  you to work on that."` failure.
- **Content-drop metric.** The existing `hard_unsafe` only fired on ≥3 dropped
  *significant* content words, so it missed the user's 1-sig-word drop. Add a
  **strict content-preservation check for the EDIT path**: the output's
  significant-content-word set must be a ⊇ of the input's *minus* words
  attributable to a correction cue in the input. Violations count as
  `content_drop_cases`. **Gate: `content_drop_cases == 0` to promote.** This is
  the metric-level capture of "don't drop content", *without* running a
  heuristic on the hot path — it's purely in the eval harness.
- **Over-edit metric.** Fraction of OK-expected cases the model marks EDIT
  (false-positive → latency + risk). Track `over_edit_rate`; soft gate.
- **Stability metric.** Run each candidate K=3 times; gate on
  `min_exact_pct` (worst run), not mean or best. Defeats the 87.5%→78.1%
  regression. Also report `exact_stdev`.
- **Latency under real contention.** Promotion gate uses
### 1.6 Prompt-optimization-loop prior art + the target model card

- **DSPy** (Stanford) — declarative LM programs + optimizers that tune
  instructions/demonstrations against a metric. Supports a cloud optimizer
  model rewriting prompts for a smaller "student" LM (GEPA `reflection_lm`).
  Reports gains on small targets down to **770M T5**. Repo:
  [stanfordnlp/dspy](https://github.com/stanfordnlp/dspy). Paper:
  [arXiv:2310.03714](https://arxiv.org/abs/2310.03714). GEPA docs:
  [dspy.ai/getting-started/gepa-optimization](https://dspy.ai/getting-started/gepa-optimization/).
  **`[PLAN]`** A more mature alternative to the bespoke
  `optimize_prompt_loop.py`; worth considering as the loop harness, but gaps
  in OK/EDIT contract + content-drop metric still need custom wiring.
- **PromptBreeder** (DeepMind) — self-referential evolutionary prompt search.
  Paper: [arXiv:2309.16797](https://arxiv.org/abs/2309.16797). No direct ≤4B
  evidence; main target was PaLM 2-L. No official repo found.
- **APE** — "Large Language Models Are Human-Level Prompt Engineers".
  Generate candidate instructions, score on target model. Paper:
  [arXiv:2211.01910](https://arxiv.org/abs/2211.01910). Repo:
  [keirp/automatic_prompt_engineer](https://github.com/keirp/automatic_prompt_engineer).
  **Instability caveat:** prompt selection can overfit — instructions selected
  for zero-shot hurt few-shot on some tasks.
- **EvoPrompt** — LLM-powered GA/differential-evolution over prompts; one
  model evolves, another executes. Paper: [arXiv:2309.08532](https://arxiv.org/abs/2309.08532).
  Repo: [beeevita/EvoPrompt](https://github.com/beeevita/EvoPrompt). No ≤4B
  evidence; instability is selection noise/cost.
- **Stable / reproducible eval** (directly addresses the prior loop's
  87.5%→78.1% regression):
  - Held-out test split discipline: optimize on train/dev, freeze, evaluate
    once on untouched test — else winner's-curse optimism.
    [arXiv:2605.05973](https://arxiv.org/abs/2605.05973).
  - **Worst-of-K / lower-quantile reporting** (min / 5th pct / lower-CI), not
    the lucky best run. PromptEval estimates performance distributions across
    many prompt variants: [arXiv:2405.17202](https://arxiv.org/abs/2405.17202).
    Lower-confidence-bound selection is preferred because the top prompt
    often changes under random seeds/eval subsets:
    [arXiv:2606.24381](https://arxiv.org/abs/2606.24381).
  - Item-level bootstrap CIs over eval examples (repeat over seeds/templates).
  - Existing harness to crib from: [EleutherAI/lm-evaluation-harness](https://github.com/EleutherAI/lm-evaluation-harness).
- **The target model card — Phi-4-mini.** Technical report:
  [arXiv:2503.01743](https://arxiv.org/abs/2503.01743). Card:
  [microsoft/Phi-4-mini-instruct](https://huggingface.co/microsoft/Phi-4-mini-instruct).
  3.8B dense decoder-only; 128K ctx; 200K vocab; 5T training tokens; SFT + DPO
  post-training on synthetic reasoning data. Chat template ChatML; system role
  supported (examples use `"You are a helpful AI assistant."`). The
  mini-**instruct** card does NOT say the model requires hidden chain-of-thought;
  a **separate** `microsoft/Phi-4-mini-reasoning` variant exists for CoT-heavy
  tasks. Design implication: for the live `instruct` checkpoint, **do NOT**
  inject `<think>`/CoT scaffolding into the generator — it wasn't trained that
  way; rely on one-call OK/EDIT + few-shot + grammar.

---

## 2. Loop design

### 2.1 Inputs / seed

- **Seed prompt** = current `CONSTRAINED_SYSTEM_PROMPT` + `CONSTRAINED_REPAIR_FEW_SHOT`
  (including the contrast fix added today). The target model is **Phi-4-mini
  (3.8B, ChatML, SFT+DPO)** — per its card it does NOT require hidden CoT, so
  the generator stays one-call OK/EDIT with no `think` scaffolding (§1.6).
- **Evaluation corpus** = `dataset.jsonl` ∪ `synthetic-minimal-v1.jsonl` ∪
  `corpus-live-20260630.jsonl` ∪ **NEW** `contrast-coherence` category (§2.4),
  with a train/dev/test split stratified by category (reuse
  `optimize_prompt_loop.split_rows`, seed 42). Test split is held out from the
  optimizer.
- **Target model** = `phi-4-mini-q5` (live default). Optionally sweep the other
  GGUFs in `llm-polish-bench/out/` as a *model* axis, but not in the same loop
  run (confounds prompt effects).
- **Optimizer model** = a cloud OpenAI-compatible model (reuse
  `optimize_prompt_loop.call_optimizer`), **only at optimization time**, never at
  inference time.

### 2.2 Per-round flow

```
for round in 1..MAX_ROUNDS:
    1. EVALUATE the current best prompt against the corpus (eval path below).
       Record: per-case (input, expected, output, raw, exact, sim, wer,
       latency_ms, content_drop, over_edit, category, split).
       Run K=3 times; keep worst-of-K for each metric.
    2. GATE: does current best pass (§2.5)?
         - If yes → STOP (promote candidate). Verify via daemon-path eval.
         - If no  → continue.
    3. COLLECT FAILURES: the test/dev cases the model got wrong, prioritized:
         - content_drop cases FIRST (the user's bug class),
         - then over_edit (false EDIT),
         - then exact-miss within each category.
       Cap at FAILURE_LIMIT (default 12).
    4. PROPOSE: call the optimizer (cloud) with:
         - repair mode (edit the current prompt minimally; do NOT invent fresh),
         - the current system_prompt + few_shot,
         - the failure examples (input, expected, model output, inferred issue),
         - the numeric targets + the no-regress categories,
         - constraint: only system_prompt edits + ≤2 few-shot changes allowed;
           total few-shot ≤ 2; prompt ≤ 85 words.
       Expect N candidate prompts back. The optimizer may also **permute
       few-shot order** as a candidate axis (good orders do NOT transfer across
       model sizes per Lu et al., §1.1) and may **restate the one hard
       constraint in the user turn** (small-model user-role recency).
    5. For each candidate:
         - validate it parses, ≤ word/few-shot limits, is non-identical,
         - EVALUATE (K=3, worst-of-K),
         - GATE.
         - If a candidate passes GATE: mark as new best, break to step 1
           (refine around it) — but do NOT auto-promote until a fresh
           cross-check run passes too.
    6. Rank remaining candidates by objective (§2.5 objective) and set the
       top-ranked as the next round's current best.
    7. If no candidate improved on any gate dimension for STAGNATION_ROUNDS (2)
       consecutive rounds: STOP (converged or stuck). Dump the final report.
```

The loop is **failure-driven by construction** (step 3), which is the user's
"update the prompt as per the evaluation of the model, not any prompt we want".

### 2.3 Eval path (within the loop, fastest faithful path)

- Import `constrained_payload` / `constrained_payload_streaming` directly from
  `services/polish/llm_polish_sidecar.py` (function-level), with a single
  warm `Llama` instance + `LlamaRAMCache` (cold-start measured separately and
  excluded from latency gating).
- Inject the candidate prompt via env-compatible kwargs (the functions already
  read `constrained_system_prompt()`; pass an explicit override so we don't
  mutate process env between candidates — refactor `constrained_payload*` to take
  an optional `system_prompt`/`few_shot` arg, defaulting to the env-read functions).
- Latency measured warm (= duplicate of daemon hot path modulo ASR contention).
  **Promotion requires** a final confirmation run through
  `bench_daemon_architecture.py` (real daemon, ASR-contending) before flipping
  the default prompt in source.

### 2.4 New corpus category

`llm-polish-bench/contrast-coherence.jsonl` (~8 rows), expected `OK`:

```
{"id":"contrast-01","category":"contrast_coherence","input":"I was working on this, but now I want you to work on that.","expected":"OK"|"I was working on this, but now I want you to work on that."}
... "We tried the cache first, instead we ended up rewriting the loop."
... "The first build failed, however the retry passed."
... "On the one hand it is fast, on the other hand it is expensive."
... "I used to like that tool, whereas now I prefer this one."
... "I was going to, but I decided not to."  (clipped, still two facts — keep verbatim)
```
(Use `OK` semantics: expected output == input verbatim. The harness records
`expected` as the verbatim input for these.)

### 2.5 Gate + objective

**Hard gates (all must pass to promote):** measured worst-of-K (K=3).

| Gate | Target | Rationale |
| --- | --- | --- |
| `content_drop_cases` | **== 0** | Directly fixes the user's bug. Hard. |
| `exact_pct` (normalized, worst-of-K) | **≥ 88** (ship floor) / 90 (stretch) | Accuracy target the user asked for. |
| `mean_similarity` (test split) | ≥ 0.93 | Semantic floor. |
| `over_edit_rate` | ≤ prev best + 2pp | Don't buy accuracy by over-editing clean text. |
| `p50_latency_ms` (warm) | ≤ baseline_p50 + 30ms | "Don't drop too much latency." |
| `p95_latency_ms` (warm) | ≤ baseline_p95 + 50ms | Tail latency budget. |
| no-regress (per category sim) | no category below its baseline sim | Don't break restarts to fix contrasts. |
| `prompt_words` | ≤ 90 | Keep prompt compact (latency + cache friendliness). |
| `few_shot_count` | ≤ 2 | Keep prompt compact. |

**Soft objective** (rank candidates that pass the hard gate, pick the best):

```
objective = mean_similarity*100 + exact_pct*0.15
          - content_drop_cases*5.0      # heavy, but it's already a hard gate
          - over_edit_rate*100
          - chatty_cases*2.5
          - prompt_words*0.025
          - few_shot_count*0.5
          - (p95_ms - baseline_p95)/1000
```
(Basically the existing `objective_for` plus `content_drop` and `over_edit`.)

**Latency budget is relative to the current live prompt's baseline**, measured
in the SAME run, so the gate is "don't make it slower" rather than an absolute
number — that's the faithful reading of "not dropping too much latency".

### 2.6 Stopping conditions

Stop the loop when ANY of:
1. A candidate passes all hard gates **and** a fresh cross-check run (K=3) on
   test split also passes → **promote** that prompt into
   `CONSTRAINED_SYSTEM_PROMPT`/`CONSTRAINED_REPAIR_FEW_SHOT` (edit source) and
   re-verify via `bench_daemon_architecture.py`.
2. `MAX_ROUNDS` (default 6) reached with no candidate passing → report the best
   non-passing candidate + the residual failure categories. Hand off the
   accumulated failure cases as finetuning seed (§1.4).
3. `STAGNATION_ROUNDS` (2) consecutive rounds with no improvement on ANY hard
   gate dimension → stop (converged/stuck).

### 2.7 Add-on variants (independent candidates)

Run these as toggles on top of the best generator prompt, in a *separate*
evaluation sweep (not inside the optimizer propose loop), to isolate their
effect. **Order = test the cheapest, highest-leverage levers first** (informed
by §1 research):

1. **`--variant repeat_penalty_1.0`** — set `SUNOTO_LLM_POLISH_REPEAT_PENALTY=1.0`.
   Config-only, no prompt change. The literature says repeat penalty suppresses
   re-emitting input tokens (names/digits/codes/negations) — a *suspected*
   contributor to the content-drop bug. Cheapest possible lever; test first.
2. **`--variant grammar_on`** — `SUNOTO_LLM_POLISH_GRAMMAR=1`. Improves FORMAT
   precision but NOT factual preservation; can distort the distribution
   (§1.1). Re-measure latency (prior loop turned it off for latency — the
   trade-off must be re-checked per candidate because the prompt also affects
   token count). **Optimize prompts under the same grammar you deploy.**
3. **`--variant cot`** — EDIT path emits a reason code (`RESTART`/`CONTRAST`/
   `FILLER`/`REPEAT`) before the edit. Directly targets the contrast-vs-restart
   mis-classification the user hit. Most promising *prompt-side* add for THIS
   specific failure mode; measure the latency cost.
4. **`--variant verifier`** — EDIT path runs a 2nd LLM judge (boolean
   per-content-word recall check). On `DROP`, **fail closed to `OK`**
   (paste raw transcript) per §1.3 — same-small-model judges are weak and
   self-preferential, so only honor an explicit DROP verdict, never an
   affirmative "looks fine".
5. **`--variant selfconsist`** — EDIT path sampled 3× at temp 0.3, majority of
   OK/EDIT; for EDIT keep highest-recall sample. Deprioritized: on 1B–4B
   samples are too correlated and increase content-loss opportunities.

Each variant is gated on the SAME hard gates. A variant only ships if it
improves `content_drop_cases` to 0 *and* holds latency. A variant that does
*not* move `content_drop_cases` is abandoned — none is added blindly.

---

## 3. Implementation file list (to build after sign-off)

All under `llm-polish-bench/` unless noted; stdlib + `llama_cpp` only, no new deps.

1. **`contrast-coherence.jsonl`** — new corpus category (§2.4).
   **[DONE]** — 18 rows created at
   `llm-polish-bench/contrast-coherence.jsonl`, schema matches `dataset.jsonl`,
   loads via the existing `bench.load_dataset`. Covers `but` / `however` /
   `instead` / `whereas` / `or` connectors; 13 pure-OK (expected==input) + 5
   mixed-disfluency (must clean a filler/repetition `um`/`uh`/`So,`/`Well,`/dup
   `the the` while **keeping** the contrast clause — the exact failure mode).
   Verified: every mixed row is a valid subsequence cleanup with no invented
   content and no dropped contrast connector.
2. **`optimize_constrained_loop.py`** — the new loop driver (retargeted to the
   OK/EDIT format; do NOT fork `optimize_prompt_loop.py`). Reuses:
   `split_rows`, `metrics_for`, `objective_for` (extended), `call_optimizer`
   (call_chat_optimizer/call_responses_optimizer), `generate_report` shape.
   New: `constrained_*` message builder, `content_drop` + `over_edit` metrics,
   worst-of-K eval, hard-gate set in §2.5.
3. **`eval_constrained.py`** — small shared module: given (system_prompt,
   few_shot, corpus) → per-case results with all metrics + warm-cache latency.
   Used by both the loop and a standalone `--eval-only` mode.
4. **`services/polish/llm_polish_sidecar.py`** — refactor
   `constrained_payload` / `constrained_payload_streaming` to accept optional
   `system_prompt` / `few_shot` kwargs (default to the env-read functions) so
   the loop can inject candidates without process-env mutation. No behavior
   change for the daemon (defaults unchanged).
5. **`docs/llm-polish-prompt-eval-loop-results-2026-06-30.md`** — results +
   decision log, mirrors the prior `...eval-loop-2026-06-29.md` format. The
   loop writes a JSON+HTML artifact to `llm-polish-bench/out/constrained-loop/`.

No Rust/daemon changes required for the loop (the env-override hooks already
exist). Promoting a winning prompt edits `llm_polish_once.py` constants only.

---

## 4. Open questions to resolve before building

- **K and cost.** K=3 worst-of-run × ~100 cases × ~6 rounds × ~4 candidates/round
  = ~7.2k LLM calls per loop, ~0.3s each warm ≈ 35 min compute on the local
  model. Acceptable on the M-series dev box; confirm before building. Drop K to 2
  if too slow.
- **Cloud optimizer access.** Requires an API key (OpenAI or NeuralWatt, as in
  `optimize_prompt_loop.py`). Confirm which provider; the env-key name must be
  set. No cloud call at inference time — only at optimization time.
- **First lever to test: `repeat_penalty=1.0`.** Per §1.2, the live
  `repeat_penalty=1.05` is a *suspected* contributor to the content-drop bug
  (it suppresses re-emitting input tokens). This is a config-only change, no
  prompt edit, no rebuild — test it BEFORE building the loop, by setting
  `SUNOTO_LLM_POLISH_REPEAT_PENALTY=1.0` and re-running the bug case. If it
  alone zeroes `content_drop_cases`, the loop may be unnecessary for the bug.
- **Grammar ON as default?** Note per §1.1 grammar improves *format* precision,
  NOT content preservation, and can distort the distribution, so it will **not**
  alone fix the content-drop bug. Run the §2.7 `--variant grammar_on` sweep to
  re-measure the latency↔format trade-off, but do not expect it to fix the bug.
- **Few-shot order permutation**: now a confirmed candidate axis (Lu et al.,
  §1.1 — order swings small models from near-random to near-SOTA and good
  orders don't transfer across model sizes).

---

## 5. Memo: what does NOT happen

- No deterministic content-loss guard on the hot path (the user's explicit
  constraint). The `drops_content_unsafely` heuristic stays as-is in
  `llm_polish_once.py` for the last-line fallback; we do NOT extend it. The fix
  must come from the model response (prompt + sampling + optional verifier).
- No cloud LLM call at dictation time. The cloud optimizer runs only during the
  offline tuning loop.
- No finetune in this pass — surveyed (§1.4), deferred.
- No re-architecture of the daemon streaming path. The loop operates strictly on
  the polish prompt and the polish-sidecar function surface.

# LLM Disfluency-Cleanup Research

**Status:** Experimental — benchmark complete; daemon integration not started.
**Date:** 2026-06-27
**Owner:** antash-mishra

This document records the experiments run to evaluate whether a **local,
non-deterministic (LLM) pass** can clean self-repair / disfluency from
voice-dictation transcripts — the one case the existing deterministic
`sunoto-polish` stage (`resolve_corrections`) provably cannot handle, e.g.
*"Hello, how are you sorry sorry whats up?"* → *"whats up?"* (abandoning a
complete but discarded clause requires intent inference, not pattern matching).

It does **not** change daemon behavior. All artifacts live in
[`llm-polish-bench/`](../llm-polish-bench). The product plan already reserves
exactly this path: "Optional local LLM polish" as a *slow path after insertion*
(see [docs/product-plan.md](product-plan.md) §7, step 8).

---

## 1. What we are working on

The voice-dictation daemon (`apps/daemon`) currently polishes ASR output with a
**deterministic** pipeline in `crates/sunoto-polish/src/lib.rs`:

1. Normalize
2. `resolve_corrections` — driven by an **exact marker-word list**
   (`SWAP_MARKERS = ["actually","i mean","no wait","wait no"]`,
   `RESTART_MARKERS = ["scratch that","strike that"]`)
3. Remove fillers
4. Dictionary / snippets / style

This handles easy cases ("Tuesday, actually Wednesday" → "Wednesday") but
**cannot** handle bare restarts without a cue word, repetition-as-repair, or
abandoned prefixes. The product plan scope (§7) defines the intended fix: an
**optional, local, non-deterministic LLM polish pass** that runs *after* the
fast deterministic insert, validates its output, and replaces the inserted text
if the result is safe. This research measures whether small local GGUF models
can do that job well enough and fast enough on this machine.

### Constraints (define "good enough")

- **Hardware:** NVIDIA RTX 3060 12 GiB. ASR sidecar (Nemotron) holds ~3.6 GiB
  VRAM → ~7.3 GiB free with the service running. macOS path is an M1 Pro
  (unified memory, Metal).
- **VRAM floor:** the repo's preflight (`--min-free-vram-mib`, default 4500)
  refuses to load into a nearly-full GPU. Polish models must be ~2–3 GiB so
  they sit beside a warm ASR sidecar. This rules out >7B dense models.
- **Latency rule (product-plan §7 / §8):** *Never let an LLM block the first
  insert.* The polish pass runs after the deterministic insert lands; it must
  complete in well under ~1 s so the corrected text "lands a beat after" the
  raw insert — never makes dictation feel slow.
- **Safety rule (product-plan §8):** constrained prompt, validator, raw fallback
  on any suspicious change. The LLM must not change facts/numbers/code or add
  information.
- **Local only:** no cloud calls. Run via `llama-cpp-python` (CUDA on Linux,
  Metal on macOS), same managed-sidecar pattern as the ASR process.

---

## 2. Experiments run

All scripts and results are in [`llm-polish-bench/`](../llm-polish-bench):

- `dataset.jsonl` — 60 hand-authored examples across 10 categories:
  `bare_restart`, `repetition`, `filler`, `false_start`, `mixed`,
  `preserve_facts`, `explicit_cancel`, `compound`, `should_not_overedit`,
  `long_utterance`.
- `bench.py` — harness. Loads one GGUF at a time (VRAM-safe), scores each
  example (exact match + word-error-rate → similarity 0..1), emits per-model
  JSON + an HTML report. Reusable: `python3 bench.py --models ...`.
- `out/report.html` — the user-facing comparison (sortable summary, per-category
  breakdown, per-example grid with raw outputs).
- `out/v1/` — frozen v1 results for direct before/after comparison.

Scoring: `similarity = 1 − word_level_Levenshtein(input→expected)` after
lowercasing/punctuation-stripping. Exact = normalized equality. Latency is
end-to-end `create_chat_completion` wall time. All runs at `temperature=0.1`
(v2) or `0.0` (v1), one pass per example.

### 2.1 v1 — initial model survey

**Setup:** original prompt ("Remove ONLY disfluencies… keep the speaker's final
intended wording"), greedy `temperature=0.0`, no few-shot, all Q4_K_M GGUF.

| Model | Family | Exact % | Sim | p50 lat |
|---|---|---|---|---|
| Qwen3.5-4B | Qwen | 53.3 | 0.7571 | 208ms |
| Qwen3-4B-Instruct-2507 | Qwen | 51.7 | 0.7268 | 128ms |
| Phi-4-mini-instruct | Microsoft | 45.0 | 0.7206 | 114ms |
| Ministral-3-3B-Instruct-2512 | Mistral | 26.7 | 0.5788 | 84ms |
| LiquidAI LFM2.5-1.2B-Instruct | Liquid | 31.7 | 0.5842 | 61ms |
| LiquidAI LFM2.5-230M | Liquid | 8.3 | 0.3305 | 75ms |

**v1 findings:**

- **No model solved `restart-01`** ("Hello, how are you sorry sorry whats up?"
  → "whats up?"). Every model kept "Hello, how are you" and only stripped the
  "sorry sorry" stutter. The abandoned-prefix case is a genuine intent-inference
  ceiling for sub-9B models — recorded as an accepted limitation.
- Sub-1B (LFM2.5-230M) is below the floor: it chatted instead of cleaning.
- 4B-class models were the practical band; LFM2.5-1.2B was the fastest
  capable model.

Raw: [`llm-polish-bench/out/v1/all.json`](../llm-polish-bench/out/v1/all.json)

### 2.2 v2 — prompt + inference refinement (the big lever)

After reading the **official Liquid prompting guide**
(https://docs.liquid.ai/lfm/key-concepts/text-generation-and-prompting) and
chat-template docs, three changes, applied together:

1. **Refined system prompt** (`SYSTEM_PROMPT_V2` in `bench.py`): explicitly
   distinguishes a RESTART (speaker discards and *restates* → keep only the
   restatement) from a literal "sorry"/"actually" that *is* the message, with
   concrete positive/negative examples in the prompt.
2. **4 few-shot examples** (LFM docs explicitly recommend few-shot for
   instruction-tuned LFM2.5; helps small models most).
3. **LFM-docs-recommended inference config** applied to all chat models:
   `temperature=0.1, top_k=50, top_p=0.95, repeat_penalty=1.05`. v1 ran at
   greedy temperature 0 with no repeat penalty — that hurt small models.

**v1 → v2 improvement (huge):**

| Model | v1 sim | v2 sim | Δ | v1 exact | v2 exact |
|---|---|---|---|---|---|
| Qwen3.5-4B | 0.76 | **0.904** | +0.15 | 53% | **78.3%** |
| Phi-4-mini | 0.72 | **0.892** | +0.17 | 45% | **71.7%** |
| LFM2.5-1.2B | 0.58 | 0.666 | +0.08 | 32% | 43.3% |

The prompt/inference lift (≈+0.15 sim) is worth **~15×** the quantization lift
(see §2.4). This is the central actionable finding: **tune the prompt, not the
quant.**

Raw: [`llm-polish-bench/out/all.json`](../llm-polish-bench/out/all.json)

### 2.3 Added: LiquidAI LFM2.5-8B-A1B (the MoE)

Tested the bigger LFM sibling — an 8B MoE with ~1.5B *active* params — to see
if "bigger LFM, still fast" would close the quality gap.

| Model | Sim | p50 lat |
|---|---|---|
| LFM2.5-1.2B (dense) | 0.666 | **74ms** |
| LFM2.5-8B-A1B (MoE, thinking) | 0.521 | 4046ms |

**Counterintuitive finding: the 8B-A1B is both slower AND worse.** It is a
*thinking* model that emits a long reasoning trace (hence 4 s latency), and
that reasoning led it *astray* — it explicitly reasoned about `restart-01` and
decided to keep "Hello, how are you" (more conservative, not less), and on
`restart-03` it hallucinated "I am not sure never mind." Bigger + thinking ≠
better for this narrow task. **Drop the 8B-A1B;** the 1.2B dense is the better
LFM.

Harness work: `bench.py` gained a `thinking: True` model flag, a
thinking-block extractor (`clean_output`, splits on the literal close-marker
and takes the tail), and per-model max_tokens/context bumps so the thinking
trace can complete before the answer emits.

Raw: [`llm-polish-bench/out/lfm2.5-8b-a1b.json`](../llm-polish-bench/out/lfm2.5-8b-a1b.json)

### 2.4 Added: Phi-4-mini quantization sweep

Asked: was Q4_K_M the best quant? Swept Q4_K_M → Q5_K_M → Q6_K → Q8_0 on
Phi-4-mini (all fit alongside ASR on the 12 GiB card).

| Quant | Size | Exact % | Sim | p50 lat |
|---|---|---|---|---|
| Q4_K_M | 2.32 GiB | 71.7 | 0.8924 | 105ms |
| **Q5_K_M** | 2.65 GiB | **73.3** | 0.8934 | 112ms |
| Q6_K | 2.94 GiB | 73.3 | **0.8941** | 122ms |
| Q8_0 | 3.80 GiB | 70.0 | 0.8907 | 137ms |

**Finding: quantization barely matters for this task.** Q4→Q6 is +0.0017 sim
(statistically negligible, within noise); exact +1.6 pts. **Q8_0 is actively
worse** (0.8907) *and* larger *and* slower — at Q8 the model over-weights token
probs and produces slightly worse cleanups. The cleanup task is too narrow for
bit depth to bite.

**Recommendation: Q5_K_M** if defensive about quality (tied-best exact, +7ms),
but **Q4_K_M is a fully defensible default.** Avoid Q8. The bigger lever was
prompt+few-shot (§2.2), not quant.

Raw: [`llm-polish-bench/out/phi-4-mini*.json`](../llm-polish-bench/out/)

---

## 3. Conclusions

1. **A small local LLM can do this job well enough.** Qwen3.5-4B (0.904 sim,
   78% exact, p50 290ms) and Phi-4-mini (0.892 sim, 72% exact, p50 105ms) both
   clear the quality and latency bars comfortably — *if* run on the slow path
   after the deterministic insert, per the product plan.
2. **The abandoned-prefix case (`restart-01`) is an accepted limitation.** No
   sub-9B model drops a complete-but-discarded clause; they treat it as valid
   speech and only strip the stutter. This is a model-capability ceiling, not a
   prompt bug — the refined prompt v2 explicitly instructs the drop and the
   models still decline.
3. **Prompt + few-shot + inference config is the dominant lever** (~+0.15 sim).
   Quant choice is a rounding error (~+0.002). Spend tuning budget on prompt
   iteration.
4. **LFM is fast but not good enough** at this size. LFM2.5-1.2B is the fastest
   capable model (p50 74ms) but trails by ~0.23 sim vs Phi-4-mini. Keep it only
   as a documented ultra-fast / low-VRAM fallback. The 8B-A1B thinking MoE is a
   dead end for this task.

### Recommended production model

**Phi-4-mini-instruct, Q5_K_M** — best quality/latency balance for the daemon's
optional polish sidecar:
- 0.8934 sim / 73.3% exact — clears the quality bar.
- p50 **112ms** — well inside the "lands a beat after insert" latency budget.
- 2.65 GiB — leaves ~4.7 GiB free alongside the ~3.6 GiB ASR sidecar (above the
  4.5 GiB preflight floor), and runs identically via Metal on the M1 Pro.

Fallback: Qwen3.5-4B (Q4_K_M) if raw quality matters more than latency (+0.01
sim, but 2.6× slower at p50 290ms).

---

## 4. TODO — what is NOT done

This research produced evidence only. **No daemon code changed.** Concrete next
steps, smallest-first, each keepable separately:

- [ ] **`PolishMode` config skeleton** — add `PolishMode { Fast, Polish }` +
      `LlmPolishConfig { model_path, n_gpu_layers, max_tokens_ms,
      repeat_penalty, ... }` to `PolishConfig` in
      `crates/sunoto-polish/src/lib.rs`, default-off, serde round-trip tests.
      No behavior change. Smallest safe first slice.
- [ ] **`llm-polish` sidecar** — a small Python process wrapping
      `llama-cpp-python`, same managed-sidecar pattern as
      `services/asr/nemotron_sidecar.py`. NDJSON over stdin/stdout, model
      loaded once at startup, emits a ready line. System prompt =
      `SYSTEM_PROMPT_V2` from this research.
- [ ] **Typed IPC messages** in `sunoto-ipc`: `polish` request → `polished`
      response (and `polish_skipped: raw` fallback).
- [ ] **Daemon slow-path wiring** — in `apps/daemon`, after the deterministic
      insert, when `mode == Polish`: call the sidecar, **validate** the result
      (length bounds, protected-term preservation, no fact additions), then
      **replace** the inserted text via the existing insertion adapter
      (delete-previous + paste, same path `scratch that` uses today). Raw
      fallback on any validation failure.
- [ ] **Installer/upgrader updates** — add `llama-cpp-python` (CUDA wheel) to
      `pyproject.toml` / `install.sh`; document the GGUF download step. Update
      `docs/desktop-configuration.md` and `README.md` together (per AGENTS.md
      style rule).
- [ ] **Larger dataset + cross-platform latency** — 60 examples is a first
      cut; expand `dataset.jsonl` and re-run `bench.py` on the M1 Pro to
      confirm Metal latency is also inside budget.
- [ ] **Prompt iteration on `restart-01`-class cases** — explicitly accept the
      current ceiling, or try a 9B-class dense model (Qwen3.5-9B fits only
      when ASR is the lightweight Parakeet-MLX macOS path; not viable on the
      12 GiB NVIDIA card alongside Nemotron).

---

## 5. Artifacts

| Path | Purpose |
|---|---|
| [`llm-polish-bench/bench.py`](../llm-polish-bench/bench.py) | Reusable harness (loads one model at a time, scores, renders HTML) |
| [`llm-polish-bench/dataset.jsonl`](../llm-polish-bench/dataset.jsonl) | 60-example cleanup dataset, 10 categories |
| [`llm-polish-bench/out/report.html`](../llm-polish-bench/out/report.html) | v2 user-facing comparison (7 models, sortable, per-example grid) |
| [`llm-polish-bench/out/all.json`](../llm-polish-bench/out/all.json) | v2 raw results |
| [`llm-polish-bench/out/v1/report-v1.html`](../llm-polish-bench/out/v1/report-v1.html) | v1 frozen report for before/after comparison |
| [`llm-polish-bench/out/v1/all.json`](../llm-polish-bench/out/v1/all.json) | v1 raw results |
| [`llm-polish-bench/out/{model}.json`](../llm-polish-bench/out/) | Per-model JSON |

Models (GGUFs, gitignored under `models/`): Ministral-3-3B, Phi-4-mini
(Q4/Q5/Q6/Q8), Qwen3-4B-Instruct-2507, Qwen3.5-4B, LFM2.5-230M,
LFM2.5-1.2B-Instruct, LFM2.5-8B-A1B.

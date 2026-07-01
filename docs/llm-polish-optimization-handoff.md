# LLM Polish Optimization Handoff

## Current State

We are optimizing the optional local LLM polish step for voice dictation. The
runtime target remains `gemma-4-e2b-it-q4` because it is fast enough on Mac
with llama.cpp/Metal. The optimizer loop now uses OpenAI `gpt-5.5` through the
Responses API to propose prompts, then evaluates each prompt locally with
Gemma on the 60-example dataset.

Latest run:

- Optimizer: `gpt-5.5`
- Target model: `gemma-4-e2b-it-q4`
- Run shape: 3 rounds, 5 candidates per round
- Total prompt results: 16
- Dashboard: `llm-polish-bench/out/prompt-optimizer/report.html`
- Full JSON: `llm-polish-bench/out/prompt-optimizer/all.json`

Current gated best is still the baseline prompt:

| Prompt | Gates | Exact | Similarity | Unsafe | p50 | p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `baseline-compact-balanced-two-shot` | 8/10 | 70.0% | 0.8657 | 8 | 175ms | 488ms |
| `r3-numeric-prefix-safe` | 7/10 | 80.0% | 0.9261 | 8 | 204ms | 424ms |

`r3-numeric-prefix-safe` is the best repair base, not the deployable winner.
It clears the similarity target and improves many false-start/correction cases,
but it fails exact match and regresses `preserve_facts`.

## What Has Been Tried

Model bake-off:

- Qwen3.5-4B had the best quality but was too slow for live dictation.
- Gemma 4 E2B was the best speed/quality target for Mac.
- LFM2 did not beat Gemma enough to justify switching.

Prompt work:

- Manual prompt sweep found `compact_balanced_two_shot` as the best practical
  baseline.
- Router/classifier experiments did not reduce latency reliably; the router
  either rewrote almost everything or skipped needed cleanup.
- Kimi K2.6 via NeuralWatt was tested. Full `kimi-k2.6` spent output budget in
  reasoning for optimizer-shaped prompts; `kimi-k2.6-fast` worked but did not
  outperform GPT-5.5 as the optimizer.
- GPT-5.5 prompt search found `r3-numeric-prefix-safe`, which improved overall
  similarity but damaged preservation cases.

## Important Failure Modes

The key failure is not latency now; it is correctness on preservation cases.

`r3-numeric-prefix-safe` category deltas versus baseline:

| Category | Baseline Sim | Candidate Sim | Notes |
| --- | ---: | ---: | --- |
| `false_start` | 0.4800 | 1.0000 | Big improvement |
| `explicit_cancel` | 0.7500 | 1.0000 | Big improvement |
| `mixed` | 0.6500 | 0.8100 | Improvement |
| `should_not_overedit` | 0.9153 | 0.9357 | Slight improvement |
| `preserve_facts` | 0.9375 | 0.8083 | Bad regression |

Representative bad outputs from `r3-numeric-prefix-safe`:

- Email formatting:
  - Input: `Her email is jane, no, janet dot smith at example dot com.`
  - Expected: `Her email is janet dot smith at example dot com.`
  - Output: `Her email is jane.smith at example.com.`

- IP formatting:
  - Input: `The IP address is one nine two, uh, one nine two dot one six eight dot one dot one.`
  - Expected: `The IP address is one nine two dot one six eight dot one dot one.`
  - Output: `The IP address is 192.168.1.1.`

- Numeric prefix:
  - Input: `My account number is four seven two, um, four seven two nine three one.`
  - Expected: `My account number is four seven two nine three one.`
  - Output: `My account number is four seven two, four seven two nine three one.`

## Next Experiment

Do not run more broad prompt-generation rounds yet. The next run should be a
targeted repair pass starting from `r3-numeric-prefix-safe`.

Implementation target:

1. Add a repair mode to `llm-polish-bench/optimize_prompt_loop.py`.
   - Suggested CLI: `--repair-prompt-id r3-numeric-prefix-safe`
   - In repair mode, GPT-5.5 must minimally edit the selected prompt instead of
     inventing fresh prompts.
   - The optimizer prompt should explicitly say to preserve the candidate's
     strengths: false starts, explicit cancel, mixed cleanup, and no broad style
     rewriting.

2. Improve validator output in `llm-polish-bench/bench_two_pass.py`.
   - Split current `unsafe` into:
     - `hard_unsafe`: real content corruption
     - `review_flags`: conservative signals that may be valid corrections
   - Keep hard unsafe for `empty_output`, `digit_compaction`,
     `formatted_target`, `code_formatting`, and true negation loss.
   - Do not hard-fail valid correction drops such as
     `fifty, no, fifty-three`.

3. Use exactly two repair examples.
   - Numeric-prefix preservation:
     - Input: `My account number is four seven two, um, four seven two nine three one.`
     - Output: `My account number is four seven two nine three one.`
   - Email correction without formatting:
     - Input: `Her email is jane, no, janet dot smith at example dot com.`
     - Output: `Her email is janet dot smith at example dot com.`

4. Run the repair experiment.
   - 3 repair rounds
   - 6 candidates per round
   - Optimizer: `gpt-5.5`
   - Target: `gemma-4-e2b-it-q4`
   - Keep `--flash-attn`
   - Stop early if a prompt passes all gates.

Target gates:

- Exact `>= 83.0%`
- Similarity `>= 0.925`
- Hard unsafe `<= 5`
- p50 `<= 220ms`
- p95 `<= 500ms`
- Prompt length `<= 85` words
- Few-shot examples `<= 2`
- No regression on `preserve_facts`
- No regression on `should_not_overedit`

## Commands

Load the local OpenAI key without printing it:

```bash
set -a
source llm-polish-bench/.env
set +a
```

Current broad optimizer command:

```bash
.venv-llm-polish-mac/bin/python llm-polish-bench/optimize_prompt_loop.py \
  --optimizer-provider openai \
  --optimizer-model gpt-5.5 \
  --rounds 3 \
  --candidates-per-round 5 \
  --flash-attn
```

After repair mode is implemented, use this shape:

```bash
.venv-llm-polish-mac/bin/python llm-polish-bench/optimize_prompt_loop.py \
  --optimizer-provider openai \
  --optimizer-model gpt-5.5 \
  --repair-prompt-id r3-numeric-prefix-safe \
  --rounds 3 \
  --candidates-per-round 6 \
  --flash-attn
```

Open the UI:

```bash
open llm-polish-bench/out/prompt-optimizer/report.html
```

## Reference Files

- `docs/llm-polish-research.md` — original model and prompt research.
- `llm-polish-bench/dataset.jsonl` — 60-example benchmark dataset.
- `llm-polish-bench/optimize_prompt_loop.py` — optimizer loop and UI generator.
- `llm-polish-bench/bench_two_pass.py` — current validator helpers.
- `llm-polish-bench/out/prompt-optimizer/all.json` — latest GPT-5.5 run.
- `llm-polish-bench/out/prompt-optimizer/report.html` — latest optimizer UI.
- `llm-polish-bench/out/prompt-optimizer/r3-numeric-prefix-safe.json` — prompt to repair.
- `llm-polish-bench/.env` — local `OPENAI_API_KEY` source. Do not copy or print contents.

## Safety Notes

- Do not put API keys into repo files, reports, logs, or final answers.
- `llm-polish-bench/.env` is locally excluded through `.git/info/exclude`.
- The word `unsafe` in current reports means dictation-correctness risk, not
  content-safety risk.
- Current unsafe count is conservative; some `negation_dropped` flags are valid
  corrections. That is why validator refinement is the next required step.


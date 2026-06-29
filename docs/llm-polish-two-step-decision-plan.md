# LLM Polish Two-Step Decision Plan

## Status

Planning only. Do not implement this plan until the user explicitly approves.

## Goal

Reduce realistic post-ASR LLM polish latency while preserving the current product
rule:

```text
audio -> ASR final transcript -> LLM polish -> insertion
```

Every non-empty final transcript should still pass through an LLM step. The
change is to make clean/no-op transcripts cheap by asking the LLM for a tiny
decision first.

## Current Observation

The daemon-architecture benchmark uses the live daemon path:

```text
benchmark -> daemon control socket -> deterministic polish -> warm LLM sidecar
```

Current tuned one-pass minimal mode on `llm-polish-bench/synthetic-minimal-v1.jsonl`:

- Final exact: `84.4%`
- Overall mean WER: `6.83%`
- Aggregate edit/word ratio: `6.96%`
- Minimal contract adherence: `84.4%`
- Clean/no-op contract adherence: `80.0%`
- LLM p50: `297ms`
- LLM p95: `598ms`
- Daemon polish path p50: `332ms`
- Daemon polish path p95: `627ms`
- Completion tokens p50: `3`
- Completion tokens p95: `20`

The main latency issue is that clean text sometimes gets:

```text
EDITED: <the same full sentence>
```

instead of:

```text
UNCHANGED
```

That forces the model to generate many unnecessary tokens.

## Proposed Architecture

Replace the current one-pass minimal prompt with an experimental two-step LLM
flow:

```text
deterministic polish output
  -> LLM decision call
      -> UNCHANGED: return deterministic output immediately
      -> EDIT: run LLM rewrite call
  -> validation
  -> insertion
```

The decision call must be an LLM call, so the "every non-empty transcript goes
through LLM" rule remains true.

## Decision Call

Purpose: decide if the transcript needs a rewrite.

Expected output:

```text
UNCHANGED
```

or:

```text
EDIT
```

Prompt requirements:

- Very short system prompt.
- Few-shot examples should include clean longer sentences, facts, numbers,
  emails, code-like text, and true disfluency examples.
- Output must be constrained to one token/label.
- Use a small `max_tokens`, likely `3` or `4`.
- Temperature should stay low.

Decision call response handling:

- Exact `UNCHANGED` -> accept original deterministic output.
- Exact `EDIT` -> run rewrite call.
- Any malformed answer -> conservative fallback to rewrite call.

## Rewrite Call

Purpose: produce the cleaned transcript only when the decision call says `EDIT`.

Options:

1. Reuse current minimal output mode and accept either:
   - `EDITED: <cleaned transcript>`
   - direct cleaned transcript as fallback
2. Switch rewrite-only prompt back to "output only cleaned transcript" because
   the decision step already handled no-op routing.

Recommended first implementation:

- Use rewrite-only prompt: output only cleaned transcript.
- Keep current validation.
- Preserve current hard safety behavior: if validation rejects the rewrite,
  return deterministic output.

## Expected Latency Shape

Clean/no-op transcripts:

```text
decision call only
```

Target:

- LLM p50 around `150-250ms`
- Completion tokens about `1-3`

Edit transcripts:

```text
decision call + rewrite call
```

Expected tradeoff:

- Edit cases may be slower than one-pass minimal mode.
- Overall user-perceived latency may still improve if most dictation is clean
  or lightly changed by deterministic polish.

## Metrics To Compare

Run the same daemon-architecture benchmark for both modes:

1. Current one-pass minimal mode.
2. Experimental two-step decision mode.

Compare:

- Final exact percentage.
- Mean WER.
- Aggregate edit/word ratio.
- Decision accuracy:
  - clean/no-op precision
  - edit recall
- LLM p50 / p95 / max.
- Daemon total p50 / p95 / max.
- Completion token p50 / p95.
- Number of rewrite calls.
- Number of validation rejections.
- Contract failures or malformed decision outputs.

## Success Criteria

Minimum bar for accepting two-step mode:

- Final exact must not drop materially from `84.4%` on the current seed set.
- Mean WER should stay near or below `6.83%`.
- Clean/no-op decision adherence should be at least `95%`.
- LLM p50 should improve below `250ms`.
- LLM p95 should improve below current `598ms`.
- No new hard safety failures for numbers, emails, URLs, code, or negation.

If edit-case latency becomes much worse, keep two-step mode behind a config flag
until the dataset shows the real clean/edit mix.

## Implementation Plan

Do not start these steps without user approval.

1. Add a sidecar output mode flag, for example:

   ```text
   SUNOTO_LLM_POLISH_MODE=one_pass_minimal|two_step
   ```

2. Add decision prompt/helpers in `services/polish/llm_polish_once.py`.

3. Update `services/polish/llm_polish_sidecar.py`:

   - Run `decision_payload()` first in two-step mode.
   - If decision is `UNCHANGED`, return original text with diagnostics.
   - If decision is `EDIT`, run rewrite completion.
   - Return diagnostics for both calls:
     - decision latency/tokens/raw output
     - rewrite latency/tokens/raw output when used
     - total latency

4. Update Rust protocol structs in `apps/daemon/src/llm_polish.rs`:

   - Parse nested two-step diagnostics.
   - Keep old fields for compatibility.

5. Update daemon logging in `apps/daemon/src/daemon.rs`:

   - Log decision result.
   - Log whether rewrite was called.
   - Keep compact latency diagnostics.

6. Update `llm-polish-bench/bench_daemon_architecture.py`:

   - Track decision accuracy.
   - Track rewrite-call rate.
   - Keep the current output/quality metrics.

7. Add focused tests:

   - Decision `UNCHANGED` returns original text.
   - Decision `EDIT` runs rewrite.
   - Malformed decision falls back to rewrite.
   - Validation rejection still falls back safely.
   - Diagnostics include decision/rewrite timing.

8. Run verification:

   ```text
   python3 -m py_compile services/polish/llm_polish_sidecar.py services/polish/llm_polish_once.py tests/phase1/test_llm_polish_sidecar.py
   python3 -m unittest tests.phase1.test_llm_polish_sidecar
   python3 -m unittest discover -s tests/phase1
   cargo test --workspace --offline
   cargo clippy --workspace --offline --all-targets -- -D warnings
   cargo build -p sunoto-daemon --release --offline
   ```

9. Restart the macOS daemon from Terminal and wait for:

   ```text
   ASR sidecar ready
   LLM polish post-ASR warmup complete
   Sunoto ready for dictation
   ```

10. Run architecture benchmark:

    ```text
    python3 llm-polish-bench/bench_daemon_architecture.py \
      --dataset llm-polish-bench/synthetic-minimal-v1.jsonl \
      --output llm-polish-bench/out/daemon-architecture/two-step.json
    ```

## Post-Implementation Document

After implementation and benchmark approval, create a separate results document:

```text
docs/llm-polish-two-step-decision-results.md
```

It should include:

- Exact code changes.
- Benchmark command.
- Before/after metrics.
- Remaining failure cases.
- Recommendation: keep experimental, make default, or revert.

## Risks

- The decision call may misclassify edit cases as `UNCHANGED`, hurting quality.
- Two LLM calls may make true edit cases slower.
- A longer decision prompt can increase prompt-token count, but prompt caching
  should soften that after warmup.
- The synthetic set may not match real dictation distribution; live logs should
  remain part of the evaluation.

## Approval Gate

Stop here until the user approves implementation.

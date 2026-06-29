# LLM Polish Constrained One-Call + Eval Plan - 2026-06-29

## Decision

The next serious latency path is **single-call constrained output**, not
two-step routing:

```text
LLM output:
  OK
  EDIT: <cleaned transcript>
```

This keeps the product rule intact: every non-empty final transcript still
passes through an LLM. Clean text exits after a one-token `OK`; edited text pays
only one generation call instead of `decision + rewrite`.

## What We Implemented

New mode:

```bash
SUNOTO_LLM_POLISH_MODE=constrained_one_call
```

Aliases:

```text
constrained
one_pass_constrained
one-pass-constrained
constrained-one-call
```

The sidecar parses:

- `OK` -> return deterministic polish output.
- `EDIT: <text>` -> validate `<text>` and return it if safe.
- malformed output -> mark `decision_malformed=true`.

GBNF grammar is available but disabled by default:

```bash
SUNOTO_LLM_POLISH_GRAMMAR=1
```

## Benchmarks

Dataset:

```text
llm-polish-bench/synthetic-minimal-v1.jsonl
```

Command shape:

```bash
python3 llm-polish-bench/bench_daemon_architecture.py \
  --dataset llm-polish-bench/synthetic-minimal-v1.jsonl \
  --output llm-polish-bench/out/daemon-architecture/<name>.json
```

| Mode | Final exact | Mean WER | Contract / decision | LLM p50 | LLM p95 | Completion p50 / p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| One-pass minimal | `84.4%` | `6.83%` | `84.4%` | `324ms` | `613ms` | `3 / 20` |
| Two-step `OK` | `87.5%` | `3.09%` | `93.8%` | `496ms` | `1136ms` | `1 / 9` |
| One-call + GBNF | `84.4%` | `6.83%` | `87.5%` | `549ms` | `1447ms` | `1 / 14` |
| One-call, no GBNF | `84.4%` | `6.83%` | `87.5%` | `292ms` | `641ms` | `1 / 14` |

Artifacts:

- `llm-polish-bench/out/daemon-architecture/one-pass-current.json`
- `llm-polish-bench/out/daemon-architecture/two-step-ok-label-current.json`
- `llm-polish-bench/out/daemon-architecture/constrained-one-call-current.json`
- `llm-polish-bench/out/daemon-architecture/constrained-one-call-nogrammar-current.json`

## Observations

Single-call constrained output without GBNF is the best latency architecture
candidate so far. It drops p50 below the one-pass baseline:

```text
one-pass p50: 324ms
one-call no-grammar p50: 292ms
```

It narrowly misses p95:

```text
one-pass p95: 613ms
one-call no-grammar p95: 641ms
```

GBNF full-output grammar is not viable for this current prompt/model path. It
makes edit outputs much slower:

```text
one-call + GBNF p50: 549ms
one-call no-grammar p50: 292ms
```

The likely reason is per-token grammar filtering overhead during long `EDIT:`
generations. Grammar still might be useful for tiny labels, but not for the
full rewritten transcript with this model/runtime.

The current constrained prompt is not better quality than one-pass yet. It
matches final exact and WER, improves output contract, and improves p50. Prompt
optimization should now target p95 and edit quality, not architecture.

## Research Findings

### Constrained Decoding

`llama.cpp` supports GBNF grammars, and `llama-cpp-python` exposes grammar
objects through the local runtime. This is useful for hard output shapes, but
our benchmark shows full-transcript grammar slows edit generations too much.

Sources:

- `llama.cpp` grammar docs:
  <https://github.com/ggml-org/llama.cpp/blob/master/grammars/README.md>
- `llama-cpp-python` API reference:
  <https://llama-cpp-python.readthedocs.io/en/latest/api-reference/>

### Cascades And Routing

LLM cascades and routers are a known cost-saving pattern, but they only help
latency if escalation is rare. Our two-step benchmark is the failure case:
edit examples pay router plus rewrite.

Sources:

- FrugalGPT:
  <https://arxiv.org/abs/2305.05176>
- RouteLLM:
  <https://arxiv.org/abs/2406.18665>

### Prompt Optimization With Evals

The durable way to tune this is an eval loop with train/dev/test splits,
category gates, and latency/safety metrics. DSPy optimizers such as MIPROv2
are designed for metric-driven prompt/program optimization. OpenAI Evals is
also relevant as a general eval harness pattern, but our primary evaluator
should stay local because the target runtime is local llama.cpp.

Sources:

- DSPy optimizers:
  <https://dspy.ai/learn/optimization/optimizers/>
- DSPy MIPROv2:
  <https://dspy.ai/api/optimizers/MIPROv2/>
- OpenAI Evals guide:
  <https://platform.openai.com/docs/guides/evals>

### Long-Term Model Shape

ASR cleanup and disfluency removal are structured edit/tagging problems. A
small task model can eventually be faster and more reliable than prompt-only
chat generation.

Sources:

- FastCorrect:
  <https://openreview.net/forum?id=N3oi7URBakV>
- Streaming disfluency detection with BERT:
  <https://arxiv.org/abs/2205.00620>

## What We Should Use

Use this stack, in order:

1. **Primary gate: daemon architecture benchmark**

   This is the truth metric because it goes through the same daemon control
   socket, deterministic polish, warm sidecar, cache, validation, and Rust
   parsing path as the product.

2. **Prompt sweep script for small manual candidate sets**

   Extend `llm-polish-bench/bench_prompt_sweep.py` or add a constrained-mode
   sibling. Use it to compare 5-20 prompt variants quickly.

3. **Prompt optimizer for larger search**

   The existing `llm-polish-bench/optimize_prompt_loop.py` already has useful
   pieces: candidate scoring, safety gates, split rows, and HTML reporting.
   Adapt it to optimize the `OK | EDIT:` contract.

4. **DSPy-style optimization only after the local harness is stable**

   DSPy is useful when we can express the task as a module and objective. We
   should not introduce it before the local eval schema is final.

## Eval Objective

A candidate prompt must be scored by category, not only aggregate exactness.

Hard gates:

- No hard safety failures for numbers, emails, URLs, code, or negation.
- Final exact at least current one-pass baseline: `>= 84.4%`.
- Mean WER no worse than one-pass: `<= 6.83%`.
- Clean/no-op contract at least `95%`.
- LLM p50 below one-pass baseline: `< 324ms`.
- LLM p95 at or below one-pass baseline: `<= 613ms`.

Optimization score:

```text
quality_score
  - hard_safety_penalty
  - clean_false_edit_penalty
  - edit_false_ok_penalty
  - p95_latency_penalty
  - prompt_token_penalty
```

Track separately:

- first post-warmup request latency
- steady-state clean p50/p95
- steady-state edit p50/p95
- completion-token p50/p95
- validation rejections
- malformed outputs
- category-level exactness

## Next Implementation Step

Create a constrained prompt sweep:

```text
llm-polish-bench/bench_constrained_prompt_sweep.py
```

It should:

- evaluate prompt candidates against `synthetic-minimal-v1.jsonl`
- use the same `OK | EDIT:` parser as the sidecar
- record exact/WER/safety/latency/token metrics
- report category failures
- write `all.json` and `report.html`

Then promote only the best candidate into the daemon sidecar and rerun:

```bash
SUNOTO_LLM_POLISH_MODE=constrained_one_call \
python3 llm-polish-bench/bench_daemon_architecture.py \
  --dataset llm-polish-bench/synthetic-minimal-v1.jsonl \
  --output llm-polish-bench/out/daemon-architecture/constrained-best.json
```

## Current Recommendation

Do not enable GBNF by default.

Keep one-pass minimal as the default until constrained one-call beats it on
both p50 and p95. The constrained one-call no-grammar mode is close enough to
justify prompt/eval optimization as the next work item.

# Constrained one-call prompt eval loop - 2026-06-29

Goal: optimize `constrained_one_call` for clean OK precision and live post-ASR
latency. Stop if latency increases.

## What changed

- Added an eval-only override path for constrained prompts:
  - `SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT_FILE`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_JSON`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_FILE`
- Default runtime prompt was restored to the previous constrained prompt after
  candidates failed the combined gate.
- Candidate files are in `llm-polish-bench/prompt-candidates/`.

## Gate baseline

Fresh baseline artifact:
`llm-polish-bench/out/daemon-architecture/constrained-one-call-nogrammar-eval-baseline-20260629.json`

| Run | Final exact | Mean WER | Clean contract | Edit contract | LLM p50 | LLM p95 | Rewrites |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline current prompt | 84.4% | 0.0683 | 90.0% | 83.3% | 318ms | 718ms | 12 |
| v1 OK-first targeted | 84.4% | 0.0481 | 95.0% | 75.0% | 270ms | 678ms | 10 |
| v2 Actually swap | 84.4% | 0.0496 | 90.0% | 75.0% | 267ms | 624ms | 10 |
| v1 promoted rerun | 78.1% | 0.0728 | 90.0% | 83.3% | 280ms | 695ms | 12 |
| v1 at temperature 0 | 78.1% | 0.0728 | 90.0% | 83.3% | 280ms | 700ms | 12 |
| v3 original system + targeted shots | 87.5% | 0.0433 | 65.0% | 91.7% | 346ms | 643ms | 17 |

## Observations

- v1 initially looked promising: it fixed the two expensive identical `EDIT:`
  clean cases for spoken digits/code and lowered p50/p95 latency.
- v1 was not stable enough to promote. A rerun with the same default prompt
  dropped final exact to 78.1% and started removing meaningful `Actually` and
  `Wait`.
- Temperature 0 did not fix that instability.
- v3 improved final exact to 87.5%, but it increased LLM p50 from 318ms to
  346ms. That violates the latency stop rule.
- The current live logs still show the deeper issue: after real ASR finals,
  one-token clean OK calls can take ~1.3-1.9s, and false EDIT calls can take
  ~3.8s. Prompt cleanup can reduce false EDIT frequency, but it does not solve
  the post-ASR MLX/llama.cpp contention by itself.

## Decision

No prompt candidate was promoted.

The default constrained prompt remains the previous stable prompt. The useful
change from this loop is the env/file override hook, which lets us run future
prompt candidates through the daemon without editing source each time.

Current daemon state:

- Mode: `constrained_one_call`
- Grammar: off (`SUNOTO_LLM_POLISH_GRAMMAR=0`)
- Prompt: default restored prompt
- Log: `/tmp/sunoto-constrained-current.log`

## Recommended next fix

Build a dedicated post-ASR LLM latency harness that feeds a WAV through the ASR
sidecar and immediately calls the LLM polish sidecar before insertion. The
current control-socket benchmark is useful for prompt quality, but it does not
fully reproduce the live ASR-final-to-LLM contention seen in daemon logs.

After that harness exists, rerun candidate v4: original system prompt plus the
original clean OK shots plus digit/code OK shots. Only promote it if clean OK
contract improves and both LLM p50 and p95 stay at or below the fresh baseline.

# LLM polish post-ASR latency resolution plan - 2026-06-29

This is a no-memory handoff for another agent. It explains why we are doing
this, what we observed, what was already tried, which references matter, and
what to implement next.

## Repository context

This repo is `voice-dictation` / Sunoto, a local system-wide voice dictation
daemon. On macOS the current ASR path is `parakeet_mlx_streaming`, and LLM
polish is an experimental Python sidecar using `llama-cpp-python` with a local
GGUF model.

Relevant files:

- `services/asr/parakeet_mlx_streaming_sidecar.py`
- `services/polish/llm_polish_sidecar.py`
- `services/polish/llm_polish_once.py`
- `apps/daemon/src/daemon.rs`
- `apps/daemon/src/bench.rs`
- `apps/daemon/src/llm_polish.rs`
- `llm-polish-bench/bench_daemon_architecture.py`
- `llm-polish-bench/synthetic-minimal-v1.jsonl`

macOS operational constraints:

- Do not run two daemons at once.
- Use the bare binary from a terminal/tmux, not launchd, for a working CGEventTap.
- Wait for both `ASR sidecar ready` and `LLM polish post-ASR warmup complete`.
- Current test daemon log used during this investigation:
  `/tmp/sunoto-constrained-current.log`
- Earlier live log with the important latency evidence:
  `/tmp/sunoto-constrained-one-call-tmux.log`

## Current mode and model

**Status update 2026-06-30:** Phi-4-mini Q5 is now the wired experimental
LLM polish model. Step 1 of the resolution plan is done:

- `Settings::llm_polish_model` (default `"phi4_mini"`) resolves the named
  profile to a repo-relative GGUF under `models/llm-polish-hf/` and passes it
  to the sidecar via `SUNOTO_LLM_POLISH_MODEL_PATH`. `"gemma4_e2b"` keeps the
  original Gemma 2B Q4 model. An explicit `llm_polish_model_path` always wins.
  Runtime override: `SUNOTO_LLM_POLISH_MODEL=<phi4_mini|gemma4_e2b>`.
- `validate()` rejects an unknown model name unless an explicit path is set, so
  a daemon/bench run never silently falls back to the sidecar's bundled Gemma
  default.
- The existing macOS config (`llm_polish_enabled: true`, no model field)
  resolves to `phi4_mini` via the serde default — no config edit required.
- LLM polish is still **off by default** for fresh `config init`
  (`llm_polish_enabled: false`); Phi is the model selected when you opt in.
- New end-to-end harness: `llm-polish-bench/e2e_phi_polish.py` (macOS, uses
  `say` + `ffmpeg`). It runs the no-insertion post-ASR bench for clean/edit/wait
  cases AND a live daemon control-socket `polish` smoke, then greps the daemon
  log for latency. Gates: clean release-to-LLM-done p50 < 1500ms & unchanged;
  edit output == "Please open the dashboard." & llm p50 < 1500ms; wait case
  rejected-or-preserved; live polish accepts & returns the edit. Run:
  `python3 llm-polish-bench/e2e_phi_polish.py --sessions 5`. Measured
  2026-06-30 (3 sessions): clean p50 ~198ms, edit p50 ~448ms, wait p50 ~208ms,
  live control-socket polish 326ms — all gates PASS.

Phi is the candidate; it is not yet the safe default for everyone. Remaining
before promotion: stronger sensitive-token (email/digit) validation
(step 3 below), and a longer real-dictation-style run against a focused target.

**Original plan text follows.**

The evaluated mode was:

```bash
SUNOTO_LLM_POLISH_MODE=constrained_one_call
SUNOTO_LLM_POLISH_GRAMMAR=0
SUNOTO_LLM_POLISH_TIMING_THRESHOLD_MS=-1
```

Current LLM model:

```text
models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf
```

Runtime:

- `llama-cpp-python`
- Metal/GPU by default via `SUNOTO_LLM_POLISH_GPU_LAYERS=-1`
- prompt cache enabled in the LLM sidecar

## Why this plan exists

The original thought was: maybe prompt tuning can reduce latency. We tested
that and found prompt tuning helps some false `EDIT:` cases, but it does not
explain the live latency problem.

The key observation is the gap between text-only/control-socket LLM latency and
real post-ASR latency.

Control-socket constrained one-call benchmark:

- LLM p50: 318ms
- LLM p95: 718ms
- final exact: 84.4%
- clean unchanged contract: 90.0%

Real live post-ASR log sample from `/tmp/sunoto-constrained-one-call-tmux.log`:

- LLM latencies: 1941ms, 1342ms, 1834ms, 1544ms, 3842ms, 1638ms, 1456ms
- clean OK LLM median: ~1689ms
- all LLM median: 1638ms
- max false EDIT: 3842ms
- ASR release-to-final was generally fine: about 453-894ms in that sample
- insertion was not the issue: about 18-30ms

This means a text-only benchmark can say "LLM p50 is 318ms" while the user
feels "LLM after ASR takes 1.5-4s." The next benchmark must reproduce the
ASR-final-to-LLM path.

## What was already tried

Prompt/candidate report:

`docs/llm-polish-constrained-prompt-eval-loop-2026-06-29.md`

Important artifacts:

- `llm-polish-bench/out/daemon-architecture/constrained-one-call-nogrammar-eval-baseline-20260629.json`
- `llm-polish-bench/out/daemon-architecture/constrained-ok-first-v1-20260629.json`
- `llm-polish-bench/out/daemon-architecture/constrained-ok-first-v2-20260629.json`
- `llm-polish-bench/out/daemon-architecture/constrained-one-call-optimized-default-20260629.json`
- `llm-polish-bench/out/daemon-architecture/constrained-one-call-optimized-temp0-20260629.json`
- `llm-polish-bench/out/daemon-architecture/constrained-original-system-targeted-few-shot-v3-20260629.json`

Summary:

| Run | Final exact | Mean WER | Clean contract | Edit contract | LLM p50 | LLM p95 | Rewrites |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline current prompt | 84.4% | 0.0683 | 90.0% | 83.3% | 318ms | 718ms | 12 |
| v1 OK-first targeted | 84.4% | 0.0481 | 95.0% | 75.0% | 270ms | 678ms | 10 |
| v2 Actually swap | 84.4% | 0.0496 | 90.0% | 75.0% | 267ms | 624ms | 10 |
| v1 default rerun | 78.1% | 0.0728 | 90.0% | 83.3% | 280ms | 695ms | 12 |
| v1 temperature 0 | 78.1% | 0.0728 | 90.0% | 83.3% | 280ms | 700ms | 12 |
| v3 original system + targeted shots | 87.5% | 0.0433 | 65.0% | 91.7% | 346ms | 643ms | 17 |

Decision from that loop:

- No prompt candidate was promoted.
- v1 initially improved clean OK precision and latency, but was unstable on
  rerun.
- v3 improved final exact but increased p50 latency from 318ms to 346ms, so it
  violated the user's stop rule.
- The default constrained prompt was restored.
- The useful change that remains is an eval-only prompt override hook:
  - `SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT_FILE`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_JSON`
  - `SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_FILE`

## Leading hypotheses

### Hypothesis 1 - MLX/Metal GPU contention

ASR uses MLX on Apple GPU. LLM polish uses llama.cpp/Metal on the same machine.
The live LLM call starts immediately after ASR final. The large gap between
control-socket LLM latency and post-ASR LLM latency suggests contention or
queued GPU work.

### Hypothesis 2 - MLX lazy evaluation is shifting time into the LLM window

MLX is lazy. If ASR final emits before all GPU work is fully synchronized, the
next llama.cpp/Metal call might inherit the wait. The user sees this as LLM
latency, but some of the work may belong to ASR.

### Hypothesis 3 - current 2B Q4 model is too large for the live path

Even if contention is fixed, the current model may still be too slow for a
post-dictation polish step. Smaller LLMs may be better for this narrow task.

### Hypothesis 4 - clean utterances should not call a generative LLM

Most dictation is already clean. A learned OK/Edit router could return clean
utterances in a few milliseconds and only call a rewrite LLM for real repair
cases. The user explicitly rejected a deterministic router as a long-term
solution, so this should be learned/eval-driven, not hand-coded rules.

## External references

MLX:

- MLX quick start and lazy evaluation:
  https://ml-explore.github.io/mlx/build/html/usage/quick_start.html
- MLX synchronize API:
  https://ml-explore.github.io/mlx/build/html/python/_autosummary/mlx.core.synchronize.html
- MLX-LM project:
  https://github.com/ml-explore/mlx-lm

llama.cpp / llama-cpp-python:

- llama-cpp-python API reference, including model/runtime parameters:
  https://llama-cpp-python.readthedocs.io/en/latest/api-reference/
- `n_gpu_layers=-1` means offload all layers; `n_gpu_layers=0` is the CPU-only
  isolation test.

Candidate replacement models:

- Gemma 3 270M, Google announcement:
  https://developers.googleblog.com/en/introducing-gemma-3-270m/
- Qwen3-0.6B model card:
  https://huggingface.co/Qwen/Qwen3-0.6B
- Qwen3-1.7B model card, useful if 0.6B quality is too weak:
  https://huggingface.co/Qwen/Qwen3-1.7B
- SmolLM2-360M-Instruct model card:
  https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct
- SmolLM2 family announcement:
  https://huggingface.co/blog/smollm2

Related architecture/research ideas:

- FrugalGPT: use cheaper/faster models first and escalate when needed.
  https://arxiv.org/abs/2305.05176
- RouteLLM: learned routing between cheaper and stronger models.
  https://github.com/lm-sys/RouteLLM

## Implementation plan

### Phase 1 - Build the correct post-ASR latency harness

Do this first. Do not promote model/config/prompt changes based only on
`bench_daemon_architecture.py`.

The harness should run:

```text
WAV/audio -> parakeet_mlx_streaming final -> deterministic polish -> LLM polish -> no insertion
```

Best implementation path:

1. Extend `apps/daemon/src/bench.rs` or add a new bench subcommand.
2. Reuse the existing ASR sidecar path already in `bench.rs`.
3. Reuse `apps/daemon/src/llm_polish.rs::LlmPolishClient`.
4. Add a `--llm-polish` / `--post-asr-llm` mode that:
   - starts ASR sidecar
   - starts and warms LLM polish sidecar
   - sends WAV chunks through ASR
   - records ASR final timing
   - applies deterministic polish
   - immediately calls LLM polish
   - records LLM diagnostics
   - skips insertion

Report fields:

- `session`
- `audio_seconds`
- `release_to_final_ms`
- `deterministic_polish_ms`
- `llm_latency_ms`
- `final_to_llm_done_ms`
- `release_to_llm_done_ms`
- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `cache_hit`
- `cache_matched_tokens`
- `cache_entries`
- `rewrite_called`
- `decision_label`
- `raw_transcript`
- `deterministic_output`
- `llm_output`

Percentiles:

- `release_to_final_ms`
- `llm_latency_ms`
- `final_to_llm_done_ms`
- `release_to_llm_done_ms`

Acceptance:

- The harness must be able to run with the current model and reproduce the
  live gap, or explain why it cannot.
- Do not type into the user's active app. This is a no-insertion benchmark.

### Phase 2 - Prove or disprove GPU contention

Run the Phase 1 harness across these variants:

1. Current path:
   - Parakeet MLX ASR
   - llama.cpp/Metal LLM
   - `SUNOTO_LLM_POLISH_GPU_LAYERS=-1`

2. Explicit MLX synchronize before final emit:
   - Add an env flag to `parakeet_mlx_streaming_sidecar.py`, for example
     `SUNOTO_ASR_MLX_SYNCHRONIZE_FINAL=1`.
   - When enabled, call MLX synchronization right before emitting final.
   - Record synchronize duration in sidecar logs/diagnostics if possible.

3. CPU-only current LLM:
   - `SUNOTO_LLM_POLISH_GPU_LAYERS=0`
   - This isolates whether llama.cpp/Metal is competing with MLX.

4. Text-only control:
   - `llm-polish-bench/bench_daemon_architecture.py`
   - Same prompt/model, no ASR immediately before the LLM call.

Interpretation:

- If CPU-only removes the 1.3-1.9s clean OK stall, the problem is GPU
  contention. Then test small CPU models.
- If explicit MLX synchronization moves time from LLM to ASR but total time is
  unchanged, the previous logs were misattributing async MLX work.
- If explicit MLX synchronization reduces total `release_to_llm_done_ms`, keep
  it.
- If neither helps, the current model/runtime is simply too slow for the path.

### Phase 3 - Model/runtime/config sweep

Only after Phase 1 and Phase 2.

Models to test:

1. Current Gemma 2B Q4 GGUF as control.
2. Gemma 3 270M, GGUF and/or MLX.
3. Qwen3-0.6B, GGUF and/or MLX.
4. SmolLM2-360M-Instruct, GGUF and/or MLX.
5. Qwen3-1.7B only if 0.6B is too weak and latency headroom exists.

Runtimes/configs:

- llama.cpp Metal, `SUNOTO_LLM_POLISH_GPU_LAYERS=-1`
- llama.cpp CPU, `SUNOTO_LLM_POLISH_GPU_LAYERS=0`
- llama.cpp partial GPU offload if useful
- MLX-LM sidecar prototype if CPU-only is too slow or if unified MLX runtime
  seems better than mixed MLX + llama.cpp/Metal

llama.cpp sweep knobs:

- `SUNOTO_LLM_POLISH_THREADS=4|6|8`
- `SUNOTO_LLM_POLISH_BATCH=128|256|512`
- `SUNOTO_LLM_POLISH_UBATCH=128|256|512`
- `SUNOTO_LLM_POLISH_TEMPERATURE=0`
- grammar off
- current dynamic token cap, then stricter edit cap

Promotion gate:

- live post-ASR clean OK p50 under 500ms, target under 250ms
- live post-ASR p95 under 900ms, target under 600ms
- clean OK precision >= current baseline
- final exact >= current baseline
- no hard validation regression
- no startup/warmup regression that makes normal use painful

### Phase 4 - Learned OK/Edit router if model swap is not enough

Do not build a long-term deterministic router.

Build a learned router:

- input: deterministic-polished transcript
- output: `OK`, `EDIT`, or `UNSURE`
- tune threshold for high clean OK precision
- only `EDIT` or `UNSURE` calls the rewrite LLM

Candidate first implementation:

- Python model with char/word n-gram features plus logistic regression or a
  compact classifier.
- Train/eval from:
  - `llm-polish-bench/synthetic-minimal-v1.jsonl`
  - future live accepted/rejected examples
  - labels derived from expected LLM mode in the benchmark artifacts

Target:

- router p50 under 20ms
- clean OK precision >= 98%
- edit recall high enough that real correction cases still escalate
- no generative LLM call for obvious clean text

Reasoning:

Clean utterances dominate normal dictation. If the router is reliable, the
steady-state path becomes ASR final -> router OK -> insertion, and rewrite LLM
latency only applies to true repair cases.

### Phase 5 - Prompt optimization resumes only after the harness exists

Use the env/file override hook added in `llm_polish_once.py` to run candidates
without source edits.

Do not promote a prompt if:

- live post-ASR p50 or p95 increases
- clean OK precision regresses
- final exact regresses
- the result is unstable across reruns

Candidate worth retrying after the harness:

- original stable system prompt
- original clean OK examples
- plus digit/code OK examples
- maybe fewer edit examples only if edit recall holds

## Suggested first task for the next agent

Implement Phase 1 as a Rust bench extension.

Start by reading:

1. `AGENTS.md`
2. `apps/daemon/src/bench.rs`
3. `apps/daemon/src/llm_polish.rs`
4. `services/asr/parakeet_mlx_streaming_sidecar.py`
5. `services/polish/llm_polish_sidecar.py`
6. `docs/llm-polish-constrained-prompt-eval-loop-2026-06-29.md`

Then add a no-insertion benchmark that writes a JSON report under:

```text
llm-polish-bench/out/post-asr-llm-latency/
```

Run current baseline first. Only after that, test CPU-only and MLX sync.

## Commands and operational notes

Check current daemon/log:

```bash
ps -axo pid,ppid,etime,command | rg 'sunoto-daemon run|llm_polish_sidecar|parakeet_mlx_streaming' | rg -v rg
tail -f /tmp/sunoto-constrained-current.log
```

Start current macOS daemon manually:

```bash
SUNOTO_LLM_POLISH_MODE=constrained_one_call \
SUNOTO_LLM_POLISH_GRAMMAR=0 \
SUNOTO_LLM_POLISH_TIMING_THRESHOLD_MS=-1 \
target/release/sunoto-daemon run > /tmp/sunoto-constrained-current.log 2>&1
```

Existing text-only LLM benchmark:

```bash
python3 llm-polish-bench/bench_daemon_architecture.py \
  --output llm-polish-bench/out/daemon-architecture/latest.json \
  --timeout-s 45
```

No-model sidecar tests:

```bash
python3 -m unittest tests.phase1.test_llm_polish_sidecar
```

Do not run a second ASR/LLM daemon while another daemon is active.

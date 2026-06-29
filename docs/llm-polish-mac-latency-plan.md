# LLM Polish Mac Latency Plan

## Goal

Make the local LLM polish step fast enough for normal macOS dictation while
keeping the live architecture simple:

```text
audio -> ASR final transcript -> LLM polish -> insertion
```

ASR partials must remain display-only. Do not send partial transcripts to the
LLM. Do not add deterministic routing, skipping, or gating as the primary
latency fix. Every final transcript must go through the LLM in the live
acceptance path.

Latest first-pass observations:
`docs/llm-polish-mac-latency-results-2026-06-28.md`.

## Targets

Normal dictation release-to-insertion:

- p50 <= 700ms
- p95 <= 1000ms
- Any normal dictation above 1000ms is unacceptable.

LLM polish request:

- p50 <= 300ms
- p95 <= 500ms
- Any LLM request above 700ms needs an explanation in logs.

The current Parakeet streaming final path has been observed around 500-750ms
release-to-final, so the LLM budget in the real app is very tight.

## Current Observation

The real app log showed:

```text
ASR turnaround 740ms
llm polish accepted in 5336ms
insert 34ms
release->insertion 6116ms
```

The LLM sidecar was already prewarmed, so this was not initial model load. The
problem is live request latency after Parakeet/MLX is already loaded and active
on Apple Silicon.

## Non-Goals

- No speculative LLM work from ASR partials.
- No deterministic router that skips clean transcripts.
- No product behavior changes until the runtime latency is understood.
- No broad prompt-quality optimizer runs during this investigation.

## Hypotheses

1. **Warmup order is wrong.**
   The LLM is warmed before Parakeet/MLX finishes loading and warming on the
   GPU. Parakeet may invalidate or perturb the Metal runtime state, making the
   first real LLM request slow.

2. **Metal/MLX contention.**
   Parakeet MLX and llama.cpp Metal share Apple unified memory and GPU
   resources. A request immediately after ASR final decode may contend with
   MLX cleanup, command buffers, or memory pressure.

3. **Prompt eval dominates.**
   The fixed system prompt plus few-shots may be expensive. If prompt eval is
   the slow part, prefix/KV caching or shorter prompts may help.

4. **Generation runs longer than visible output suggests.**
   The cleaned output may be short even if the raw model output was long.
   Finish reason, completion tokens, and raw output length are required.

5. **llama.cpp settings are not optimal for this machine.**
   `n_ctx`, `n_batch`, `n_ubatch`, `flash_attn`, `n_threads`,
   `n_gpu_layers`, and memory-lock settings need a controlled sweep.

6. **Model/runtime choice is wrong for the Mac app path.**
   Gemma GGUF through llama.cpp/Metal may be fast in isolation but slow beside
   Parakeet MLX. MLX-LM or a smaller model may be better.

## Instrumentation Required

For every LLM request, log:

- session id
- input chars and word count
- raw output chars
- cleaned output chars
- prompt tokens
- completion tokens
- total tokens
- max tokens requested
- finish reason
- prompt eval time, if available
- token eval time, if available
- tokens/sec, if available
- total wall time
- backend and settings summary

The log must distinguish:

```text
model load time
startup generation warmup time
per-request prompt eval time
per-request generation time
daemon wait time
```

## Experiment Matrix

Use the same prompts across every state.

Clean prompt:

```text
Hey, how are you doing?
```

Repair prompt:

```text
Her email is jane, no, janet dot smith at example dot com.
```

Longer normal dictation prompt:

```text
I wanted to follow up on the notes from yesterday and confirm that the next draft will be ready before lunch.
```

Runtime states:

1. LLM alone, no ASR process running.
2. LLM after Parakeet is loaded and idle.
3. LLM immediately after a Parakeet final decode.
4. LLM second request after a Parakeet final decode.
5. Full daemon path, first dictation after startup.
6. Full daemon path, second dictation after startup.

Run at least 20 requests per prompt per state for p50/p95. Record first request
separately from steady-state latency.

## Candidate Model Priority

Start with models that can run in the existing local runtimes without adding a
new product architecture.

| Priority | Candidate | Runtime | Why | First-pass status |
| --- | --- | --- | --- | --- |
| 1 | `gemma-4-e2b-it-q4` GGUF | llama.cpp/Metal | Current fastest steady-state backend; already integrated. | Keep as baseline and primary target. |
| 2 | `mlx-community/gemma-3n-E2B-it-lm-4bit` | MLX-LM | Text-native E2B LM, smaller than Gemma 4 E2B VLM variants. | Test as first MLX E2B candidate. |
| 3 | `mlx-community/gemma-3-1b-it-4bit` | MLX-LM | Smaller fallback that may trade quality for latency. | Test with stop-token handling before judging fully. |
| 4 | `mlx-community/gemma-4-e2b-it-OptiQ-4bit` | MLX / maybe VLM | Apple-Silicon-targeted quantization, but larger. | Test only if smaller MLX candidates look promising. |
| 5 | `mlx-community/gemma-4-e2b-it-4bit` | MLX-VLM | Already cached, popular, but not loadable by plain MLX-LM. | Requires MLX-VLM or adapter harness. |
| 6 | E4B / MoE variants | MLX / llama.cpp | Possible quality improvement, but likely memory/latency risk on 16GB Mac. | Defer until E2B/1B results justify it. |

Current working hypothesis after the first pass: the current GGUF
llama.cpp/Metal backend is the best latency target. MLX candidates are still
worth tracking, but they should not replace the current backend unless they
beat it in the full Parakeet-loaded matrix.

## Tuning Matrix

Only tune one axis at a time.

### Warmup

- Current startup warmup before ASR ready.
- Extra production-shaped warmup after ASR ready.
- Warmup with clean prompt and repair prompt.
- Warmup with same `max_tokens` as production.

### llama.cpp Settings

- `n_ctx`: 512, 1024, 2048.
- `n_batch`: 128, 256, 512.
- `n_ubatch`: 128, 256, 512.
- `flash_attn`: on, off.
- `n_threads`: 4, 6, 8.
- `n_gpu_layers`: full Metal, partial Metal, CPU.
- `use_mlock`: on, off.
- `logits_all`: keep false unless a benchmark proves otherwise.

### Prompt Shape

- Current two-shot chat prompt.
- One-shot prompt.
- Zero-shot compact prompt.
- Same instructions with a shorter system message.
- Raw completion format instead of chat format, if quality remains acceptable.

### Output Controls

- `max_tokens`: 16, 24, 32, 48.
- Stop sequences that prevent explanatory text.
- Temperature 0.0 vs 0.1.
- Greedy decoding settings where supported.

### Runtime Alternatives

- Current GGUF Gemma via llama.cpp/Metal.
- Same model through MLX-LM if available.
- `mlx-community/gemma-3n-E2B-it-lm-4bit`.
- `mlx-community/gemma-3-1b-it-4bit` with explicit stop-token handling.
- `mlx-community/gemma-4-e2b-it-OptiQ-4bit`.
- Gemma 4 E2B/E4B MLX-VLM variants if we add or use the VLM harness.
- Smaller/faster local model candidates.
- CPU-only LLM for contention comparison, even if likely slower.

## Decision Rules

1. If LLM alone is fast but slows only when Parakeet is loaded, prioritize
   Metal/MLX contention experiments.
2. If only the first request after ASR is slow, prioritize post-ASR warmup.
3. If prompt eval is slow but generation is fast, prioritize prompt shortening
   and prefix/KV caching.
4. If generation is slow, prioritize smaller model, output caps, and decoding
   settings.
5. If all paths are slow on the current model/runtime, switch runtime/model
   rather than adding product-level routing.

## Implementation Phases

### Phase 1: Restore Experiment Baseline

- Remove deterministic LLM router behavior.
- Keep final-transcript-only LLM polish.
- Keep or add diagnostic logging.
- Confirm every final transcript goes through LLM.

### Phase 2: Build Benchmark Harness

- Create a repeatable local harness that can run the experiment matrix without
  typing into a real app.
- Emit JSON and a small summary table with p50, p95, max, prompt tokens,
  completion tokens, and tokens/sec.
- Include a mode that keeps Parakeet loaded and idle.
- Include a mode that triggers Parakeet final decode immediately before LLM.

### Phase 3: Run Baseline Matrix

- Run the three prompts across all six runtime states.
- Save artifacts under:

```text
llm-polish-bench/out/macos-live-latency/
```

- Write observations to a dated markdown file in `docs/`.
- Include model metadata, load time, warmup time, p50, p95, max, output
  correctness notes, and artifact paths.

### Phase 4: Tune

- Run warmup-order experiments first.
- Run llama.cpp parameter sweeps.
- Run prompt-shape sweeps.
- Run runtime/model alternatives only after the current stack is measured.

### Phase 5: Live Acceptance

Run the real daemon with Parakeet and LLM enabled. Dictate:

1. Clean short phrase twice.
2. Repair phrase twice.
3. Longer normal phrase twice.

Acceptance requires:

- Every final transcript logs an LLM request.
- Normal dictation release-to-insertion p95 <= 1000ms.
- LLM p95 <= 500ms.
- No individual normal dictation exceeds 1000ms without a logged explanation.

## Useful Commands

Check the live daemon log:

```bash
tail -f /tmp/sunoto-bare.log
```

Confirm there is only one daemon stack:

```bash
ps -axo pid,ppid,stat,command | rg 'sunoto-daemon run|parakeet_mlx_streaming_sidecar.py|llm_polish_sidecar.py|nemotron' | rg -v rg
```

Build and test:

```bash
cargo test --workspace --offline
cargo clippy --workspace --offline --all-targets -- -D warnings
cargo build -p sunoto-daemon --release --offline
```

Restart the macOS daemon from Terminal context:

```bash
pkill -f 'target/release/sunoto-daemon run' || true
osascript -e 'tell application "Terminal" to do script "cd /Users/antash/workspace/voice-dictation && nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 & disown"'
```

## References

- `docs/llm-polish-optimization-handoff.md`
- `docs/llm-polish-research.md`
- `docs/desktop-configuration.md`
- `services/polish/llm_polish_sidecar.py`
- `services/asr/parakeet_mlx_streaming_sidecar.py`
- llama-cpp-python macOS Metal install guidance:
  `https://llama-cpp-python.readthedocs.io/en/latest/install/macos/`
- llama-cpp-python API reference:
  `https://llama-cpp-python.readthedocs.io/en/latest/api-reference/`
- MLX-LM:
  `https://github.com/ml-explore/mlx-lm`

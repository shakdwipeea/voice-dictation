# LLM Polish Mac Latency Results - 2026-06-28

## Summary

First-pass result: keep the current `gemma-4-e2b-it-q4` GGUF model through
llama.cpp/Metal as the primary latency target.

The MLX Gemma candidates tested so far did not beat the current GGUF backend.
The live 5s LLM delay is more likely a first production-shaped warmup issue
than a steady-state Parakeet/MLX contention issue.

## Environment

- Machine: Apple M1 Pro, 16GB unified memory.
- ASR backend: `parakeet_mlx_streaming`, model
  `mlx-community/parakeet-tdt-0.6b-v3`.
- LLM baseline backend: `llama-cpp-python==0.3.31` with Metal libraries.
- MLX runtime: `mlx==0.31.2`, `mlx-lm==0.31.3`.
- Current GGUF model:
  `models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf`.

## Candidate Metadata

| Candidate | Runtime | Approx repo size | Notes |
| --- | --- | ---: | --- |
| `mlx-community/gemma-3n-E2B-it-lm-4bit` | MLX-LM | 2.55GB | Text-native E2B LM candidate. |
| `mlx-community/gemma-4-e2b-it-OptiQ-4bit` | MLX / maybe VLM | 5.26GB | Apple-Silicon-targeted mixed precision quant, larger. Not tested yet. |
| `mlx-community/gemma-4-e2b-it-4bit` | MLX-VLM | 3.61GB | Already cached locally, but plain MLX-LM failed to load it. |
| `mlx-community/gemma-4-E2B-it-qat-4bit` | MLX-VLM | 4.36GB | Likely needs VLM harness. Not tested yet. |
| `mlx-community/gemma-3-1b-it-4bit` | MLX-LM | 0.77GB | Smaller latency candidate; needs stop-token handling. |

The cached `gemma-4-e2b-it-4bit` failed with plain MLX-LM because the weights
did not match the text-only model class. It should be treated as an MLX-VLM
candidate, not a drop-in MLX-LM backend.

## Prompts

Clean:

```text
Hey, how are you doing?
```

Repair:

```text
Her email is jane, no, janet dot smith at example dot com.
```

Long:

```text
I wanted to follow up on the notes from yesterday and confirm that the next draft will be ready before lunch.
```

## Results

### llama.cpp/Metal GGUF, LLM Alone

Artifact:
`llm-polish-bench/out/macos-live-latency/llamacpp-gemma-4-e2b-q4-llm-alone.json`

| Prompt | p50 | p95 | Max | Notes |
| --- | ---: | ---: | ---: | --- |
| Clean | 122ms | 157ms | 158ms | Fast, but sometimes dropped `Hey,`. |
| Repair | 257ms | 262ms | 306ms | Correct email repair. |
| Long | 457ms | 460ms | 506ms | Meets p95 target; max slightly above 500ms. |

Load time was about 690ms. The first production-shaped warmup request took
5471ms. After that warmup, steady-state latency was excellent.

### llama.cpp/Metal GGUF, Parakeet Loaded And Idle

Artifact:
`llm-polish-bench/out/macos-live-latency/llamacpp-gemma-4-e2b-q4-parakeet-idle.json`

| Prompt | p50 | p95 | Max | Notes |
| --- | ---: | ---: | ---: | --- |
| Clean | 121ms | 157ms | 161ms | No meaningful slowdown vs LLM-alone. |
| Repair | 253ms | 262ms | 300ms | Correct email repair. |
| Long | 452ms | 456ms | 498ms | Under target. |

Parakeet being loaded and idle did not meaningfully slow the LLM.

### llama.cpp/Metal GGUF, Immediately After Parakeet Final Decode

Artifact:
`llm-polish-bench/out/macos-live-latency/llamacpp-gemma-4-e2b-q4-after-parakeet-final.json`

This test used synthetic non-silent 16 kHz audio to force Parakeet's final
decode path, then immediately ran the clean LLM polish prompt.

| Prompt | p50 | p95 | Max | Notes |
| --- | ---: | ---: | ---: | --- |
| Clean | 245ms | 261ms | 276ms | Still within LLM target after explicit LLM warmup. |

ASR final decode took about 978-1054ms on the synthetic audio. The LLM did not
show a steady 3-5s slowdown after ASR final decode.

### MLX-LM Gemma 3n E2B 4-bit, LLM Alone

Artifact:
`llm-polish-bench/out/macos-live-latency/mlx-gemma-3n-e2b-lm-4bit-llm-alone.json`

| Prompt | p50 | p95 | Max | Notes |
| --- | ---: | ---: | ---: | --- |
| Clean | 821ms | 838ms | 859ms | Correct but above target. |
| Repair | 982ms | 1023ms | 1149ms | Correct but too slow. |
| Long | 1178ms | 1241ms | 1250ms | Too slow. |

The model loaded from cache in about 2657ms. The first warmup request took
3436ms. Steady-state latency missed the target before Parakeet was involved.

### MLX-LM Gemma 3 1B 4-bit, LLM Alone

Artifact:
`llm-polish-bench/out/macos-live-latency/mlx-gemma-3-1b-it-4bit-llm-alone.json`

| Prompt | p50 | p95 | Max | Notes |
| --- | ---: | ---: | ---: | --- |
| Clean | 633ms | 637ms | 642ms | Emits repeated `<end_of_turn>` without stop handling. |
| Repair | 752ms | 764ms | 1003ms | Correct visible repair, but too many extra tokens. |
| Long | 896ms | 944ms | 999ms | Too slow as measured. |

This model may deserve a second test with explicit stop-token handling, but it
does not beat the current llama.cpp/Metal GGUF baseline in the raw pass.

## Observations

1. The current llama.cpp/Metal GGUF backend is the best latency candidate so
   far.
2. Parakeet loaded-and-idle does not explain the 5s live LLM request.
3. A Parakeet final decode immediately before the LLM request also did not
   reproduce a 5s LLM latency once the LLM had a production-shaped warmup.
4. The first production-shaped llama.cpp request can take 4-5.5s. This matches
   the failure shape seen in the live app.
5. The sidecar's existing tiny warmup is probably not enough. The next daemon
   experiment should run a production-shaped LLM warmup after ASR readiness.
6. MLX E2B is slower than the current GGUF backend in steady state. It is not
   the best immediate replacement.
7. MLX 1B needs stop handling before a fair quality/latency call, but its first
   token latency was already around 400-490ms, so it is unlikely to beat the
   current GGUF backend.
8. The baseline prompt sometimes drops greeting filler like `Hey,`. That is a
   prompt/correctness issue separate from inference latency.

## Current Recommendation

Do not switch to MLX Gemma yet.

Use the current GGUF llama.cpp/Metal backend and fix warmup sequencing:

```text
start Parakeet
wait for ASR ready
start or keep LLM sidecar
run production-shaped LLM warmup
only then accept dictation
```

The production-shaped warmup should use the same prompt/few-shot path as real
requests and should not be limited to a tiny 4-token completion.

## Next Tests

1. Remove the deterministic router and restore every-final-transcript LLM
   behavior.
2. Add daemon-side production-shaped warmup after ASR ready.
3. Rerun real daemon dictation:
   - first clean phrase
   - second clean phrase
   - repair phrase
   - long phrase
4. Add explicit stop-token handling for MLX-LM and retest
   `mlx-community/gemma-3-1b-it-4bit`.
5. Only if MLX 1B improves substantially, test
   `mlx-community/gemma-4-e2b-it-OptiQ-4bit`.

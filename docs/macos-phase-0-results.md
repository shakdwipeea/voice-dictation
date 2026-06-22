# macOS Phase 0 Results — Nemotron macOS ASR latency

> **Superseded.** The macOS real-ASR path has moved to Parakeet-MLX on Apple
> GPU/Metal — see `docs/parakeet-mlx-migration-plan.md` and
> `docs/desktop-configuration.md` (default backend `parakeet_mlx_streaming`).
> This document is retained as the historical Nemotron Phase 0 benchmark and
> the rationale for why the NeMo CPU/CoreML path was abandoned.

Measurement notes for the macOS port's ASR decision (see
`docs/macos-port-plan.md` §1). The original Phase 0 benchmark only compared
whole-utterance `model.transcribe()` on Apple Silicon. Live dictation showed
that this was the wrong signal for the product latency path.

## Historical decision: offline CPU RNNT

Use `backend = "nemotron_offline"` with `asr_device = "cpu"` for the
**legacy** macOS real-ASR path (superseded by Parakeet-MLX — see the header
note). It is not sub-second, but it is stable and does not block the daemon
while recording.

`backend = "nemotron"` with `asr_device = "cpu"` is supported for experiments,
but the live daemon test was worse: a 26.6 s hold delivered only 4.6 s of audio
to the sidecar and still waited 7.38 s after release. That points to CPU
streaming backpressure, not a usable latency win.

## Why the earlier MPS result changed

The first Phase 0 script used synthesized tone WAVs. That proved the model
could load and execute on MPS, but it produced empty transcripts and therefore
under-measured the RNNT decoder work that real speech triggers. Tone timing is
now treated as a smoke test only, not a product latency gate.

The benchmark script has been updated to default to real speech from Hugging
Face:

```bash
.venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
  --device cpu \
  --output build/phase0/macos-real-speech-cpu.json
```

It downloads `hf-internal-testing/librispeech_asr_dummy` (`clean`,
`validation`), exports cached 16 kHz mono WAVs, records latency/RTF, and
computes WER when reference text is present.

## Hugging Face offline real-speech benchmark

Recorded on MPS and CPU with the same 5 Hugging Face LibriSpeech clips:

```bash
.venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
  --device mps \
  --limit 5 \
  --output build/phase0/macos-real-speech-mps-limit5.json

.venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
  --device cpu \
  --limit 5 \
  --output build/phase0/macos-real-speech-cpu-limit5.json
```

Summary:

| metric | MPS | CPU | CPU rerun |
| --- | ---: | ---: | ---: |
| actual device | MPS | CPU | CPU |
| clips | 5 | 5 | 5 |
| warm import | 5.69 s | 6.06 s | 5.74 s |
| model load | 6.89 s | 7.34 s | 6.53 s |
| warm-up transcribe | 1.04 s | 1.67 s | 1.59 s |
| latency p50 | 1.015 s | 1.531 s | 1.589 s |
| latency p95 / max | 2.123 s | 2.248 s | 2.215 s |
| RTF p50 | 0.103 | 0.155 | 0.160 |
| mean WER | 0.0265 | 0.0265 | 0.0265 |

Clip-level latency:

| clip | audio | MPS | CPU | CPU rerun |
| --- | ---: | ---: | ---: | ---: |
| 000 | 5.86 s | 0.806 s | 1.500 s | 1.589 s |
| 001 | 4.82 s | 0.562 s | 1.363 s | 1.426 s |
| 002 | 12.48 s | 1.264 s | 1.565 s | 1.643 s |
| 003 | 9.90 s | 1.015 s | 1.531 s | 1.580 s |
| 004 | 29.40 s | 2.123 s | 2.248 s | 2.215 s |

MPS won on this controlled offline benchmark, but this benchmark processes
cached clean clips after the fact. It does not measure the live streaming
schedule, mic capture behavior, or the release-to-final path users feel.

## Real-speech spot measurements

These older numbers came from live daemon logs on this Mac with short
"Hello, how are you..." utterances. They measure the path users feel:
release-to-final ASR plus insertion, but they were captured before the sidecar
started using `verbose=False`, `batch_size=1`, `num_workers=0`, and
`use_lhotse=False` by default.

### CPU (`asr_device = "cpu"`)

| session | audio | ASR turnaround | release-to-insertion |
| --- | ---: | ---: | ---: |
| 2 | 1.88 s | 1.292 s | 1.304 s |
| 3 | 1.86 s | 1.243 s | 1.254 s |
| 4 | 1.96 s | 1.350 s | 1.364 s |
| 5 | 2.10 s | 1.239 s | 1.251 s |

Session 1 was slower (`2.282 s` release-to-final), likely a first-run warm
effect after switching devices. After that, CPU stabilized around
`1.24–1.35 s` ASR turnaround for roughly two seconds of speech.

### MPS (`asr_device = "mps"`)

| session | audio | ASR turnaround | release-to-insertion |
| --- | ---: | ---: | ---: |
| 2 | 2.00 s | 2.299 s | 2.336 s |
| 3 | 1.76 s | 2.044 s | 2.060 s |
| 4 | 2.14 s | 1.767 s | 1.780 s |
| 5 | 2.86 s | 5.580 s | timed out |
| 6 | 2.14 s | 1.596 s | 1.612 s |

This MPS live run was slower and had a timeout-inducing spike. Even after
offline wrapper cleanup, the controlled benchmark was not enough evidence to
make MPS the default because the live short-utterance path is the product
signal.

## Practical interpretation

- CPU streaming RNNT is not viable on this Mac with the current PyTorch/NeMo
  path. It backpressures the daemon and still leaves multi-second release
  latency.
- MPS should stay experimental until it wins in live short-utterance daemon
  logs, not just offline file benchmarks.
- The whole-utterance NeMo path is usable but not yet "fast dictation" fast:
  even the best offline p50 still waits until release to do the ASR work.
- Sub-second macOS latency likely needs streaming or an ONNX/CoreML backend,
  not smaller daemon-side optimizations.

## Next optimization candidates

1. **ONNX Runtime / quantized Nemotron.** A 2026 on-device ASR study reports a
   Nemotron streaming ONNX Runtime path with int4 quantization that is
   faster-than-real-time on CPU and much smaller than the PyTorch baseline.
   This is the most plausible route to sub-second local macOS dictation while
   keeping Nemotron.
2. **Alternative NeMo ASR models.** Benchmark Parakeet-TDT/CTC variants on the
   same real-speech harness. They may be faster on CPU, but punctuation,
   capitalization, and dictation quality must be checked before switching.
3. **Wrapper overhead cleanup.** The sidecar already spends only ~1–3 ms
   writing WAVs and ~10–30 ms inserting text. Avoiding `model.transcribe()`'s
   dataloader construction has already helped MPS, but it will not by itself
   turn whole-utterance ASR into a consistent sub-500 ms path.

## References checked

- Nemotron model card: `nvidia/nemotron-speech-streaming-en-0.6b` is a
  cache-aware FastConformer-RNNT streaming model with 80/160/560/1120 ms chunk
  configurations and normal NeMo `ASRModel.from_pretrained(...).transcribe()`
  usage.
- PyTorch MPS docs: `mps` maps PyTorch graphs and primitives to Metal
  Performance Shaders on macOS, but this does not guarantee lower latency for
  RNNT batch-1 decoder work.
- NeMo ASR docs: whole-file transcription through `model.transcribe()` is the
  supported simple API, which is what the offline sidecar uses today.
- Banfic et al. 2026, "Pushing the Limits of On-Device Streaming ASR":
  reports a Nemotron streaming ONNX Runtime path with int4 quantization,
  faster-than-real-time CPU inference, and 0.56 s algorithmic latency. That is
  the strongest research lead for sub-second local macOS dictation while
  keeping Nemotron.

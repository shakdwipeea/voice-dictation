# Nemotron ASR Phase 0 Harness

This directory contains the feasibility harness for the local
`nvidia/nemotron-speech-streaming-en-0.6b` backend.

The harness deliberately delegates model execution to NVIDIA NeMo's official
cache-aware streaming inference script. This keeps Phase 0 aligned with the
model's supported runtime while we measure which streaming profile should be
used by the application.

## Runtime Installation

Nemotron requires a working NVIDIA driver, CUDA-compatible PyTorch, and NVIDIA
NeMo. Follow the model card's installation instructions in an isolated Python
environment:

```bash
bash services/asr/setup_phase0_runtime.sh
```

Run the preflight before downloading the model:

```bash
.venv-nemotron/bin/python services/asr/nemotron_benchmark.py preflight
```

The Phase 0 setup recreates the isolated environment and installs
`torch==2.7.1+cu128` plus `cuda-python==12.8.0` before the tested NeMo commit.
Installing NeMo first leaves CUDA 13 packages behind even if PyTorch is later
replaced, which disables NeMo's fastest CUDA-graph path on the NVIDIA 570
driver. The setup also pins the Lightning and NVIDIA OneLogger dependency
bounds checked by `uv pip check`.

## Benchmark

The benchmark expects mono, 16 kHz, 16-bit PCM WAV files and invokes NeMo's
official streaming inference example pinned by the setup script. Run all
required Phase 0 profiles with:

```bash
make phase0-nemotron
```

The Make target downloads the exact pinned example into
`build/phase0/nemotron/nemo-source` and writes results into the same canonical
`build/phase0/nemotron` directory. Override `NEMOTRON_AUDIO` or
`NEMOTRON_MODEL` when needed.

The benchmark writes a JSON summary containing transcription output,
process-level elapsed time, streaming inference time, and real-time factor.
Supply `--reference` once per audio file when measuring WER; omitted references
do not generate a fake WER. Process-level timing includes model startup and is
not the final application's warm-stream latency. A persistent sidecar will be
implemented after Phase 0 proves the runtime is viable.

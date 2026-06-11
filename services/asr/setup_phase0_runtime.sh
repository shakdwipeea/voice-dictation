#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VENV="${VENV:-$ROOT/.venv-nemotron}"
UV_CACHE_DIR="${UV_CACHE_DIR:-/tmp/sunoto-uv-cache}"
NEMO_COMMIT="${NEMO_COMMIT:-c9040511b2dbefe64767d9b8853b3a20d63a2cd2}"
NEMO_SOURCE_ROOT="${NEMO_SOURCE_ROOT:-$ROOT/build/phase0/nemotron/nemo-source}"
NEMO_SCRIPT_DIR="$NEMO_SOURCE_ROOT/examples/asr/asr_cache_aware_streaming"
NEMO_SCRIPT_URL="https://raw.githubusercontent.com/NVIDIA/NeMo/$NEMO_COMMIT/examples/asr/asr_cache_aware_streaming/speech_to_text_cache_aware_streaming_infer.py"

export UV_CACHE_DIR

uv venv --clear --python 3.12 "$VENV"

# Install driver-compatible CUDA packages before NeMo so its unconstrained
# dependencies do not leave an unusable CUDA 13 runtime alongside PyTorch.
uv pip install \
  --python "$VENV/bin/python" \
  --index-url https://download.pytorch.org/whl/cu128 \
  "torch==2.7.1"
uv pip install \
  --python "$VENV/bin/python" \
  "cuda-python==12.8.0"
uv pip install \
  --python "$VENV/bin/python" \
  Cython \
  packaging \
  "nemo_toolkit[asr] @ git+https://github.com/NVIDIA/NeMo.git@$NEMO_COMMIT"
uv pip install \
  --python "$VENV/bin/python" \
  "fsspec[http]>=2022.5.0,<2026.0" \
  "setuptools>=79.0.0"

mkdir -p "$NEMO_SCRIPT_DIR"
curl --fail --location --retry 3 \
  --output "$NEMO_SCRIPT_DIR/speech_to_text_cache_aware_streaming_infer.py" \
  "$NEMO_SCRIPT_URL"

"$VENV/bin/python" "$ROOT/services/asr/nemotron_benchmark.py" preflight

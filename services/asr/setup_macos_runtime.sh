#!/usr/bin/env bash
# Set up the macOS NeMo runtime for the offline Nemotron ASR sidecar.
#
# Mirrors services/asr/setup_phase0_runtime.sh but for Apple Silicon:
#   - python 3.12 (NeMo's best-supported CPython on macOS),
#   - torch from the default PyPI index (MPS-enabled arm64 wheels),
#   - no CUDA / nvidia-smi / cuda-python,
#   - nemo_toolkit[asr] from the same pinned NeMo commit as the Linux runtime.
#
# Produces .venv-nemotron-mac. CPU/MPS only — no GPU/Xid concerns.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VENV="${VENV:-$ROOT/.venv-nemotron-mac}"
PYTHON="${PYTHON:-python3.12}"
NEMO_COMMIT="${NEMO_COMMIT:-c9040511b2dbefe64767d9b8853b3a20d63a2cd2}"

command -v "$PYTHON" >/dev/null || { echo "$PYTHON not found (brew install python@3.12)" >&2; exit 1; }

echo "== creating $VENV with $PYTHON =="
"$PYTHON" -m venv --clear "$VENV"

# Upgrade pip tooling inside the venv.
"$VENV/bin/python" -m pip install --upgrade pip setuptools wheel

# torch first, from the default index (macOS arm64 wheels are MPS-enabled).
# Pin to the same major line as the Linux runtime where practical.
"$VENV/bin/python" -m pip install "torch==2.7.1"

# NeMo ASR toolkit from the pinned commit (matches the Linux runtime).
"$VENV/bin/python" -m pip install \
  Cython packaging \
  "nemo_toolkit[asr] @ git+https://github.com/NVIDIA/NeMo.git@$NEMO_COMMIT"

# Runtime helpers used by the real-speech macOS benchmark:
# datasets downloads the tiny Hugging Face LibriSpeech sample; soundfile
# decodes/export WAVs without pulling audio work into the daemon.
"$VENV/bin/python" -m pip install \
  "fsspec[http]>=2022.5.0,<2026.0" \
  "setuptools>=79.0.0" \
  "datasets>=2.18" \
  "soundfile>=0.12"

echo "== verifying imports =="
"$VENV/bin/python" - <<'PY'
import torch, nemo.collections.asr as nemo_asr
print("torch", torch.__version__, "mps_available", torch.backends.mps.is_available())
print("nemo ok")
PY

echo "== done: $VENV =="
echo "Run:  $VENV/bin/python services/asr/phase0_macos_measure.py"

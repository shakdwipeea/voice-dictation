#!/usr/bin/env python3
"""Offline Parakeet-MLX ASR sidecar for macOS (Apple Silicon MLX/Metal).

Whole-utterance transcription for the macOS port using senstella/parakeet-mlx
and an MLX-converted NVIDIA Parakeet-TDT checkpoint. The daemon/IPC contract is
identical to the existing offline Nemotron backend:

  - ready on startup after model load + warmup,
  - session_started on start_session,
  - no partials while recording,
  - one final on finish_session,
  - error on failure.

The protocol and buffer plumbing are reused from nemotron_sidecar.py and
nemotron_offline_sidecar.py. The default path feeds buffered PCM directly to
parakeet-mlx's low-level `get_logmel + generate` API, avoiding temp WAVs and
ffmpeg on the hot path. The file-based `model.transcribe()` path remains only
for optional parakeet-mlx chunking experiments.

Important: parakeet-mlx loads MLX-converted Hugging Face repos such as
``mlx-community/parakeet-tdt-0.6b-v3``. It does not load raw NeMo checkpoints
such as ``nvidia/parakeet-tdt-0.6b-v2`` directly.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from array import array

from nemotron_offline_sidecar import SAMPLE_RATE, _OfflineBufferEngine
from nemotron_sidecar import SidecarServer, log, serve

BACKEND_NAME = "parakeet_mlx_offline"
DEFAULT_MODEL = "mlx-community/parakeet-tdt-0.6b-v3"


def _dtype_name(dtype) -> str:
    return getattr(dtype, "__name__", str(dtype))


class OfflineParakeetMlxEngine(_OfflineBufferEngine):
    """Loads Parakeet-MLX once and transcribes whole utterances on finish."""

    backend = BACKEND_NAME

    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        precision: str = "bf16",
        cache_dir: str | None = None,
        chunk_duration: float | None = None,
        overlap_duration: float = 15.0,
    ) -> None:
        super().__init__()
        self.model = None
        self._mx = None
        self._dtype = None
        self._get_logmel = None
        self.precision = precision
        self.chunk_duration = chunk_duration
        self.overlap_duration = overlap_duration
        self._load(model_name, precision, cache_dir)

    # ----- model load + warmup -------------------------------------------------

    def _load(self, model_name: str, precision: str, cache_dir: str | None) -> None:
        started = time.perf_counter()
        log("importing MLX + parakeet_mlx ...")
        import mlx.core as mx  # noqa: delayed; Apple Silicon runtime dependency
        from parakeet_mlx import from_pretrained  # noqa: delayed; optional in tests
        from parakeet_mlx.audio import get_logmel  # noqa: delayed; avoids ffmpeg

        self._mx = mx
        self._get_logmel = get_logmel
        if precision == "fp32":
            self._dtype = mx.float32
        elif precision == "bf16":
            self._dtype = mx.bfloat16
        else:  # argparse normally prevents this; keep the engine defensive.
            raise ValueError(f"unsupported precision {precision!r}; use bf16 or fp32")

        log(
            f"imports done in {time.perf_counter() - started:.1f}s; "
            f"mlx default_device={mx.default_device()}"
        )

        load_started = time.perf_counter()
        kwargs = {"dtype": self._dtype}
        if cache_dir:
            kwargs["cache_dir"] = os.path.expanduser(cache_dir)
        self.model = from_pretrained(model_name, **kwargs)
        log(
            f"model loaded: {model_name} precision={precision} "
            f"dtype={_dtype_name(self._dtype)} in {time.perf_counter() - load_started:.1f}s"
        )

        warm_started = time.perf_counter()
        self._warmup()
        log(
            f"warmup done in {time.perf_counter() - warm_started:.1f}s; "
            f"ready after {time.perf_counter() - started:.1f}s total"
        )

    def _warmup(self) -> None:
        """Transcribe ~1s of silence so the first real session is not cold."""
        try:
            self._transcribe_pcm_i16(array("h", [0] * SAMPLE_RATE))
        except Exception as error:  # noqa: BLE001
            log(f"warmup parakeet-mlx transcribe failed ({error!r}); continuing")

    # ----- transcription ------------------------------------------------------

    def finish(self) -> str:
        """Return one final transcript, using direct PCM unless chunking is enabled.

        The base offline engine writes a temp WAV before calling
        ``_transcribe_wav``. For the normal Parakeet-MLX path, the daemon has
        already delivered exactly the 16 kHz mono PCM that parakeet-mlx needs, so
        converting the buffer directly to an MLX array avoids WAV write + ffmpeg
        decode overhead. If ``--chunk-duration`` is set, fall back to
        ``model.transcribe(path)`` because parakeet-mlx's chunk stitching lives
        there.
        """
        if self.chunk_duration is not None:
            return super().finish()
        try:
            if len(self._buffer) == 0:
                return ""
            t0 = time.perf_counter()
            samples = array("h", self._buffer)
            text = self._transcribe_pcm_i16(samples)
            t_total = time.perf_counter() - t0
            n_samples = len(samples)
            duration_s = n_samples / SAMPLE_RATE
            log(
                f"finish: {n_samples} samples ({duration_s:.2f}s audio), "
                f"direct_pcm total {t_total*1000:.0f}ms"
            )
            return text or ""
        finally:
            self._buffer = array("h")

    def _transcribe_pcm_i16(self, samples: array) -> str:
        if self.model is None or self._mx is None or self._get_logmel is None:
            raise RuntimeError("Parakeet-MLX model is not loaded")
        t0 = time.perf_counter()
        audio = self._mx.array(samples, dtype=self._mx.float32) / 32768.0
        t_audio = time.perf_counter() - t0

        t_mel0 = time.perf_counter()
        mel = self._get_logmel(audio, self.model.preprocessor_config)
        t_mel = time.perf_counter() - t_mel0

        t_gen0 = time.perf_counter()
        results = self.model.generate(mel)
        t_generate = time.perf_counter() - t_gen0
        log(
            f"model.generate: audio_array {t_audio*1000:.0f}ms, "
            f"logmel {t_mel*1000:.0f}ms, decode {t_generate*1000:.0f}ms, "
            f"total {(time.perf_counter() - t0)*1000:.0f}ms"
        )
        if not results:
            return ""
        result = results[0]
        text = getattr(result, "text", result)
        return text if isinstance(text, str) else ""

    def _transcribe_wav(self, path: str) -> str:
        if self.model is None:
            raise RuntimeError("Parakeet-MLX model is not loaded")
        t0 = time.perf_counter()
        kwargs = {"dtype": self._dtype}
        if self.chunk_duration is not None:
            kwargs["chunk_duration"] = self.chunk_duration
            kwargs["overlap_duration"] = self.overlap_duration
        result = self.model.transcribe(path, **kwargs)
        t_model = time.perf_counter() - t0
        log(f"model.transcribe: {t_model*1000:.0f}ms")
        text = getattr(result, "text", result)
        return text if isinstance(text, str) else ""


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument(
        "--precision",
        default="bf16",
        choices=("bf16", "fp32"),
        help="MLX model/audio precision; bf16 is the parakeet-mlx default and benchmark winner",
    )
    parser.add_argument(
        "--cache-dir",
        default=None,
        help="optional Hugging Face cache directory for parakeet_mlx.from_pretrained()",
    )
    parser.add_argument(
        "--chunk-duration",
        type=float,
        default=None,
        help="optional parakeet-mlx chunk duration in seconds for long utterances",
    )
    parser.add_argument(
        "--overlap-duration",
        type=float,
        default=15.0,
        help="chunk overlap in seconds when --chunk-duration is set",
    )
    parser.add_argument(
        "--profile-ms",
        type=int,
        default=160,
        choices=sorted({80, 160, 560, 1120}),
        help="accepted for config parity with other backends; ignored in offline mode",
    )
    return parser


def main(argv=None) -> int:
    args = create_parser().parse_args(argv)

    # Reserve the real stdout for the protocol, then point fd 1 at stderr so
    # stray library prints cannot corrupt the NDJSON protocol stream. Mirrors
    # nemotron_sidecar.main and nemotron_offline_sidecar.main.
    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr

    engine = OfflineParakeetMlxEngine(
        model_name=args.model,
        precision=args.precision,
        cache_dir=args.cache_dir,
        chunk_duration=args.chunk_duration,
        overlap_duration=args.overlap_duration,
    )
    server = SidecarServer(engine)
    return serve(server, sys.stdin, protocol_out)


if __name__ == "__main__":
    raise SystemExit(main())

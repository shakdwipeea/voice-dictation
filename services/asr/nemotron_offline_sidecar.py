#!/usr/bin/env python3
"""Offline Nemotron ASR sidecar for macOS (CPU default, MPS override).

Whole-utterance transcription for the macOS port: audio_chunk samples are
buffered during a session and transcribed in one shot on finish_session.
This reuses the SidecarServer protocol layer from nemotron_sidecar.py so the
daemon/IPC contract is identical to the Linux streaming backend.

Why offline here (see docs/macos-port-plan.md): the streaming engine in
nemotron_sidecar.py is a cache-aware RNNT path tuned for CUDA
(conformer_stream_step, incremental mel, chunk/pre-cache state). On Apple
Silicon there is no CUDA, MPS op coverage for that streaming path is poor,
and batch=1 micro-steps are latency-bound so MPS would not help anyway.
Push-to-talk already provides the end-of-utterance signal, so we drop
streaming partials and call model.transcribe() once on release. The model
and weights are identical to the Linux backend; only the inference schedule
differs.

Protocol behavior:
  - ready on startup (after the warm load),
  - session_started on start_session,
  - NO partial events while recording (the overlay stays minimal: dot + meter),
  - one final on finish_session,
  - error on failure.

profile_ms is accepted (so configs port between Linux/macOS) but ignored:
there is no chunking in offline mode.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
import wave
from array import array

from nemotron_sidecar import (
    DEFAULT_MODEL,
    SidecarServer,
    log,
    serve,
)

BACKEND_NAME = "nemotron_offline"
SAMPLE_RATE = 16_000


class _OfflineBufferEngine:
    """Buffer + WAV-writing engine core, testable without NeMo.

    Subclasses implement ``_transcribe_wav(path)`` to run the actual model.
    The protocol contract (start/accept_audio/finish/cancel + ``backend``)
    matches what ``SidecarServer`` expects, so a fake subclass drives the
    protocol-layer tests identically to the real engine.
    """

    backend = BACKEND_NAME

    def __init__(self) -> None:
        self._buffer: array[int] = array("h")

    # ----- session lifecycle -------------------------------------------------

    def start(self, profile_ms: int) -> None:
        # profile_ms is accepted for config parity but ignored offline.
        self._buffer = array("h")

    def accept_audio(self, samples) -> list:
        """Append audio; emit no partials (offline mode).

        ``SidecarServer`` always converts the wire i16 samples to float32 in
        [-1, 1] (divide by 32768) before calling ``accept_audio`` — the same
        contract the streaming ``NemotronEngine`` relies on. We invert that
        here (multiply by 32768, round, clamp to int16) so the written WAV
        holds the original PCM. Callers must feed float32, not raw i16.
        """
        for s in samples:
            value = int(round(float(s) * 32768.0))
            if value > 32767:
                value = 32767
            elif value < -32768:
                value = -32768
            self._buffer.append(value)
        return []

    def finish(self) -> str:
        try:
            if len(self._buffer) == 0:
                return ""
            t0 = time.perf_counter()
            path = self._write_wav(self._buffer)
            t_write = time.perf_counter() - t0
            n_samples = len(self._buffer)
            duration_s = n_samples / SAMPLE_RATE
            try:
                text = self._transcribe_wav(path)
            finally:
                try:
                    os.unlink(path)
                except OSError:
                    pass
            t_total = time.perf_counter() - t0
            log(
                f"finish: {n_samples} samples ({duration_s:.2f}s audio), "
                f"wav_write {t_write*1000:.0f}ms, "
                f"total {t_total*1000:.0f}ms"
            )
            if text is None:
                return ""
            if not isinstance(text, str):
                text = getattr(text, "text", "") or ""
            return text
        finally:
            self._buffer = array("h")

    def cancel(self) -> None:
        self._buffer = array("h")

    # ----- to be provided by the model-loading subclass ----------------------

    def _transcribe_wav(self, path: str):  # pragma: no cover - overridden
        raise NotImplementedError

    # ----- helpers -----------------------------------------------------------

    @staticmethod
    def _write_wav(samples: array) -> str:
        """Write 16 kHz mono s16le PCM to a temp WAV file; return its path."""
        fd, path = tempfile.mkstemp(suffix=".wav", prefix="sunoto-offline-")
        with os.fdopen(fd, "wb") as fh:
            with wave.open(fh, "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(SAMPLE_RATE)
                wav.writeframes(samples.tobytes())
        return path


class OfflineNemotronEngine(_OfflineBufferEngine):
    """Loads Nemotron once (warm) and transcribes whole utterances.

    CPU is the default device because live short-utterance dictation has been
    more stable there. MPS remains available via ``--device mps`` and falls
    back to CPU when the model cannot be loaded there.
    """

    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        device: str = "cpu",
        use_lhotse: bool = False,
    ) -> None:
        super().__init__()
        self._torch = None
        self.model = None
        self.device = None
        self.device_name = device
        self.use_lhotse = use_lhotse
        self._load(model_name, device)

    # ----- model load + warmup -------------------------------------------------

    def _load(self, model_name: str, device: str) -> None:
        started = time.perf_counter()
        log("importing torch + NeMo ...")
        import torch  # noqa: delayed import; heavy and optional for the protocol layer
        import nemo.collections.asr as nemo_asr

        self._torch = torch
        torch.set_grad_enabled(False)
        try:
            torch.set_float32_matmul_precision("high")
        except Exception:  # noqa: BLE001 - setting is best-effort and may be ignored
            pass
        log(f"imports done in {time.perf_counter() - started:.1f}s")

        load_started = time.perf_counter()
        try:
            self.device = torch.device(device)
            self.model = nemo_asr.models.ASRModel.from_pretrained(
                model_name, map_location=self.device
            )
        except Exception as error:
            if device != "cpu":
                log(f"load on {device} failed ({error!r}); falling back to CPU")
                self.device = torch.device("cpu")
                self.model = nemo_asr.models.ASRModel.from_pretrained(
                    model_name, map_location=self.device
                )
                self.device_name = "cpu"
            else:
                raise
        self.model.eval()
        log(
            f"model loaded on {self.device} in "
            f"{time.perf_counter() - load_started:.1f}s"
        )

        warm_started = time.perf_counter()
        self._warmup()
        log(
            f"warmup done in {time.perf_counter() - warm_started:.1f}s; "
            f"ready after {time.perf_counter() - started:.1f}s total"
        )

    def _warmup(self) -> None:
        """Transcribe ~1s of silence so the first real session isn't a cold
        start. Failures are logged, not fatal: the daemon restarts the sidecar
        on crash, but a warmup blip should not take the sidecar down."""
        try:
            path = self._write_wav(array("h", [0] * SAMPLE_RATE))
            try:
                self.model.transcribe(
                    [path],
                    return_hypotheses=False,
                    use_lhotse=self.use_lhotse,
                    batch_size=1,
                    num_workers=0,
                    verbose=False,
                )
            finally:
                try:
                    os.unlink(path)
                except OSError:
                    pass
        except Exception as error:  # noqa: BLE001
            log(f"warmup transcribe failed ({error!r}); continuing")

    # ----- transcription ------------------------------------------------------

    def _transcribe_wav(self, path: str) -> str:
        t0 = time.perf_counter()
        results = self.model.transcribe(
            [path],
            return_hypotheses=False,
            use_lhotse=self.use_lhotse,
            batch_size=1,
            num_workers=0,
            verbose=False,
        )
        t_model = time.perf_counter() - t0
        log(f"model.transcribe: {t_model*1000:.0f}ms")
        if not results:
            return ""
        text = results[0]
        return text if isinstance(text, str) else getattr(text, "text", "") or ""


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument(
        "--device",
        default="cpu",
        help="torch device: cpu (default) or mps. MPS falls back to cpu on load errors.",
    )
    parser.add_argument(
        "--use-lhotse",
        action="store_true",
        help="use NeMo's Lhotse dataloader path during transcribe(); default is off "
        "to reduce single-utterance overhead and log churn",
    )
    parser.add_argument(
        "--profile-ms",
        type=int,
        default=160,
        choices=sorted({80, 160, 560, 1120}),
        help="accepted for config parity with the streaming backend; "
        "ignored in offline mode (no chunking)",
    )
    return parser


def main(argv=None) -> int:
    args = create_parser().parse_args(argv)

    # Reserve the real stdout for the protocol, then point fd 1 at stderr so
    # any stray library print (NeMo logs INFO to stdout) is diverted. Mirrors
    # nemotron_sidecar.main.
    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr

    engine = OfflineNemotronEngine(
        model_name=args.model, device=args.device, use_lhotse=args.use_lhotse
    )
    server = SidecarServer(engine)
    return serve(server, sys.stdin, protocol_out)


if __name__ == "__main__":
    raise SystemExit(main())

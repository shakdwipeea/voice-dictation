#!/usr/bin/env python3
"""Streaming Parakeet-MLX ASR sidecar for macOS (Apple Silicon MLX/Metal).

This backend uses parakeet-mlx's real-time ``transcribe_stream`` API. Unlike the
offline Parakeet backend, it does not write temp WAVs and it does not call the
file-based ``model.transcribe(path)`` path. Audio arrives from the daemon as
16 kHz mono i16 PCM, SidecarServer converts it to float32 in [-1, 1], and this
engine feeds buffered chunks directly to ``transcriber.add_audio()``. On
``finish_session`` the default final transcript is generated from the full
buffered utterance via parakeet-mlx's low-level ``get_logmel()`` +
``model.generate()`` API. That keeps streaming partials but restores offline-like
final accuracy without temp WAVs or ffmpeg.

Protocol behavior:
  - ready on startup after model load + warmup,
  - session_started on start_session,
  - partial events while recording when the stream result changes,
  - one final on finish_session,
  - error on failure.

The stable production fallback remains the separate ``parakeet_mlx_offline``
backend. This backend is pure in-memory MLX: streaming partials plus direct PCM
final, no file-based speech generation path.
"""

from __future__ import annotations

import argparse
import os
import sys
import time

from nemotron_sidecar import PROFILE_CONTEXTS, SidecarServer, log, serve
from parakeet_mlx_offline_sidecar import DEFAULT_MODEL, _dtype_name

BACKEND_NAME = "parakeet_mlx_streaming"
SAMPLE_RATE = 16_000


def _positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be > 0")
    return parsed


def env_bool(name: str, default: bool = False) -> bool:
    value = os.environ.get(name)
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "on"}


class StreamingParakeetMlxEngine:
    """Loads Parakeet-MLX once and streams per push-to-talk session."""

    backend = BACKEND_NAME

    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        precision: str = "bf16",
        cache_dir: str | None = None,
        chunk_ms: int = 560,
        flush_ms: int = 0,
        min_final_ms: int = 160,
        left_context: int = 256,
        right_context: int = 256,
        depth: int = 1,
        keep_original_attention: bool = False,
        final_mode: str = "direct",
    ) -> None:
        self.model = None
        self._mx = None
        self._dtype = None
        self._get_logmel = None
        self.precision = precision
        self.chunk_samples = max(1, SAMPLE_RATE * chunk_ms // 1000)
        self.flush_samples = max(0, SAMPLE_RATE * flush_ms // 1000)
        self.min_final_samples = max(1, SAMPLE_RATE * min_final_ms // 1000)
        self.context_size = (left_context, right_context)
        self.depth = depth
        self.keep_original_attention = keep_original_attention
        if final_mode not in {"direct", "streaming"}:
            raise ValueError("final_mode must be 'direct' or 'streaming'")
        self.final_mode = final_mode

        self._stream_cm = None
        self._transcriber = None
        self._pending: list[float] = []
        self._pending_len = 0
        self._all_audio: list[float] = []
        self._last_text = ""
        self._last_good_text = ""
        self._seen_audio = False
        self._chunk_count = 0

        self._load(model_name, precision, cache_dir)

    # ----- model load + warmup -------------------------------------------------

    def _load(self, model_name: str, precision: str, cache_dir: str | None) -> None:
        started = time.perf_counter()
        log("importing MLX + parakeet_mlx streaming ...")
        import mlx.core as mx  # noqa: delayed; Apple Silicon runtime dependency
        from parakeet_mlx import from_pretrained  # noqa: delayed; optional in tests
        from parakeet_mlx.audio import get_logmel  # noqa: delayed; no ffmpeg

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
        log(
            f"stream config: chunk={self.chunk_samples / SAMPLE_RATE:.3f}s, "
            f"flush={self.flush_samples / SAMPLE_RATE:.3f}s, "
            f"min_final={self.min_final_samples / SAMPLE_RATE:.3f}s, "
            f"context={self.context_size}, depth={self.depth}, "
            f"keep_original_attention={self.keep_original_attention}, "
            f"final_mode={self.final_mode}"
        )

        warm_started = time.perf_counter()
        self._warmup()
        log(
            f"warmup done in {time.perf_counter() - warm_started:.1f}s; "
            f"ready after {time.perf_counter() - started:.1f}s total"
        )

    def _warmup(self) -> None:
        """Run one tiny streaming context so the first real session is warm."""
        try:
            self._open_stream()
            self._add_audio_to_stream([0.0] * SAMPLE_RATE, reason="warmup")
        except Exception as error:  # noqa: BLE001
            log(f"warmup parakeet-mlx streaming failed ({error!r}); continuing")
        finally:
            self._close_stream()

    # ----- session lifecycle --------------------------------------------------

    def start(self, profile_ms: int) -> None:
        # profile_ms is accepted for daemon/config parity. Chunk/context tuning is
        # exposed through this backend's own CLI flags because Parakeet-MLX's
        # streaming context is not the same concept as Nemotron's latency profile.
        if profile_ms not in PROFILE_CONTEXTS:
            raise ValueError(f"unsupported profile_ms: {profile_ms}")
        self.cancel()
        self._pending = []
        self._pending_len = 0
        self._all_audio = []
        self._last_text = ""
        self._last_good_text = ""
        self._seen_audio = False
        self._chunk_count = 0
        self._open_stream()

    def accept_audio(self, samples) -> list[str]:
        if self._transcriber is None:
            raise RuntimeError("no active streaming session")
        if len(samples) == 0:
            return []
        self._append_pending(samples)
        self._seen_audio = True
        events = []
        while self._pending_len >= self.chunk_samples:
            chunk = self._take_pending(self.chunk_samples)
            text = self._add_audio_to_stream(chunk, reason="partial")
            if text != self._last_text:
                self._last_text = text
                if text:
                    events.append(text)
        return events

    def finish(self) -> str:
        if self._transcriber is None:
            raise RuntimeError("no active streaming session")
        t0 = time.perf_counter()
        try:
            if not self._seen_audio and self._pending_len == 0:
                return ""
            if self._pending_len > 0:
                if self._chunk_count == 0 or self._pending_len >= self.min_final_samples:
                    self._add_audio_to_stream(self._take_pending(self._pending_len), reason="final")
                else:
                    log(
                        f"finish: dropping tiny streaming tail {self._pending_len} samples "
                        f"({self._pending_len / SAMPLE_RATE:.3f}s)"
                    )
                    self._pending = []
                    self._pending_len = 0
            if self.flush_samples > 0:
                self._add_audio_to_stream([0.0] * self.flush_samples, reason="flush")
            streaming_text = self._current_text()
            if self.final_mode == "direct":
                text = self._transcribe_direct_pcm(self._all_audio)
                if self._is_degenerate(text):
                    log("finish: direct final text was degenerate; using streaming fallback")
                    text = streaming_text
            else:
                text = streaming_text
            if self._is_degenerate(text) and self._last_good_text:
                log("finish: final stream text was degenerate; using last good partial")
                text = self._last_good_text
            sync_ms = self._synchronize_final_if_requested()
            sync_part = f"mlx_sync={sync_ms:.0f}ms, " if sync_ms is not None else ""
            log(
                f"finish: streaming chunks={self._chunk_count}, "
                f"final_text_chars={len(text)}, {sync_part}"
                f"total {(time.perf_counter() - t0)*1000:.0f}ms"
            )
            return text
        finally:
            self._close_stream()
            self._pending = []
            self._pending_len = 0
            self._all_audio = []
            self._last_text = ""
            self._last_good_text = ""
            self._seen_audio = False
            self._chunk_count = 0

    def _synchronize_final_if_requested(self) -> float | None:
        if not env_bool("SUNOTO_ASR_MLX_SYNCHRONIZE_FINAL"):
            return None
        synchronize = getattr(self._mx, "synchronize", None)
        if not callable(synchronize):
            log("finish: MLX final synchronize requested but unavailable")
            return None
        started = time.perf_counter()
        synchronize()
        elapsed_ms = (time.perf_counter() - started) * 1000
        log(f"finish: MLX final synchronize {elapsed_ms:.0f}ms")
        return elapsed_ms

    def cancel(self) -> None:
        self._close_stream()
        self._pending = []
        self._pending_len = 0
        self._all_audio = []
        self._last_text = ""
        self._last_good_text = ""
        self._seen_audio = False
        self._chunk_count = 0

    # ----- streaming internals ------------------------------------------------

    def _open_stream(self) -> None:
        if self.model is None:
            raise RuntimeError("Parakeet-MLX model is not loaded")
        self._stream_cm = self.model.transcribe_stream(
            context_size=self.context_size,
            depth=self.depth,
            keep_original_attention=self.keep_original_attention,
        )
        self._transcriber = self._stream_cm.__enter__()

    def _close_stream(self) -> None:
        if self._stream_cm is not None:
            try:
                self._stream_cm.__exit__(None, None, None)
            finally:
                self._stream_cm = None
                self._transcriber = None

    def _append_pending(self, samples) -> None:
        # SidecarServer already converted i16 PCM to float32 in [-1, 1]. Keep a
        # plain list so this module remains importable without numpy.
        values = [float(sample) for sample in samples]
        self._pending.extend(values)
        self._pending_len += len(values)
        self._all_audio.extend(values)

    def _take_pending(self, count: int) -> list[float]:
        chunk = self._pending[:count]
        del self._pending[:count]
        self._pending_len -= len(chunk)
        return chunk

    def _add_audio_to_stream(self, samples, *, reason: str) -> str:
        if self._mx is None or self._transcriber is None:
            raise RuntimeError("no active streaming session")
        t0 = time.perf_counter()
        audio = self._mx.array(samples, dtype=self._mx.float32)
        self._transcriber.add_audio(audio)
        self._chunk_count += 1
        text = self._current_text()
        if text and not self._is_degenerate(text):
            self._last_good_text = text
        elapsed_ms = (time.perf_counter() - t0) * 1000
        if reason != "partial" or text != self._last_text:
            log(
                f"stream {reason}: {len(samples)} samples "
                f"({len(samples) / SAMPLE_RATE:.3f}s), {elapsed_ms:.0f}ms, "
                f"text_chars={len(text)}"
            )
        return text

    def _transcribe_direct_pcm(self, samples: list[float]) -> str:
        if self.model is None or self._mx is None or self._get_logmel is None:
            raise RuntimeError("Parakeet-MLX model is not loaded")
        if not samples:
            return ""
        t0 = time.perf_counter()
        audio = self._mx.array(samples, dtype=self._mx.float32)
        t_audio = time.perf_counter() - t0

        t_mel0 = time.perf_counter()
        mel = self._get_logmel(audio, self.model.preprocessor_config)
        t_mel = time.perf_counter() - t_mel0

        t_gen0 = time.perf_counter()
        results = self.model.generate(mel)
        t_generate = time.perf_counter() - t_gen0
        log(
            f"direct final: audio_array {t_audio*1000:.0f}ms, "
            f"logmel {t_mel*1000:.0f}ms, decode {t_generate*1000:.0f}ms, "
            f"total {(time.perf_counter() - t0)*1000:.0f}ms"
        )
        if not results:
            return ""
        result = results[0]
        text = getattr(result, "text", result)
        return text if isinstance(text, str) else ""

    def _current_text(self) -> str:
        result = self._transcriber.result if self._transcriber is not None else None
        text = getattr(result, "text", "") if result is not None else ""
        return text if isinstance(text, str) else ""

    @staticmethod
    def _is_degenerate(text: str) -> bool:
        if not text:
            return True
        # Current parakeet-mlx streaming can emit a pathological repeated-<unk>
        # tail if asked to decode tiny residual chunks or trailing silence.
        # Treat that as unusable and keep the last sane partial/final instead.
        return text.count("<unk>") >= 3 and text.count("<unk>") * 5 >= len(text) // 2


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument(
        "--precision",
        default="bf16",
        choices=("bf16", "fp32"),
        help="MLX model precision; bf16 is the parakeet-mlx default",
    )
    parser.add_argument(
        "--cache-dir",
        default=None,
        help="optional Hugging Face cache directory for parakeet_mlx.from_pretrained()",
    )
    parser.add_argument(
        "--profile-ms",
        type=int,
        default=560,
        choices=sorted(PROFILE_CONTEXTS),
        help="accepted for daemon config parity; macOS default is 560ms",
    )
    parser.add_argument(
        "--chunk-ms",
        type=_positive_int,
        default=560,
        help="audio accumulated before each transcriber.add_audio() call",
    )
    parser.add_argument(
        "--flush-ms",
        type=int,
        default=0,
        help="trailing silence added on finish; disabled by default because current parakeet-mlx streaming can emit <unk> on silence",
    )
    parser.add_argument(
        "--min-final-ms",
        type=_positive_int,
        default=160,
        help="drop a smaller residual tail on finish after at least one streaming chunk",
    )
    parser.add_argument("--left-context", type=_positive_int, default=256)
    parser.add_argument("--right-context", type=_positive_int, default=256)
    parser.add_argument("--depth", type=_positive_int, default=1)
    parser.add_argument(
        "--keep-original-attention",
        action="store_true",
        help="keep full attention during streaming; default switches to local attention",
    )
    parser.add_argument(
        "--final-mode",
        choices=("direct", "streaming"),
        default="direct",
        help="direct = full-utterance get_logmel+generate final (default); streaming = final from transcribe_stream state",
    )
    return parser


def main(argv=None) -> int:
    args = create_parser().parse_args(argv)
    if args.flush_ms < 0:
        raise ValueError("--flush-ms must be >= 0")

    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr

    engine = StreamingParakeetMlxEngine(
        model_name=args.model,
        precision=args.precision,
        cache_dir=args.cache_dir,
        chunk_ms=args.chunk_ms,
        flush_ms=args.flush_ms,
        min_final_ms=args.min_final_ms,
        left_context=args.left_context,
        right_context=args.right_context,
        depth=args.depth,
        keep_original_attention=args.keep_original_attention,
        final_mode=args.final_mode,
    )
    server = SidecarServer(engine)
    return serve(server, sys.stdin, protocol_out)


if __name__ == "__main__":
    raise SystemExit(main())

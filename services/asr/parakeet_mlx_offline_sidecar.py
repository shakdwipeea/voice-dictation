#!/usr/bin/env python3
"""Offline Parakeet-MLX ASR sidecar for macOS (Apple Silicon MLX/Metal).

Whole-utterance-quality transcription for the macOS port using
senstella/parakeet-mlx and an MLX-converted NVIDIA Parakeet-TDT checkpoint. The
daemon/IPC contract is identical to the existing offline Nemotron backend:

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

Long dictations are transcribed in pieces while the user is still talking.
Once the untranscribed audio passes ``--segment-min-s``, the engine cuts it in
the middle of the next clear pause (or at the quietest moment once it reaches
``--segment-max-s``) and a background worker transcribes that piece. Each
piece re-hears ``--segment-overlap-s`` of audio before its cut, so words at the
seam keep their context, and the token streams are joined with parakeet-mlx's
own overlap merge (the one its chunked ``transcribe()`` uses). On release only
the last piece is left, so the wait after release stays about the same however
long the dictation ran, and only that piece's audio is kept in memory.
Dictations shorter than the minimum get one whole-utterance pass, as before.

The Apple GPU clocks down after a couple of idle seconds, and the first decode
after that pays a fixed ~0.6 s to wake it, which is most of the wait after a
tap. So when the speaker pauses for 200 ms, the engine starts decoding the open
audio right away (``--no-pause-decode`` turns this off). If the tap then comes
with nothing said after the pause, that decode is the last piece and the wake
up already happened during the pause. If speech resumes, it is discarded.

Important: parakeet-mlx loads MLX-converted Hugging Face repos such as
``mlx-community/parakeet-tdt-0.6b-v3``. It does not load raw NeMo checkpoints
such as ``nvidia/parakeet-tdt-0.6b-v2`` directly.
"""

from __future__ import annotations

import argparse
import math
import os
import sys
import time
from array import array
from collections import namedtuple
from concurrent.futures import ThreadPoolExecutor

import word_boost
from nemotron_offline_sidecar import SAMPLE_RATE, _OfflineBufferEngine
from nemotron_sidecar import SidecarServer, log, serve

BACKEND_NAME = "parakeet_mlx_offline"
DEFAULT_MODEL = "mlx-community/parakeet-tdt-0.6b-v3"

DEFAULT_SEGMENT_MIN_S = 10.0
DEFAULT_SEGMENT_MAX_S = 20.0
DEFAULT_SEGMENT_OVERLAP_S = 3.0
# Pause detection reads the loudness of 20 ms frames (the daemon's frame size).
FRAME_SAMPLES = SAMPLE_RATE // 50
PAUSE_FRAMES = 15  # 300 ms: longer than the gaps between words in a phrase
# A frame is quiet below this share of the piece's loud (90th percentile)
# frames, or below the floor when the whole piece is soft.
QUIET_RATIO = 0.2
QUIET_FLOOR_RMS = 60.0
# Looking for a cut costs a scan of the open piece; doing it every 40 ms
# instead of every 20 ms frame keeps the reader thread mostly idle.
CHECK_FRAMES = 2
# Silence after speech this long starts a pause decode: longer than most gaps
# between words, short enough to start well before a tap.
PAUSE_DECODE_FRAMES = 10  # 200 ms
# The last 200 ms before the tap can hold the key's click; it never holds a
# whole word, so neither reuse nor "speech resumed" looks at it.
TAP_GUARD_FRAMES = 10
# MLX keeps freed GPU buffers for reuse, by default up to the whole memory
# limit. Every piece has a different length, so without a cap the cache grows
# piece after piece (a 66 s dictation peaked at 4 GB, 2.7 GB over the weights).
MLX_CACHE_LIMIT_BYTES = 256 * 1024 * 1024


# A decode started at a pause: reusable as the last piece while the open piece
# still starts at `segment_start` and nothing after `end` is louder than `quiet`.
# `future` is None once it became a piece and the open piece is that pause.
PauseDecode = namedtuple("PauseDecode", "future segment_start end quiet")


def _dtype_name(dtype) -> str:
    return getattr(dtype, "__name__", str(dtype))


def find_cut(
    levels, first: int, last: int, threshold: float, force: bool
) -> int | None:
    """Pick the frame to cut at among windows of PAUSE_FRAMES in [first, last).

    A window is a pause when every frame in it is at or below ``threshold``;
    the quietest pause wins and the cut lands in its middle. With ``force``
    the quietest window wins even when it is not a pause. Returns None when
    there is no window, or no pause and no ``force``.
    """
    if last - first < PAUSE_FRAMES:
        return None
    total = 0.0
    loud = 0
    best_pause = None  # (sum, start)
    best_any = None
    for frame in range(first, last):
        level = levels[frame]
        total += level
        loud += level > threshold
        start = frame - PAUSE_FRAMES + 1
        if start > first:
            dropped = levels[start - 1]
            total -= dropped
            loud -= dropped > threshold
        if start < first:
            continue
        if loud == 0 and (best_pause is None or total < best_pause[0]):
            best_pause = (total, start)
        if best_any is None or total < best_any[0]:
            best_any = (total, start)
    chosen = best_pause or (best_any if force else None)
    return None if chosen is None else chosen[1] + PAUSE_FRAMES // 2


class OfflineParakeetMlxEngine(_OfflineBufferEngine):
    """Loads Parakeet-MLX once; transcribes long dictations piece by piece."""

    backend = BACKEND_NAME

    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        precision: str = "bf16",
        cache_dir: str | None = None,
        chunk_duration: float | None = None,
        overlap_duration: float = 15.0,
        segment_min_s: float = DEFAULT_SEGMENT_MIN_S,
        segment_max_s: float = DEFAULT_SEGMENT_MAX_S,
        segment_overlap_s: float = DEFAULT_SEGMENT_OVERLAP_S,
        pause_decode: bool = True,
    ) -> None:
        super().__init__()
        self.model = None
        self._mx = None
        self._dtype = None
        self._get_logmel = None
        self._token_index = None
        self._booster = None  # this dictation's words; touched on the worker only
        self.precision = precision
        self.chunk_duration = chunk_duration
        self.overlap_duration = overlap_duration
        # parakeet-mlx's own chunking (the WAV path) replaces live pieces.
        self.segment_min_samples = (
            int(segment_min_s * SAMPLE_RATE) if chunk_duration is None else 0
        )
        self.segment_max_samples = int(segment_max_s * SAMPLE_RATE)
        self.segment_overlap_s = segment_overlap_s
        self.segment_overlap_samples = int(segment_overlap_s * SAMPLE_RATE)
        self.pause_decode = pause_decode and chunk_duration is None
        # Every MLX call runs on this one thread, so a piece decodes while the
        # protocol thread keeps reading audio.
        self._worker = ThreadPoolExecutor(max_workers=1, thread_name_prefix="parakeet-mlx")
        self._reset_session()
        self._worker.submit(self._load, model_name, precision, cache_dir).result()

    # ----- model load + warmup -------------------------------------------------

    def _load(self, model_name: str, precision: str, cache_dir: str | None) -> None:
        started = time.perf_counter()
        log("importing MLX + parakeet_mlx ...")
        import mlx.core as mx  # noqa: delayed; Apple Silicon runtime dependency
        from parakeet_mlx import from_pretrained  # noqa: delayed; optional in tests
        from parakeet_mlx.audio import get_logmel  # noqa: delayed; avoids ffmpeg

        self._mx = mx
        self._get_logmel = get_logmel
        mx.set_cache_limit(MLX_CACHE_LIMIT_BYTES)
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
        self._token_index = word_boost.TokenIndex(self.model.vocabulary)
        log(
            f"model loaded: {model_name} precision={precision} "
            f"dtype={_dtype_name(self._dtype)} in {time.perf_counter() - load_started:.1f}s"
        )

        warm_started = time.perf_counter()
        self._warmup()
        self._release_cache()
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

    # ----- session lifecycle --------------------------------------------------

    def _reset_session(self) -> None:
        self._buffer = array("h")
        self._buffer_start = 0  # session sample index of _buffer[0]
        self._levels = array("f")  # RMS of every complete 20 ms frame
        self._checked_frames = 0
        self._segment_start = 0  # where the untranscribed audio begins
        self._pieces = []  # futures of (text, tokens), in audio order
        self._pause = None  # the latest PauseDecode, if any

    def start(self, profile_ms: int) -> None:
        # profile_ms is accepted for config parity but ignored offline.
        self.cancel()
        self._worker.submit(self._set_booster, None)

    def set_words(self, words: dict) -> None:
        """Favour these words in this dictation's decodes (see word_boost)."""
        self._worker.submit(self._set_booster, words)

    def _set_booster(self, words) -> None:
        if not words or self._token_index is None:
            self._booster = None
            return
        t0 = time.perf_counter()
        booster = word_boost.WordBooster(self._token_index, words)
        self._booster = booster or None
        log(
            f"word list: {len(booster.targets)} words, "
            f"{len(booster.start_bonus)} start pieces, "
            f"built in {(time.perf_counter() - t0) * 1000:.1f}ms"
        )

    def accept_audio(self, samples) -> list:
        super().accept_audio(samples)
        if self.segment_min_samples > 0 or self.pause_decode:
            self._measure_new_frames()
            if len(self._levels) - self._checked_frames >= CHECK_FRAMES:
                self._checked_frames = len(self._levels)
                self._check_open_piece()
        return []

    def cancel(self) -> None:
        # A piece already decoding finishes on the worker; its result is dropped.
        for piece in self._pieces:
            piece.cancel()
        if self._pause is not None:
            self._pause.future.cancel()
        self._reset_session()

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
            return self._worker.submit(super().finish).result()
        try:
            end = self._buffer_start + len(self._buffer)
            if end == 0:
                return ""
            t0 = time.perf_counter()
            reused = self._pause_covers_tail()
            if reused:
                last = self._pause.future  # None: the tap came in a decoded pause
            else:
                start = max(self._buffer_start, self._segment_start - self.segment_overlap_samples)
                tail = self._buffer[start - self._buffer_start :]
                last = self._worker.submit(
                    self._decode_piece, f"piece {len(self._pieces) + 1}", tail, start
                )
            pieces = self._pieces + ([last] if last is not None else [])
            results = [piece.result() for piece in pieces]
            if len(results) == 1:
                text = results[0][0]
            else:
                tokens = results[0][1]
                for _text, more in results[1:]:
                    tokens = self._merge_tokens(tokens, more)
                text = self._tokens_text(tokens)
            if self._booster is not None and text:
                text = self._booster.respell(text)
            log(
                f"finish: {end} samples ({end / SAMPLE_RATE:.2f}s audio), "
                f"{len(pieces)} piece(s), last one "
                f"{'from the pause decode' if reused else 'decoded after the tap'}, "
                f"release to text {(time.perf_counter() - t0) * 1000:.0f}ms"
            )
            return text or ""
        finally:
            self._reset_session()
            self._worker.submit(self._release_cache)

    # ----- pieces of a long dictation ----------------------------------------

    def _measure_new_frames(self) -> None:
        end = self._buffer_start + len(self._buffer)
        for frame in range(len(self._levels), end // FRAME_SAMPLES):
            lo = frame * FRAME_SAMPLES - self._buffer_start
            chunk = self._buffer[lo : lo + FRAME_SAMPLES]
            self._levels.append(math.sqrt(math.sumprod(chunk, chunk) / FRAME_SAMPLES))

    def _check_open_piece(self) -> None:
        piece_levels = sorted(self._levels[self._segment_start // FRAME_SAMPLES :])
        if not piece_levels:
            return
        threshold = max(QUIET_FLOOR_RMS, QUIET_RATIO * piece_levels[int(len(piece_levels) * 0.9)])
        if self.segment_min_samples > 0:
            pause = self._open_pause()
            # The newest 200 ms may be the click of the key about to stop the
            # recording; speech that really resumed is still there 200 ms on.
            heard = self._levels[: len(self._levels) - TAP_GUARD_FRAMES]
            resumed = pause is None or any(
                level > threshold for level in heard[-(-pause.end // FRAME_SAMPLES) :]
            )
            end = self._buffer_start + len(self._buffer)
            if pause is not None and pause.future is not None:
                long_enough = pause.end - self._segment_start >= self.segment_min_samples
                if (resumed and long_enough) or (
                    not resumed and end - self._segment_start >= self.segment_max_samples
                ):
                    # The pause decode ends inside a pause, so it is the next
                    # piece as is. Still silent at the max length: keep its
                    # quiet level so a tap now needs no further decode.
                    self._commit(pause.end, pause.future, None if resumed else pause.quiet)
                    return
            if resumed:
                # While the speaker stays silent after a pause decode, a cut
                # would only throw away the decode a tap is about to reuse.
                cut = self._choose_cut(threshold)
                if cut is not None:
                    self._commit(cut)
                    return
        if self.pause_decode:
            self._decode_at_pause(threshold)

    def _open_pause(self):
        """The pause decode for the open piece, if there is one."""
        pause = self._pause
        if pause is None or pause.segment_start != self._segment_start:
            return None
        return pause

    def _choose_cut(self, threshold: float) -> int | None:
        """Return the sample to cut the open piece at, or None to keep going."""
        end = self._buffer_start + len(self._buffer)
        pending = end - self._segment_start
        if pending < self.segment_min_samples:
            return None
        frame = find_cut(
            self._levels,
            first=(self._segment_start + self.segment_min_samples // 2) // FRAME_SAMPLES,
            last=len(self._levels),
            threshold=threshold,
            force=pending >= self.segment_max_samples,
        )
        return None if frame is None else frame * FRAME_SAMPLES

    def _decode_at_pause(self, threshold: float) -> None:
        """Start decoding the open audio when the speaker has just paused."""
        pause = self._open_pause()
        if pause is not None and pause.future is not None and not pause.future.done():
            return  # one at a time; the worker would only queue them up
        now = len(self._levels)
        paused_from = now - PAUSE_DECODE_FRAMES
        # Only speech after the last pause decode counts.
        heard_from = (pause.end if pause is not None else self._segment_start) // FRAME_SAMPLES
        if paused_from <= heard_from:
            return
        silence = self._levels[paused_from:now]
        if max(silence) > threshold or max(self._levels[heard_from:paused_from]) <= threshold:
            return
        end = self._buffer_start + len(self._buffer)
        start = max(self._buffer_start, self._segment_start - self.segment_overlap_samples)
        future = self._worker.submit(
            self._decode_piece, "pause decode", self._buffer[start - self._buffer_start :], start
        )
        # Reuse needs the rest to sound like this pause, not merely below the
        # speech threshold: a soft last word must still be heard.
        quiet = max(QUIET_FLOOR_RMS, min(threshold, 3 * sum(silence) / len(silence)))
        self._pause = PauseDecode(future, self._segment_start, end, quiet)

    def _pause_covers_tail(self) -> bool:
        pause = self._open_pause()
        if pause is None:
            return False
        after = self._levels[-(-pause.end // FRAME_SAMPLES) : len(self._levels) - TAP_GUARD_FRAMES]
        return all(level <= pause.quiet for level in after)

    def _commit(self, cut: int, decoded=None, quiet: float | None = None) -> None:
        """Close the open piece at `cut`, decoding it unless `decoded` already is."""
        if decoded is None:
            start = max(self._buffer_start, self._segment_start - self.segment_overlap_samples)
            piece = self._buffer[start - self._buffer_start : cut - self._buffer_start]
            label = f"piece {len(self._pieces) + 1}"
            decoded = self._worker.submit(self._decode_piece, label, piece, start)
        self._pieces.append(decoded)
        self._segment_start = cut
        self._pause = None if quiet is None else PauseDecode(None, cut, cut, quiet)
        # Later pieces only re-hear the overlap before this cut.
        keep_from = cut - self.segment_overlap_samples
        if keep_from > self._buffer_start:
            del self._buffer[: keep_from - self._buffer_start]
            self._buffer_start = keep_from

    def _decode_piece(self, label: str, samples: array, start: int):
        t0 = time.perf_counter()
        decoded = self._decode_pcm_i16(samples, start / SAMPLE_RATE)
        log(
            f"{label}: {start / SAMPLE_RATE:.1f}-"
            f"{(start + len(samples)) / SAMPLE_RATE:.1f}s decoded in "
            f"{(time.perf_counter() - t0) * 1000:.0f}ms"
        )
        return decoded

    def _merge_tokens(self, tokens, more):
        from parakeet_mlx.alignment import (  # noqa: delayed; optional in tests
            merge_longest_common_subsequence,
            merge_longest_contiguous,
        )

        try:
            return merge_longest_contiguous(
                tokens, more, overlap_duration=self.segment_overlap_s
            )
        except RuntimeError:
            return merge_longest_common_subsequence(
                tokens, more, overlap_duration=self.segment_overlap_s
            )

    @staticmethod
    def _tokens_text(tokens) -> str:
        from parakeet_mlx.alignment import (  # noqa: delayed; optional in tests
            sentences_to_result,
            tokens_to_sentences,
        )

        return sentences_to_result(tokens_to_sentences(tokens)).text

    def _release_cache(self) -> None:
        # Hand the decode's scratch buffers back to the system between
        # dictations, as parakeet-mlx's streaming context does on exit.
        if self._mx is not None:
            self._mx.clear_cache()

    # ----- transcription ------------------------------------------------------

    def _transcribe_pcm_i16(self, samples: array) -> str:
        return self._decode_pcm_i16(samples, 0.0)[0]

    def _decode_pcm_i16(self, samples: array, offset_s: float):
        """Return (text, tokens) for i16 PCM, token times shifted by offset_s."""
        if self.model is None or self._mx is None or self._get_logmel is None:
            raise RuntimeError("Parakeet-MLX model is not loaded")
        t0 = time.perf_counter()
        audio = self._mx.array(memoryview(samples)).astype(self._mx.float32) / 32768.0
        t_audio = time.perf_counter() - t0

        t_mel0 = time.perf_counter()
        mel = self._get_logmel(audio, self.model.preprocessor_config)
        t_mel = time.perf_counter() - t_mel0

        t_gen0 = time.perf_counter()
        booster = self._booster
        if booster is None:
            results = self.model.generate(mel)
        else:
            results = [word_boost.generate(self.model, self._mx, mel, booster)]
        t_generate = time.perf_counter() - t_gen0
        log(
            f"model.generate: audio_array {t_audio*1000:.0f}ms, "
            f"logmel {t_mel*1000:.0f}ms, decode {t_generate*1000:.0f}ms"
            f"{f' with {len(booster.targets)} words' if booster else ''}, "
            f"total {(time.perf_counter() - t0)*1000:.0f}ms"
        )
        if not results:
            return "", []
        result = results[0]
        tokens = result.tokens
        for token in tokens:
            token.start += offset_s
            token.end = token.start + token.duration
        text = getattr(result, "text", result)
        return (text if isinstance(text, str) else ""), tokens

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
        help="optional parakeet-mlx chunk duration in seconds for long utterances; "
        "replaces the live pieces with one chunked pass on release",
    )
    parser.add_argument(
        "--overlap-duration",
        type=float,
        default=15.0,
        help="chunk overlap in seconds when --chunk-duration is set",
    )
    parser.add_argument(
        "--segment-min-s",
        type=float,
        default=DEFAULT_SEGMENT_MIN_S,
        help="start transcribing a dictation in pieces once this much audio is "
        "untranscribed, cutting at the next pause; 0 = one pass on release",
    )
    parser.add_argument(
        "--segment-max-s",
        type=float,
        default=DEFAULT_SEGMENT_MAX_S,
        help="cut a piece at its quietest moment once it reaches this length, "
        "even without a clear pause",
    )
    parser.add_argument(
        "--segment-overlap-s",
        type=float,
        default=DEFAULT_SEGMENT_OVERLAP_S,
        help="audio before each cut that the next piece hears again, so words at "
        "the seam keep their context",
    )
    parser.add_argument(
        "--no-pause-decode",
        dest="pause_decode",
        action="store_false",
        help="do not start decoding at pauses; decode the last piece after the tap",
    )
    parser.add_argument(
        "--profile-ms",
        type=int,
        default=160,
        choices=sorted({80, 160, 560, 1120}),
        help="accepted for config parity with other backends; ignored in offline mode",
    )
    return parser


def validate_segments(parser: argparse.ArgumentParser, args) -> None:
    if args.segment_min_s < 0:
        parser.error("--segment-min-s must be >= 0")
    if args.segment_min_s == 0:
        return
    if args.segment_max_s < args.segment_min_s:
        parser.error("--segment-max-s must be >= --segment-min-s")
    if not 0 < args.segment_overlap_s < args.segment_min_s / 2:
        parser.error("--segment-overlap-s must be > 0 and under half of --segment-min-s")


def main(argv=None) -> int:
    parser = create_parser()
    args = parser.parse_args(argv)
    validate_segments(parser, args)

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
        segment_min_s=args.segment_min_s,
        segment_max_s=args.segment_max_s,
        segment_overlap_s=args.segment_overlap_s,
        pause_decode=args.pause_decode,
    )
    server = SidecarServer(engine)
    return serve(server, sys.stdin, protocol_out)


if __name__ == "__main__":
    raise SystemExit(main())

"""Protocol + CLI tests for the Parakeet-MLX offline sidecar, no MLX/model load."""

from __future__ import annotations

import io
import json
import os
import sys
import unittest
import wave
from contextlib import redirect_stderr
from array import array
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "asr"))

import parakeet_mlx_offline_sidecar  # noqa: E402
from nemotron_offline_sidecar import _OfflineBufferEngine  # noqa: E402
from parakeet_mlx_offline_sidecar import (  # noqa: E402
    BACKEND_NAME,
    DEFAULT_MODEL,
    PAUSE_FRAMES,
    OfflineParakeetMlxEngine,
    create_parser,
    find_cut,
    validate_segments,
)

TOKEN_SAMPLES = 1_600  # one fake word per 0.1 s
PIECES = {"segment_min_s": 1.0, "segment_max_s": 2.0, "segment_overlap_s": 0.3, "pause_decode": False}
PAUSES = {"segment_min_s": 0, "pause_decode": True}


class _FakeParakeetOfflineEngine(_OfflineBufferEngine):
    backend = BACKEND_NAME

    def __init__(self, final_text: str = "parakeet final") -> None:
        super().__init__()
        self.final_text = final_text
        self.transcribe_calls: list[str] = []

    def _transcribe_wav(self, path: str) -> str:
        self.transcribe_calls.append(path)
        with wave.open(path, "rb") as wav:
            assert wav.getnchannels() == 1
            assert wav.getsampwidth() == 2
            assert wav.getframerate() == 16_000
            frames = wav.readframes(wav.getnframes())
        self.last_samples = array("h")
        self.last_samples.frombytes(frames)
        return self.final_text


class _Token:
    def __init__(self, index: int) -> None:
        self.id = index
        self.text = f" w{index}"
        self.start = index * TOKEN_SAMPLES / 16_000
        self.end = (index + 1) * TOKEN_SAMPLES / 16_000


class _FakeDirectParakeetEngine(OfflineParakeetMlxEngine):
    """Exercise OfflineParakeetMlxEngine without loading MLX.

    Each decode yields one token per 0.1 s of audio, named after its absolute
    position, so overlapping pieces agree on the words they share and a
    correctly stitched dictation reads " w0 w1 ... wN".
    """

    def __init__(self, final_text: str | None = "direct final", chunk_duration=None, **segments) -> None:
        self.final_text = final_text
        self.direct_calls = 0
        self.wav_calls = 0
        self.pieces: list[tuple[int, int]] = []  # (start sample, length)
        super().__init__(chunk_duration=chunk_duration, **segments)

    def _load(self, model_name, precision, cache_dir) -> None:
        pass

    def _decode_pcm_i16(self, samples: array, offset_s: float):
        self.direct_calls += 1
        self.last_samples = array("h", samples)
        start = round(offset_s * 16_000)
        self.pieces.append((start, len(samples)))
        first = -(-start // TOKEN_SAMPLES)
        tokens = [_Token(i) for i in range(first, (start + len(samples)) // TOKEN_SAMPLES)]
        text = self.final_text if self.final_text is not None else self._tokens_text(tokens)
        return text, tokens

    def _merge_tokens(self, tokens, more):
        if not more:
            return tokens
        return [token for token in tokens if token.start < more[0].start] + more

    @staticmethod
    def _tokens_text(tokens) -> str:
        return "".join(token.text for token in tokens).strip()

    def _transcribe_wav(self, path: str) -> str:
        self.wav_calls += 1
        with wave.open(path, "rb") as wav:
            frames = wav.readframes(wav.getnframes())
        self.last_samples = array("h")
        self.last_samples.frombytes(frames)
        return self.final_text


def _feed(engine, levels_and_seconds, after_frame=None):
    """Feed constant-loudness stretches in 20 ms frames, as the daemon does."""
    for level, seconds in levels_and_seconds:
        for _ in range(round(seconds * 50)):
            # Alternate the sign so each frame's RMS equals `level`.
            engine.accept_audio([(level if i % 2 else -level) / 32768.0 for i in range(320)])
            if after_frame is not None:
                after_frame()


def _words(seconds: float) -> str:
    return " ".join(f"w{i}" for i in range(round(seconds * 10)))


class BufferContractTest(unittest.TestCase):
    def test_backend_label_distinguishes_parakeet_mlx(self):
        self.assertEqual(BACKEND_NAME, "parakeet_mlx_offline")
        self.assertEqual(_FakeParakeetOfflineEngine().backend, BACKEND_NAME)

    def test_accept_audio_converts_sidecar_float32_to_i16(self):
        engine = _FakeParakeetOfflineEngine(final_text="ok")
        engine.start(160)
        engine.accept_audio([0.0, 16384 / 32768.0, -32768 / 32768.0, 32767 / 32768.0])
        self.assertEqual(engine.finish(), "ok")
        self.assertEqual(list(engine.last_samples), [0, 16384, -32768, 32767])

    def test_empty_session_finishes_empty_string_without_transcribing(self):
        engine = _FakeParakeetOfflineEngine()
        engine.start(160)
        self.assertEqual(engine.finish(), "")
        self.assertEqual(engine.transcribe_calls, [])

    def test_lifecycle_buffers_then_transcribes_once_on_finish(self):
        engine = _FakeParakeetOfflineEngine(final_text="hello mlx")
        engine.start(160)
        self.assertEqual(engine.accept_audio([1 / 32768.0, 2 / 32768.0]), [])
        self.assertEqual(engine.accept_audio([3 / 32768.0]), [])
        self.assertEqual(engine.finish(), "hello mlx")
        self.assertEqual(len(engine.transcribe_calls), 1)
        self.assertEqual(list(engine.last_samples), [1, 2, 3])

    def test_cancel_drops_buffered_audio(self):
        engine = _FakeParakeetOfflineEngine()
        engine.start(160)
        engine.accept_audio([1 / 32768.0])
        engine.cancel()
        engine.start(160)
        self.assertEqual(engine.finish(), "")
        self.assertEqual(engine.transcribe_calls, [])

    def test_parakeet_default_finish_uses_direct_pcm_path(self):
        engine = _FakeDirectParakeetEngine(final_text="direct ok")
        engine.start(160)
        engine.accept_audio([10 / 32768.0, -20 / 32768.0, 30 / 32768.0])
        self.assertEqual(engine.finish(), "direct ok")
        self.assertEqual(engine.direct_calls, 1)
        self.assertEqual(engine.wav_calls, 0)
        self.assertEqual(list(engine.last_samples), [10, -20, 30])

    def test_parakeet_chunking_finish_falls_back_to_wav_transcribe(self):
        engine = _FakeDirectParakeetEngine(final_text="chunk ok", chunk_duration=120.0)
        engine.start(160)
        engine.accept_audio([10 / 32768.0, -20 / 32768.0])
        self.assertEqual(engine.finish(), "chunk ok")
        self.assertEqual(engine.direct_calls, 0)
        self.assertEqual(engine.wav_calls, 1)
        self.assertEqual(list(engine.last_samples), [10, -20])


class FindCutTest(unittest.TestCase):
    def test_no_cut_without_a_full_window(self):
        self.assertIsNone(find_cut([0.0] * (PAUSE_FRAMES - 1), 0, PAUSE_FRAMES - 1, 100, True))

    def test_cuts_in_the_middle_of_the_quietest_pause(self):
        levels = [500.0] * 20 + [10.0] * 15 + [500.0] * 10 + [5.0] * 15 + [500.0] * 5
        self.assertEqual(find_cut(levels, 0, len(levels), 100, False), 45 + PAUSE_FRAMES // 2)

    def test_a_dip_that_is_not_a_pause_is_only_cut_when_forced(self):
        levels = [500.0] * 20 + [300.0] * 15 + [500.0] * 20
        self.assertIsNone(find_cut(levels, 0, len(levels), 100, False))
        self.assertEqual(find_cut(levels, 0, len(levels), 100, True), 20 + PAUSE_FRAMES // 2)

    def test_pauses_before_the_search_start_are_ignored(self):
        levels = [0.0] * 15 + [500.0] * 30
        self.assertIsNone(find_cut(levels, 15, len(levels), 100, False))


class PiecesTest(unittest.TestCase):
    def test_short_dictation_is_one_whole_pass(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PIECES)
        engine.start(560)
        _feed(engine, [(8000, 0.9)])
        self.assertEqual(engine.finish(), _words(0.9))
        self.assertEqual(engine.pieces, [(0, 14_400)])

    def test_long_dictation_is_cut_inside_a_pause_and_stitched(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PIECES)
        engine.start(560)
        _feed(engine, [(8000, 1.6), (0, 0.5), (8000, 1.0)])
        self.assertEqual(engine.finish(), _words(3.1))
        # Cut 140 ms into the pause at 1.6 s; the tail re-hears 0.3 s before it.
        self.assertEqual(engine.pieces, [(0, 27_840), (23_040, 26_560)])

    def test_without_a_pause_the_cut_lands_on_the_quietest_moment(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PIECES)
        engine.start(560)
        _feed(engine, [(8000, 1.2), (3000, 0.3), (8000, 1.5)])
        self.assertEqual(engine.finish(), _words(3.0))
        self.assertEqual(engine.pieces, [(0, 21_440), (16_640, 31_360)])

    def test_only_the_open_piece_and_its_overlap_stay_in_memory(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PIECES)
        engine.start(560)
        kept = []
        _feed(engine, [(8000, 1.2), (0, 0.4)] * 8, after_frame=lambda: kept.append(len(engine._buffer)))
        self.assertEqual(engine.finish(), _words(12.8))
        self.assertGreaterEqual(len(engine.pieces), 6)
        # Max piece + overlap + one 100 ms cut check, not the whole 12.8 s.
        self.assertLessEqual(max(kept), round((2.0 + 0.3 + 0.1) * 16_000))

    def test_cancel_drops_queued_pieces_and_starts_clean(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PIECES)
        engine.start(560)
        _feed(engine, [(8000, 1.6), (0, 0.5)])
        self.assertEqual(len(engine._pieces), 1)
        engine.cancel()
        self.assertEqual((engine._pieces, len(engine._buffer), engine._segment_start), ([], 0, 0))
        engine.start(560)
        _feed(engine, [(8000, 0.5)])
        self.assertEqual(engine.finish(), _words(0.5))
        self.assertEqual(engine.pieces[-1], (0, 8_000))

    def test_segment_min_zero_keeps_one_pass_on_release(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **{**PIECES, "segment_min_s": 0})
        engine.start(560)
        _feed(engine, [(8000, 1.6), (0, 0.5), (8000, 1.0)])
        self.assertEqual(engine.finish(), _words(3.1))
        self.assertEqual(engine.pieces, [(0, 49_600)])


class PauseDecodeTest(unittest.TestCase):
    """Decoding at the speaker's pause so the tap finds the work done."""

    def test_tap_after_a_pause_reuses_the_pause_decode(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        _feed(engine, [(8000, 0.8), (0, 0.5)])
        # One decode, started 200 ms into the pause; nothing left for the tap.
        self.assertEqual(engine.finish(), _words(1.0))
        self.assertEqual(engine.pieces, [(0, 16_000)])

    def test_a_long_silence_starts_only_one_pause_decode(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        _feed(engine, [(8000, 0.8), (0, 2.0)])
        self.assertEqual(engine.finish(), _words(1.0))
        self.assertEqual(engine.pieces, [(0, 16_000)])

    def test_speech_after_the_pause_is_decoded_after_the_tap(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        _feed(engine, [(8000, 0.8), (0, 0.5), (8000, 0.5)])
        self.assertEqual(engine.finish(), _words(1.8))
        self.assertEqual(engine.pieces, [(0, 16_000), (0, 28_800)])

    def test_the_key_click_right_before_the_tap_does_not_count_as_speech(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        _feed(engine, [(8000, 0.8), (0, 0.5), (8000, 0.1)])
        self.assertEqual(engine.finish(), _words(1.0))
        self.assertEqual(engine.pieces, [(0, 16_000)])

    def test_a_soft_last_word_after_the_pause_is_not_dropped(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        # 400 is below the speech threshold (20% of 8000) but far above the pause.
        _feed(engine, [(8000, 0.8), (0, 0.5), (400, 0.4)])
        self.assertEqual(engine.finish(), _words(1.7))
        self.assertEqual(engine.pieces, [(0, 16_000), (0, 27_200)])

    def test_no_cut_while_silent_after_a_pause_decode(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **{**PIECES, "pause_decode": True})
        engine.start(560)
        _feed(engine, [(8000, 1.6), (0, 0.5), (8000, 1.0), (0, 0.5)])
        self.assertEqual(engine.finish(), _words(3.3))
        # The first pause decode became piece 1 at the max length; the second
        # was still covering the final pause at the tap. Nothing after the tap.
        self.assertEqual(engine.pieces, [(0, 28_800), (24_000, 29_120)])

    def test_the_key_click_does_not_cut_a_long_piece_in_its_final_pause(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **{**PIECES, "pause_decode": True})
        engine.start(560)
        # Long enough to cut (min 1 s), then a pause, a click and the tap.
        _feed(engine, [(8000, 1.2), (0, 0.4), (3000, 0.04), (0, 0.04)])
        self.assertEqual(engine.finish(), _words(1.4))
        self.assertEqual(engine.pieces, [(0, 22_400)])

    def test_speech_after_a_long_enough_pause_decode_makes_it_a_piece(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **{**PIECES, "pause_decode": True})
        engine.start(560)
        _feed(engine, [(8000, 1.2), (0, 0.3), (8000, 1.0)])
        self.assertEqual(engine.finish(), _words(2.5))
        self.assertEqual(engine.pieces, [(0, 22_400), (17_600, 22_400)])

    def test_cancel_drops_the_pause_decode(self):
        engine = _FakeDirectParakeetEngine(final_text=None, **PAUSES)
        engine.start(560)
        _feed(engine, [(8000, 0.8), (0, 0.5)])
        engine.cancel()
        self.assertIsNone(engine._pause)
        engine.start(560)
        _feed(engine, [(8000, 0.5)])
        self.assertEqual(engine.finish(), _words(0.5))
        self.assertEqual(engine.pieces[-1], (0, 8_000))


class OfflineProtocolTest(unittest.TestCase):
    def setUp(self):
        self.engine = _FakeParakeetOfflineEngine(final_text="offline parakeet")
        self.server = parakeet_mlx_offline_sidecar.SidecarServer(self.engine)

    def handle(self, line):
        return self.server.handle_line(line)

    def test_health_reports_parakeet_mlx_backend(self):
        self.assertEqual(
            self.handle('{"type":"health"}'),
            [{"type": "ready", "backend": BACKEND_NAME}],
        )

    def test_session_emits_no_partials_and_one_final(self):
        self.assertEqual(
            self.handle('{"type":"start_session","session_id":7,"profile_ms":160}'),
            [{"type": "session_started", "session_id": 7}],
        )
        for _ in range(3):
            self.assertEqual(
                self.handle('{"type":"audio_chunk","session_id":7,"samples":[0,1,2]}'),
                [],
            )
        self.assertEqual(self.engine.transcribe_calls, [])
        self.assertEqual(
            self.handle('{"type":"finish_session","session_id":7}'),
            [{"type": "final", "session_id": 7, "text": "offline parakeet"}],
        )
        self.assertEqual(len(self.engine.transcribe_calls), 1)

    def test_profile_ms_is_accepted_but_ignored(self):
        for profile in (80, 160, 560, 1120):
            engine = _FakeParakeetOfflineEngine()
            server = parakeet_mlx_offline_sidecar.SidecarServer(engine)
            events = server.handle_line(
                f'{{"type":"start_session","session_id":1,"profile_ms":{profile}}}'
            )
            self.assertEqual(events, [{"type": "session_started", "session_id": 1}])

    def test_serve_writes_compact_json_lines(self):
        stdin = io.StringIO(
            '{"type":"health"}\n'
            '{"type":"start_session","session_id":1,"profile_ms":160}\n'
            '{"type":"audio_chunk","session_id":1,"samples":[0,1,2,3]}\n'
            '{"type":"finish_session","session_id":1}\n'
        )
        stdout = io.StringIO()
        self.assertEqual(parakeet_mlx_offline_sidecar.serve(self.server, stdin, stdout), 0)
        lines = stdout.getvalue().strip().splitlines()
        self.assertEqual(
            lines[0],
            json.dumps({"type": "ready", "backend": BACKEND_NAME}, separators=(",", ":")),
        )
        self.assertEqual(
            lines[-1],
            json.dumps(
                {"type": "final", "session_id": 1, "text": "offline parakeet"},
                separators=(",", ":"),
            ),
        )
        self.assertFalse(any('"type":"partial"' in line for line in lines))


class CliTest(unittest.TestCase):
    def test_defaults_target_v3_bf16(self):
        args = create_parser().parse_args([])
        self.assertEqual(args.model, DEFAULT_MODEL)
        self.assertEqual(args.model, "mlx-community/parakeet-tdt-0.6b-v3")
        self.assertEqual(args.precision, "bf16")
        self.assertEqual(args.profile_ms, 160)
        self.assertIsNone(args.cache_dir)
        self.assertIsNone(args.chunk_duration)
        self.assertEqual(args.overlap_duration, 15.0)
        self.assertEqual(
            (args.segment_min_s, args.segment_max_s, args.segment_overlap_s), (10.0, 20.0, 3.0)
        )
        self.assertTrue(args.pause_decode)
        self.assertFalse(create_parser().parse_args(["--no-pause-decode"]).pause_decode)

    def test_segment_flags_are_validated(self):
        parser = create_parser()
        for argv in (
            ["--segment-max-s", "5"],
            ["--segment-overlap-s", "5"],
            ["--segment-overlap-s", "0"],
            ["--segment-min-s", "-1"],
        ):
            with self.subTest(argv=argv), redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    validate_segments(parser, parser.parse_args(argv))
        # Pieces off: the other segment flags no longer matter.
        validate_segments(parser, parser.parse_args(["--segment-min-s", "0", "--segment-overlap-s", "99"]))

    def test_precision_model_and_chunking_are_overridable(self):
        args = create_parser().parse_args(
            [
                "--model",
                "mlx-community/parakeet-tdt-0.6b-v2",
                "--precision",
                "fp32",
                "--cache-dir",
                "~/hf-cache",
                "--chunk-duration",
                "120",
                "--overlap-duration",
                "10",
            ]
        )
        self.assertEqual(args.model, "mlx-community/parakeet-tdt-0.6b-v2")
        self.assertEqual(args.precision, "fp32")
        self.assertEqual(args.cache_dir, "~/hf-cache")
        self.assertEqual(args.chunk_duration, 120)
        self.assertEqual(args.overlap_duration, 10)

    def test_module_imports_without_parakeet_mlx(self):
        # Importing the module and parser must not require MLX/parakeet_mlx; only
        # constructing the real engine imports those optional runtime packages.
        self.assertTrue(hasattr(parakeet_mlx_offline_sidecar, "OfflineParakeetMlxEngine"))
        self.assertTrue(issubclass(OfflineParakeetMlxEngine, _OfflineBufferEngine))


if __name__ == "__main__":
    unittest.main()


class SpecialTokenGuardTest(unittest.TestCase):
    """A run of <unk> (a GPU glitch under load) never reaches the user."""

    class _Tok:
        def __init__(self, id, text):
            self.id, self.text, self.start, self.duration, self.end, self.confidence = id, text, 0.0, 0.1, 0.1, 1.0

    class _Result:
        def __init__(self, tokens):
            self.tokens = tokens
            self.text = "".join(t.text for t in tokens)

    def _engine(self, retries):
        engine = _FakeDirectParakeetEngine()

        class Model:
            vocabulary = ["<unk>", "\u2581hi", "\u2581there"]

            def generate(self, mel):
                return [retries.pop(0)]

        engine.model = Model()
        return engine

    def test_a_glitched_decode_is_decoded_again(self):
        good = self._Result([self._Tok(1, " hi"), self._Tok(2, " there")])
        bad = self._Result([self._Tok(0, "<unk>")] * 5)
        engine = self._engine([good])
        self.assertIs(engine._without_special_tokens(bad, mel=None), good)

    def test_a_clean_decode_is_kept_without_a_retry(self):
        good = self._Result([self._Tok(1, " hi")])
        engine = self._engine([])
        self.assertIs(engine._without_special_tokens(good, mel=None), good)

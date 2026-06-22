"""Protocol + engine tests for Parakeet-MLX streaming sidecar, no MLX/model load."""

from __future__ import annotations

import io
import json
import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "asr"))

import parakeet_mlx_streaming_sidecar  # noqa: E402
from parakeet_mlx_offline_sidecar import DEFAULT_MODEL  # noqa: E402
from parakeet_mlx_streaming_sidecar import (  # noqa: E402
    BACKEND_NAME,
    StreamingParakeetMlxEngine,
    create_parser,
)


class _FakeStreamingEngine(StreamingParakeetMlxEngine):
    """Exercise StreamingParakeetMlxEngine's buffering/protocol without MLX."""

    def __init__(self, texts=None, chunk_samples=4, flush_samples=0, final_mode="direct") -> None:
        self.model = object()
        self._mx = object()
        self._dtype = object()
        self._get_logmel = object()
        self.precision = "bf16"
        self.chunk_samples = chunk_samples
        self.flush_samples = flush_samples
        self.min_final_samples = 3
        self.context_size = (256, 256)
        self.depth = 1
        self.keep_original_attention = False
        self.final_mode = final_mode
        self._stream_cm = None
        self._transcriber = None
        self._pending = []
        self._pending_len = 0
        self._all_audio = []
        self._last_text = ""
        self._last_good_text = ""
        self._seen_audio = False
        self._chunk_count = 0
        self.texts = list(texts or [])
        self.current = ""
        self.open_count = 0
        self.close_count = 0
        self.added_chunks: list[tuple[str, list[float]]] = []
        self.direct_calls: list[list[float]] = []

    def _open_stream(self) -> None:
        self.open_count += 1
        self._transcriber = object()

    def _close_stream(self) -> None:
        if self._transcriber is not None:
            self.close_count += 1
        self._transcriber = None
        self._stream_cm = None

    def _add_audio_to_stream(self, samples, *, reason: str) -> str:
        self._chunk_count += 1
        self.added_chunks.append((reason, list(samples)))
        if self.texts:
            self.current = self.texts.pop(0)
        return self.current

    def _transcribe_direct_pcm(self, samples: list[float]) -> str:
        self.direct_calls.append(list(samples))
        return self.current

    def _current_text(self) -> str:
        return self.current


class StreamingEngineTest(unittest.TestCase):
    def test_backend_label(self):
        self.assertEqual(BACKEND_NAME, "parakeet_mlx_streaming")
        self.assertEqual(_FakeStreamingEngine().backend, BACKEND_NAME)

    def test_accept_audio_buffers_until_chunk_size_then_emits_changed_partials(self):
        engine = _FakeStreamingEngine(texts=["hel", "hello"], chunk_samples=4)
        engine.start(160)
        self.assertEqual(engine.accept_audio([1 / 32768.0, 2 / 32768.0]), [])
        self.assertEqual(engine._pending_len, 2)
        self.assertEqual(engine.accept_audio([3 / 32768.0, 4 / 32768.0]), ["hel"])
        self.assertEqual(engine.accept_audio([5 / 32768.0] * 4), ["hello"])
        self.assertEqual([reason for reason, _chunk in engine.added_chunks], ["partial", "partial"])
        self.assertEqual(engine.finish(), "hello")
        self.assertEqual(engine.direct_calls, [[1 / 32768.0, 2 / 32768.0, 3 / 32768.0, 4 / 32768.0] + [5 / 32768.0] * 4])
        self.assertEqual(engine.open_count, 1)
        self.assertEqual(engine.close_count, 1)

    def test_finish_flushes_pending_audio_and_optional_trailing_silence(self):
        engine = _FakeStreamingEngine(texts=["final-ish", "final"], chunk_samples=10, flush_samples=3)
        engine.start(160)
        engine.accept_audio([10 / 32768.0, -10 / 32768.0])
        self.assertEqual(engine.finish(), "final")
        self.assertEqual(
            engine.added_chunks,
            [
                ("final", [10 / 32768.0, -10 / 32768.0]),
                ("flush", [0.0, 0.0, 0.0]),
            ],
        )

    def test_finish_drops_tiny_residual_after_streaming_chunks(self):
        engine = _FakeStreamingEngine(texts=["partial"], chunk_samples=4, flush_samples=0)
        engine.start(160)
        self.assertEqual(engine.accept_audio([1 / 32768.0] * 4), ["partial"])
        engine.accept_audio([2 / 32768.0])
        self.assertEqual(engine.finish(), "partial")
        self.assertEqual(engine.added_chunks, [("partial", [1 / 32768.0] * 4)])
        self.assertEqual(engine.direct_calls, [[1 / 32768.0] * 4 + [2 / 32768.0]])

    def test_cancel_closes_stream_and_drops_pending_audio(self):
        engine = _FakeStreamingEngine(chunk_samples=10)
        engine.start(160)
        engine.accept_audio([1 / 32768.0, 2 / 32768.0])
        engine.cancel()
        self.assertEqual(engine.close_count, 1)
        self.assertEqual(engine._pending_len, 0)
        self.assertEqual(engine._all_audio, [])
        self.assertEqual(engine.added_chunks, [])

    def test_empty_session_finishes_empty_string_without_chunks(self):
        engine = _FakeStreamingEngine()
        engine.start(160)
        self.assertEqual(engine.finish(), "")
        self.assertEqual(engine.added_chunks, [])

    def test_invalid_profile_is_rejected(self):
        engine = _FakeStreamingEngine()
        with self.assertRaises(ValueError):
            engine.start(123)


class StreamingProtocolTest(unittest.TestCase):
    def setUp(self):
        self.engine = _FakeStreamingEngine(texts=["partial", "final"], chunk_samples=3, flush_samples=1)
        self.server = parakeet_mlx_streaming_sidecar.SidecarServer(self.engine)

    def handle(self, line):
        return self.server.handle_line(line)

    def test_health_reports_streaming_backend(self):
        self.assertEqual(
            self.handle('{"type":"health"}'),
            [{"type": "ready", "backend": BACKEND_NAME}],
        )

    def test_session_emits_partials_and_final(self):
        self.assertEqual(
            self.handle('{"type":"start_session","session_id":5,"profile_ms":160}'),
            [{"type": "session_started", "session_id": 5}],
        )
        partial = self.handle('{"type":"audio_chunk","session_id":5,"samples":[0,1,2]}')
        self.assertEqual(partial, [{"type": "partial", "session_id": 5, "text": "partial"}])
        final = self.handle('{"type":"finish_session","session_id":5}')
        self.assertEqual(final, [{"type": "final", "session_id": 5, "text": "final"}])

    def test_serve_writes_compact_json_lines(self):
        stdin = io.StringIO(
            '{"type":"health"}\n'
            '{"type":"start_session","session_id":1,"profile_ms":160}\n'
            '{"type":"audio_chunk","session_id":1,"samples":[0,1,2]}\n'
            '{"type":"finish_session","session_id":1}\n'
        )
        stdout = io.StringIO()
        self.assertEqual(parakeet_mlx_streaming_sidecar.serve(self.server, stdin, stdout), 0)
        lines = stdout.getvalue().strip().splitlines()
        self.assertEqual(
            lines[0],
            json.dumps({"type": "ready", "backend": BACKEND_NAME}, separators=(",", ":")),
        )
        self.assertIn('"type":"partial"', lines[2])
        self.assertIn('"type":"final"', lines[-1])


class CliTest(unittest.TestCase):
    def test_defaults_target_v3_streaming(self):
        args = create_parser().parse_args([])
        self.assertEqual(args.model, DEFAULT_MODEL)
        self.assertEqual(args.model, "mlx-community/parakeet-tdt-0.6b-v3")
        self.assertEqual(args.precision, "bf16")
        self.assertEqual(args.profile_ms, 160)
        self.assertEqual(args.chunk_ms, 320)
        self.assertEqual(args.flush_ms, 0)
        self.assertEqual(args.min_final_ms, 160)
        self.assertEqual(args.left_context, 256)
        self.assertEqual(args.right_context, 256)
        self.assertEqual(args.depth, 1)
        self.assertFalse(args.keep_original_attention)
        self.assertEqual(args.final_mode, "direct")

    def test_tuning_flags_are_overridable(self):
        args = create_parser().parse_args(
            [
                "--model",
                "mlx-community/parakeet-tdt-0.6b-v2",
                "--precision",
                "fp32",
                "--profile-ms",
                "80",
                "--chunk-ms",
                "640",
                "--flush-ms",
                "0",
                "--min-final-ms",
                "80",
                "--left-context",
                "128",
                "--right-context",
                "64",
                "--depth",
                "2",
                "--keep-original-attention",
                "--final-mode",
                "streaming",
            ]
        )
        self.assertEqual(args.model, "mlx-community/parakeet-tdt-0.6b-v2")
        self.assertEqual(args.precision, "fp32")
        self.assertEqual(args.profile_ms, 80)
        self.assertEqual(args.chunk_ms, 640)
        self.assertEqual(args.flush_ms, 0)
        self.assertEqual(args.min_final_ms, 80)
        self.assertEqual(args.left_context, 128)
        self.assertEqual(args.right_context, 64)
        self.assertEqual(args.depth, 2)
        self.assertTrue(args.keep_original_attention)
        self.assertEqual(args.final_mode, "streaming")

    def test_module_imports_without_parakeet_mlx(self):
        self.assertTrue(hasattr(parakeet_mlx_streaming_sidecar, "StreamingParakeetMlxEngine"))


if __name__ == "__main__":
    unittest.main()

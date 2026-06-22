"""Protocol + CLI tests for the Parakeet-MLX offline sidecar, no MLX/model load."""

from __future__ import annotations

import io
import json
import os
import sys
import unittest
import wave
from array import array
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "asr"))

import parakeet_mlx_offline_sidecar  # noqa: E402
from nemotron_offline_sidecar import _OfflineBufferEngine  # noqa: E402
from parakeet_mlx_offline_sidecar import (  # noqa: E402
    BACKEND_NAME,
    DEFAULT_MODEL,
    OfflineParakeetMlxEngine,
    create_parser,
)


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


class _FakeDirectParakeetEngine(OfflineParakeetMlxEngine):
    """Exercise OfflineParakeetMlxEngine.finish without loading MLX."""

    def __init__(self, final_text: str = "direct final", chunk_duration=None) -> None:
        _OfflineBufferEngine.__init__(self)
        self.final_text = final_text
        self.chunk_duration = chunk_duration
        self.overlap_duration = 15.0
        self.direct_calls = 0
        self.wav_calls = 0

    def _transcribe_pcm_i16(self, samples: array) -> str:
        self.direct_calls += 1
        self.last_samples = array("h", samples)
        return self.final_text

    def _transcribe_wav(self, path: str) -> str:
        self.wav_calls += 1
        with wave.open(path, "rb") as wav:
            frames = wav.readframes(wav.getnframes())
        self.last_samples = array("h")
        self.last_samples.frombytes(frames)
        return self.final_text


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

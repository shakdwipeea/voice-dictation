"""Protocol + buffer tests for the offline Nemotron sidecar, no NeMo/GPU.

These mirror tests/phase1/test_nemotron_sidecar_protocol.py but exercise the
offline engine: buffered audio, whole-utterance WAV transcription, and the
"no partials, one final" protocol behavior.
"""

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

import nemotron_offline_sidecar  # noqa: E402
from nemotron_offline_sidecar import (  # noqa: E402
    BACKEND_NAME,
    OfflineNemotronEngine,
    _OfflineBufferEngine,
    create_parser,
    main,
)


class _FakeOfflineEngine(_OfflineBufferEngine):
    """Records transcribe calls; returns a scripted final text.

    No NeMo: ``_transcribe_wav`` reads the WAV back (proving the buffer engine
    wrote valid 16 kHz mono s16le PCM) and returns a fixed string.
    """

    def __init__(self, final_text: str = "hello world") -> None:
        super().__init__()
        self.final_text = final_text
        self.transcribe_calls: list[str] = []

    def _transcribe_wav(self, path: str) -> str:
        self.transcribe_calls.append(path)
        # Validate the WAV the buffer engine wrote is well-formed and matches
        # what we buffered (proves the offline plumbing end to end).
        with wave.open(path, "rb") as wav:
            assert wav.getnchannels() == 1
            assert wav.getsampwidth() == 2
            assert wav.getframerate() == 16_000
            frames = wav.readframes(wav.getnframes())
        self.last_samples = array("h")
        self.last_samples.frombytes(frames)
        return self.final_text


class BufferEngineTest(unittest.TestCase):
    def test_backend_label_distinguishes_offline(self):
        self.assertEqual(_FakeOfflineEngine().backend, BACKEND_NAME)
        self.assertEqual(BACKEND_NAME, "nemotron_offline")

    def test_empty_session_finishes_empty_string_without_transcribing(self):
        engine = _FakeOfflineEngine()
        engine.start(160)
        self.assertEqual(engine.finish(), "")
        self.assertEqual(engine.transcribe_calls, [])

    def test_lifecycle_buffers_then_transcribes_once_on_finish(self):
        engine = _FakeOfflineEngine(final_text="hi there")
        engine.start(160)
        # No partials are ever emitted in offline mode. Feed float32 (the
        # SidecarServer contract: i16 / 32768); the buffer inverts it.
        self.assertEqual(
            engine.accept_audio([0.0, 16384 / 32768.0, -32768 / 32768.0, 32767 / 32768.0]),
            [],
        )
        self.assertEqual(engine.accept_audio([100 / 32768.0, 200 / 32768.0, 300 / 32768.0]), [])
        self.assertEqual(engine.finish(), "hi there")
        # Exactly one whole-utterance transcribe call.
        self.assertEqual(len(engine.transcribe_calls), 1)
        # The buffered i16 samples round-trip through the written WAV intact.
        self.assertEqual(
            list(engine.last_samples), [0, 16384, -32768, 32767, 100, 200, 300]
        )

    def test_finish_clears_the_buffer_for_the_next_session(self):
        engine = _FakeOfflineEngine()
        engine.start(160)
        engine.accept_audio([1 / 32768.0, 2 / 32768.0, 3 / 32768.0])
        engine.finish()
        engine.start(160)
        self.assertEqual(engine.accept_audio([4 / 32768.0, 5 / 32768.0]), [])
        self.assertEqual(engine.finish(), "hello world")
        self.assertEqual(list(engine.last_samples), [4, 5])

    def test_cancel_drops_buffered_audio(self):
        engine = _FakeOfflineEngine()
        engine.start(160)
        engine.accept_audio([1, 2, 3])
        engine.cancel()
        engine.start(160)
        self.assertEqual(engine.finish(), "")
        self.assertEqual(engine.transcribe_calls, [])

    def test_accept_audio_converts_sidecar_float32_to_i16(self):
        # Production contract: SidecarServer divides wire i16 by 32768 and
        # passes float32 in [-1, 1) to accept_audio. The buffer must invert
        # that so the WAV holds the original PCM (regression: previously
        # int(0.014) truncated every sample to 0 -> silent audio).
        engine = _FakeOfflineEngine(final_text="ok")
        engine.start(160)
        # Mimic SidecarServer: feed float32 (i16 / 32768).
        engine.accept_audio([0.0, 16384 / 32768.0, -32768 / 32768.0, 32767 / 32768.0])
        self.assertEqual(engine.finish(), "ok")
        # The written WAV must carry the original i16 values, not zeros.
        self.assertEqual(list(engine.last_samples), [0, 16384, -32768, 32767])

    def test_write_wav_is_16khz_mono_s16le(self):
        path = _OfflineBufferEngine._write_wav(array("h", [0, -1, 32767, -32768]))
        try:
            with wave.open(path, "rb") as wav:
                self.assertEqual(wav.getnchannels(), 1)
                self.assertEqual(wav.getsampwidth(), 2)
                self.assertEqual(wav.getframerate(), 16_000)
                data = array("h")
                data.frombytes(wav.readframes(wav.getnframes()))
                self.assertEqual(list(data), [0, -1, 32767, -32768])
        finally:
            os.unlink(path)


class OfflineProtocolTest(unittest.TestCase):
    def setUp(self):
        self.engine = _FakeOfflineEngine(final_text="offline final")
        self.server = nemotron_offline_sidecar.SidecarServer(self.engine)

    def handle(self, line):
        return self.server.handle_line(line)

    def test_health_reports_offline_backend(self):
        self.assertEqual(
            self.handle('{"type":"health"}'),
            [{"type": "ready", "backend": BACKEND_NAME}],
        )

    def test_session_emits_no_partials_and_one_final(self):
        started = self.handle(
            '{"type":"start_session","session_id":11,"profile_ms":160}'
        )
        self.assertEqual(started, [{"type": "session_started", "session_id": 11}])

        # While recording: no partial events, ever.
        for _ in range(3):
            self.assertEqual(
                self.handle('{"type":"audio_chunk","session_id":11,"samples":[0,1,2]}'),
                [],
            )
        self.assertEqual(self.engine.transcribe_calls, [])

        final = self.handle('{"type":"finish_session","session_id":11}')
        self.assertEqual(
            final, [{"type": "final", "session_id": 11, "text": "offline final"}]
        )
        # Transcribe ran exactly once, on finish.
        self.assertEqual(len(self.engine.transcribe_calls), 1)

    def test_profile_ms_is_accepted_but_ignored(self):
        # Any supported profile_ms must be accepted so Linux/macOS configs port.
        for profile in (80, 160, 560, 1120):
            engine = _FakeOfflineEngine()
            server = nemotron_offline_sidecar.SidecarServer(engine)
            events = server.handle_line(
                f'{{"type":"start_session","session_id":1,"profile_ms":{profile}}}'
            )
            self.assertEqual(events, [{"type": "session_started", "session_id": 1}])

    def test_cancel_matches_the_streaming_contract(self):
        self.handle('{"type":"start_session","session_id":3,"profile_ms":160}')
        events = self.handle('{"type":"cancel_session","session_id":3}')
        self.assertEqual(
            events,
            [{"type": "error", "session_id": 3, "message": "session cancelled"}],
        )

    def test_serve_writes_compact_json_lines(self):
        stdin = io.StringIO(
            '{"type":"health"}\n'
            '{"type":"start_session","session_id":1,"profile_ms":160}\n'
            '{"type":"audio_chunk","session_id":1,"samples":[0,1,2,3]}\n'
            '{"type":"finish_session","session_id":1}\n'
        )
        stdout = io.StringIO()
        self.assertEqual(nemotron_offline_sidecar.serve(self.server, stdin, stdout), 0)
        lines = stdout.getvalue().strip().splitlines()
        self.assertEqual(
            lines[0], json.dumps({"type": "ready", "backend": BACKEND_NAME}, separators=(",", ":"))
        )
        self.assertEqual(
            lines[-1], json.dumps({"type": "final", "session_id": 1, "text": "offline final"}, separators=(",", ":"))
        )
        # No partial lines are ever emitted.
        self.assertFalse(any('"type":"partial"' in line for line in lines))


class CliTest(unittest.TestCase):
    def test_defaults_target_cpu_and_nemotron_model(self):
        args = create_parser().parse_args([])
        self.assertEqual(args.device, "cpu")
        self.assertEqual(args.model, "nvidia/nemotron-speech-streaming-en-0.6b")
        self.assertEqual(args.profile_ms, 160)
        self.assertFalse(args.use_lhotse)

    def test_device_is_overridable_to_mps(self):
        args = create_parser().parse_args(["--device", "mps", "--use-lhotse"])
        self.assertEqual(args.device, "mps")
        self.assertTrue(args.use_lhotse)

    def test_module_imports_without_nemo_or_numpy(self):
        # The protocol + buffer layer must stay importable on the system python
        # so these tests run anywhere, like the streaming sidecar.
        self.assertTrue(hasattr(nemotron_offline_sidecar, "OfflineNemotronEngine"))


if __name__ == "__main__":
    unittest.main()

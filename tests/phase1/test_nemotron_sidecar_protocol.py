"""Protocol-layer tests for the Nemotron sidecar, no GPU or NeMo required."""

from __future__ import annotations

import io
import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "asr"))

import nemotron_sidecar  # noqa: E402
from nemotron_sidecar import (  # noqa: E402
    PROFILE_CONTEXTS,
    SidecarServer,
    create_parser,
    flush_padding_samples,
    samples_to_float32,
    serve,
    stable_frame_count,
)


class FakeEngine:
    """Records calls; returns scripted partial texts and a final text."""

    backend = "fake"

    def __init__(self, partials=None, final_text="fake final"):
        self.partials = list(partials or [])
        self.final_text = final_text
        self.calls = []
        self.fail_next = None

    def _maybe_fail(self):
        if self.fail_next is not None:
            error = self.fail_next
            self.fail_next = None
            raise error

    def start(self, profile_ms):
        self.calls.append(("start", profile_ms))
        self._maybe_fail()

    def accept_audio(self, samples):
        self.calls.append(("audio", len(samples)))
        self._maybe_fail()
        if self.partials:
            return [self.partials.pop(0)]
        return []

    def finish(self):
        self.calls.append(("finish",))
        self._maybe_fail()
        return self.final_text

    def cancel(self):
        self.calls.append(("cancel",))


class ProtocolTest(unittest.TestCase):
    def setUp(self):
        self.engine = FakeEngine()
        self.server = SidecarServer(self.engine)

    def handle(self, line):
        return self.server.handle_line(line)

    def test_health_reports_engine_backend(self):
        self.assertEqual(
            self.handle('{"type":"health"}'),
            [{"type": "ready", "backend": "fake"}],
        )

    def test_blank_lines_are_ignored(self):
        self.assertEqual(self.handle("   \n"), [])

    def test_malformed_json_yields_error_event(self):
        events = self.handle("this is not json")
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["type"], "error")
        self.assertIsNone(events[0]["session_id"])
        self.assertIn("invalid request", events[0]["message"])

    def test_non_object_json_yields_error_event(self):
        events = self.handle("[1,2,3]")
        self.assertEqual(events[0]["type"], "error")
        self.assertIn("not a JSON object", events[0]["message"])

    def test_start_session_requires_valid_fields(self):
        no_session = self.handle('{"type":"start_session","profile_ms":160}')
        self.assertIn("session_id", no_session[0]["message"])
        no_profile = self.handle('{"type":"start_session","session_id":1}')
        self.assertIn("profile_ms", no_profile[0]["message"])
        bad_profile = self.handle(
            '{"type":"start_session","session_id":1,"profile_ms":100}'
        )
        self.assertIn("unsupported profile_ms 100", bad_profile[0]["message"])
        bool_session = self.handle(
            '{"type":"start_session","session_id":true,"profile_ms":160}'
        )
        self.assertEqual(bool_session[0]["type"], "error")
        self.assertEqual(self.engine.calls, [])

    def test_session_lifecycle(self):
        started = self.handle(
            '{"type":"start_session","session_id":7,"profile_ms":160}'
        )
        self.assertEqual(started, [{"type": "session_started", "session_id": 7}])
        self.assertEqual(self.engine.calls, [("start", 160)])

        self.engine.partials = ["hello"]
        partials = self.handle(
            '{"type":"audio_chunk","session_id":7,"samples":[0,16384,-32768]}'
        )
        self.assertEqual(
            partials, [{"type": "partial", "session_id": 7, "text": "hello"}]
        )

        final = self.handle('{"type":"finish_session","session_id":7}')
        self.assertEqual(
            final, [{"type": "final", "session_id": 7, "text": "fake final"}]
        )
        # The session is gone; further requests for it are invalid.
        stale = self.handle('{"type":"finish_session","session_id":7}')
        self.assertEqual(stale[0]["type"], "error")

    def test_new_session_supersedes_active_one(self):
        self.handle('{"type":"start_session","session_id":1,"profile_ms":160}')
        events = self.handle(
            '{"type":"start_session","session_id":2,"profile_ms":160}'
        )
        self.assertEqual(
            events,
            [
                {"type": "error", "session_id": 1, "message": "superseded"},
                {"type": "session_started", "session_id": 2},
            ],
        )
        self.assertIn(("cancel",), self.engine.calls)

    def test_audio_for_wrong_session_is_an_error(self):
        self.handle('{"type":"start_session","session_id":1,"profile_ms":160}')
        events = self.handle('{"type":"audio_chunk","session_id":2,"samples":[0]}')
        self.assertEqual(events[0]["type"], "error")
        self.assertNotIn(("audio", 1), self.engine.calls)

    def test_audio_samples_must_be_an_array(self):
        self.handle('{"type":"start_session","session_id":1,"profile_ms":160}')
        events = self.handle('{"type":"audio_chunk","session_id":1,"samples":"x"}')
        self.assertIn("samples must be an array", events[0]["message"])

    def test_cancel_session_matches_mock_contract(self):
        self.handle('{"type":"start_session","session_id":3,"profile_ms":160}')
        events = self.handle('{"type":"cancel_session","session_id":3}')
        self.assertEqual(
            events,
            [{"type": "error", "session_id": 3, "message": "session cancelled"}],
        )

    def test_unknown_request_type_is_an_error(self):
        events = self.handle('{"type":"reboot"}')
        self.assertIn("invalid request: reboot", events[0]["message"])

    def test_engine_failure_is_reported_not_fatal(self):
        self.handle('{"type":"start_session","session_id":1,"profile_ms":160}')
        self.engine.fail_next = RuntimeError("CUDA exploded")
        events = self.handle('{"type":"audio_chunk","session_id":1,"samples":[0]}')
        self.assertEqual(events[0]["type"], "error")
        self.assertIn("engine failure", events[0]["message"])
        self.assertIn("CUDA exploded", events[0]["message"])
        self.assertIsNone(self.server.active_session)
        # The server keeps serving after the failure.
        self.assertEqual(
            self.handle('{"type":"health"}'),
            [{"type": "ready", "backend": "fake"}],
        )

    def test_serve_writes_compact_json_lines(self):
        stdin = io.StringIO('{"type":"health"}\nnot json\n')
        stdout = io.StringIO()
        self.assertEqual(serve(self.server, stdin, stdout), 0)
        lines = stdout.getvalue().strip().splitlines()
        self.assertEqual(lines[0], '{"type":"ready","backend":"fake"}')
        self.assertEqual(len(lines), 2)


class StreamingMathTest(unittest.TestCase):
    def test_profile_contexts_match_the_published_lookaheads(self):
        self.assertEqual(
            PROFILE_CONTEXTS,
            {80: [70, 0], 160: [70, 1], 560: [70, 6], 1120: [70, 13]},
        )

    def test_samples_convert_to_unit_floats(self):
        converted = list(samples_to_float32([0, 16384, -32768, 32767]))
        self.assertAlmostEqual(converted[0], 0.0)
        self.assertAlmostEqual(converted[1], 0.5)
        self.assertAlmostEqual(converted[2], -1.0)
        self.assertLess(abs(converted[3] - 0.99997), 1e-4)

    def test_stable_frames_never_cover_the_stft_guard(self):
        self.assertEqual(stable_frame_count(0), 0)
        self.assertEqual(stable_frame_count(256), 0)
        self.assertEqual(stable_frame_count(257), 1)
        for total in (300, 1000, 16000, 16001):
            stable = stable_frame_count(total)
            # The last stable frame's center plus the guard stays inside the
            # buffered audio, so growing the buffer cannot change it.
            self.assertLess((stable - 1) * 160 + 256, total)
            self.assertGreaterEqual(stable * 160 + 256, total)

    def test_flush_padding_lands_on_frame_boundaries(self):
        for total in (0, 1, 159, 160, 4000, 16013):
            padding = flush_padding_samples(total, chunk_frames=152)
            self.assertEqual((total + padding) % 160, 0)
            self.assertGreaterEqual(padding, 152 * 160 + 256)
        with self.assertRaises(ValueError):
            flush_padding_samples(-1, chunk_frames=8)
        with self.assertRaises(ValueError):
            flush_padding_samples(0, chunk_frames=0)

    def test_module_import_does_not_require_numpy(self):
        # The protocol layer must stay importable on the system python (no
        # numpy/NeMo) so these tests can run anywhere.
        self.assertTrue(hasattr(nemotron_sidecar, "NemotronEngine"))

    def test_streaming_parser_accepts_cpu_device_for_macos(self):
        args = create_parser().parse_args(["--profile-ms", "80", "--device", "cpu"])
        self.assertEqual(args.profile_ms, 80)
        self.assertEqual(args.device, "cpu")


if __name__ == "__main__":
    unittest.main()

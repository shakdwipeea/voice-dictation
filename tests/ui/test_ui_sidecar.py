"""Protocol tests for the UI sidecar dispatch — no GTK required."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "src"))

from voice_dictation.ui_sidecar import dispatch  # noqa: E402


class StubOverlay:
    def __init__(self):
        self.calls = []

    def show(self):
        self.calls.append(("show",))

    def hide(self):
        self.calls.append(("hide",))

    def set_recording(self, elapsed_s, peak, rms, segment_count):
        self.calls.append(("recording", elapsed_s, peak, rms, segment_count))

    def set_status(self, status):
        self.calls.append(("status", status))

    def set_state(self, name, detail):
        self.calls.append(("state", name, detail))

    def add_segment(self, text):
        self.calls.append(("segment", text))

    def clear_segments(self):
        self.calls.append(("clear",))

    def show_system_palette(self, session_id, transcript, suggestions):
        self.calls.append(("system_palette", session_id, transcript, suggestions))

    def dismiss_system_palette(self, session_id):
        self.calls.append(("dismiss_system_palette", session_id))

    def shutdown(self):
        self.calls.append(("shutdown",))


class DispatchTest(unittest.TestCase):
    def setUp(self):
        self.overlay = StubOverlay()

    def test_show_hide(self):
        self.assertTrue(dispatch(self.overlay, {"type": "show"}))
        self.assertTrue(dispatch(self.overlay, {"type": "hide"}))
        self.assertEqual(self.overlay.calls, [("show",), ("hide",)])

    def test_recording_maps_fields(self):
        ok = dispatch(self.overlay, {
            "type": "recording",
            "elapsed_s": 1.5, "peak": 0.4, "rms": 0.05, "segments": 2,
        })
        self.assertTrue(ok)
        self.assertEqual(self.overlay.calls, [("recording", 1.5, 0.4, 0.05, 2)])

    def test_recording_defaults_missing_fields(self):
        self.assertTrue(dispatch(self.overlay, {"type": "recording"}))
        self.assertEqual(self.overlay.calls, [("recording", 0.0, 0.0, 0.0, 0)])

    def test_status_segment_clear(self):
        dispatch(self.overlay, {"type": "status", "text": "transcribing"})
        dispatch(self.overlay, {"type": "segment", "text": "hello"})
        dispatch(self.overlay, {"type": "clear"})
        self.assertEqual(self.overlay.calls, [
            ("status", "transcribing"), ("segment", "hello"), ("clear",),
        ])

    def test_state_maps_name_and_detail(self):
        self.assertTrue(
            dispatch(self.overlay, {"type": "state", "name": "loading_asr", "detail": "loading speech model"})
        )
        self.assertTrue(dispatch(self.overlay, {"type": "state", "name": "ready"}))
        self.assertEqual(
            self.overlay.calls,
            [("state", "loading_asr", "loading speech model"), ("state", "ready", "")],
        )

    def test_shutdown_stops_loop_without_calling_overlay(self):
        self.assertFalse(dispatch(self.overlay, {"type": "shutdown"}))
        self.assertEqual(self.overlay.calls, [])

    def test_system_palette_and_dismiss_map_typed_fields(self):
        suggestions = [{
            "suggestion_id": "session-7:suggestion-1",
            "title": "Open Google Chrome",
            "subtitle": "Application",
            "action_label": "Open",
        }]
        self.assertTrue(dispatch(self.overlay, {
            "type": "system_palette",
            "session_id": 7,
            "transcript": "open chrome",
            "suggestions": suggestions,
        }))
        self.assertTrue(dispatch(self.overlay, {
            "type": "dismiss_system_palette",
            "session_id": 7,
        }))
        self.assertEqual(self.overlay.calls, [
            ("system_palette", 7, "open chrome", suggestions),
            ("dismiss_system_palette", 7),
        ])

    def test_unknown_op_is_skipped(self):
        self.assertTrue(dispatch(self.overlay, {"type": "explode"}))
        self.assertTrue(dispatch(self.overlay, {}))
        self.assertEqual(self.overlay.calls, [])

    def test_bad_field_types_are_not_fatal(self):
        ok = dispatch(self.overlay, {"type": "recording", "peak": "loud"})
        self.assertTrue(ok)
        self.assertEqual(self.overlay.calls, [])


if __name__ == "__main__":
    unittest.main()

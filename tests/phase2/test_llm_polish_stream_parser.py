"""Unit tests for the LLM-polish streaming prefix parser.

Pure-stdlib unittest (no model load, no daemon). Exercises `_PrefixState` in
`services/polish/llm_polish_sidecar.py`, which strips the `EDIT: ` prefix and
detects the `OK` fast path from a raw token stream so the sidecar can emit
`polish_chunk` deltas for progressive insertion.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "polish"))

try:
    import llm_polish_sidecar as sidecar  # noqa: E402

    _HAS_SIDECAR = True
except Exception:  # pragma: no cover - llama-cpp-python unavailable in CI.
    sidecar = None  # type: ignore[assignment]
    _HAS_SIDECAR = False


def feed_all(state, deltas):
    """Drive a stream of deltas; collect (cleaned, decision, done) per step."""
    out = []
    for delta in deltas:
        out.append(state.feed(delta))
    return out


@unittest.skipUnless(_HAS_SIDECAR, "llama-cpp-python / sidecar not importable")
class PrefixStateEditTests(unittest.TestCase):
    def test_edit_prefix_split_across_deltas(self):
        state = sidecar._PrefixState()
        results = feed_all(state, ["EDIT", ": ", "Please ", "send this."])
        # "EDIT" alone is ambiguous (colon missing) -> not yet decided.
        self.assertEqual(results[0], (None, None, False))
        # ": " completes the prefix; no content yet.
        self.assertEqual(results[1], (None, "EDIT", False))
        self.assertEqual(results[2], ("Please ", "EDIT", False))
        self.assertEqual(results[3], ("send this.", "EDIT", False))

    def test_edit_prefix_newline_separator(self):
        state = sidecar._PrefixState()
        results = feed_all(state, ["EDIT:\n", "Hello world"])
        self.assertEqual(results[0], (None, "EDIT", False))
        self.assertEqual(results[1][:2], ("Hello world", "EDIT"))

    def test_edit_prefix_single_delta(self):
        state = sidecar._PrefixState()
        results = feed_all(state, ["EDIT: merged text here"])
        self.assertEqual(results[0][1], "EDIT")
        self.assertEqual(results[0][0], "merged text here")

    def test_empty_delta_after_edit_forwarded_as_none(self):
        state = sidecar._PrefixState()
        feed_all(state, ["EDIT: hi"])
        cleaned, decision, done = state.feed("")
        self.assertEqual((cleaned, decision, done), (None, "EDIT", False))


@unittest.skipUnless(_HAS_SIDECAR, "llama-cpp-python / sidecar not importable")
class PrefixStateOkTests(unittest.TestCase):
    def test_ok_fast_path_emits_no_chunks(self):
        state = sidecar._PrefixState()
        results = feed_all(state, ["OK"])
        self.assertEqual(results[0][1], "OK")
        self.assertTrue(results[0][2])
        self.assertIsNone(results[0][0])

    def test_ok_with_trailing_punctuation(self):
        state = sidecar._PrefixState()
        results = feed_all(state, ["OK."])
        self.assertEqual(results[0][1], "OK")
        self.assertTrue(results[0][2])

    def test_ok_then_more_deltas_stay_done(self):
        state = sidecar._PrefixState()
        state.feed("OK")
        cleaned, decision, done = state.feed(" more")
        self.assertEqual((cleaned, decision, done), (None, "OK", False))


if __name__ == "__main__":
    unittest.main()

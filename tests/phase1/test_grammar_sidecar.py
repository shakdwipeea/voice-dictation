"""Protocol tests for the Improve button's grammar helper. No model is loaded;
the MLX model itself is checked against PyTorch by
tools/improve/grammar_mlx_check.py."""

from __future__ import annotations

import io
import json
import math
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from types import SimpleNamespace

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "improve"))

import grammar_sidecar  # noqa: E402
from grammar_sidecar import Corrector, Server  # noqa: E402


def args(**overrides):
    values = {
        "model_dir": None,
        "repo": grammar_sidecar.DEFAULT_REPO,
        "revision": grammar_sidecar.DEFAULT_REVISION,
        "dtype": "bfloat16",
        "unload_after": 60.0,
        "exit_after": 300.0,
    }
    values.update(overrides)
    return SimpleNamespace(**values)


class _FakeCorrector:
    def __init__(self, fail: bool = False) -> None:
        self.fail = fail
        self.seen: list[str] = []

    def correct(self, text: str) -> str:
        self.seen.append(text)
        if self.fail:
            raise RuntimeError("model exploded")
        return text.replace("dont", "doesn't")


def run(server: Server, *messages: dict) -> list[dict]:
    out = io.StringIO()
    with redirect_stdout(out), redirect_stderr(io.StringIO()):
        for message in messages:
            server.handle(message)
    return [json.loads(line) for line in out.getvalue().splitlines()]


class GrammarSidecarProtocolTests(unittest.TestCase):
    def test_health_replies_ready_without_loading_the_model(self) -> None:
        server = Server(args())
        self.assertEqual(run(server, {"type": "health"}), [{"type": "ready", "backend": "grammar"}])
        self.assertIsNone(server.corrector)

    def test_improve_returns_the_corrected_text_for_its_session(self) -> None:
        server = Server(args())
        server.corrector = _FakeCorrector()
        [reply] = run(server, {"type": "improve", "session_id": 7, "text": "She dont like it."})
        self.assertEqual(reply["type"], "improved")
        self.assertEqual(reply["session_id"], 7)
        self.assertEqual(reply["text"], "She doesn't like it.")
        self.assertIsInstance(reply["ms"], int)

    def test_failures_are_reported_and_the_helper_keeps_running(self) -> None:
        server = Server(args())
        server.corrector = _FakeCorrector(fail=True)
        replies = run(
            server,
            {"type": "improve", "session_id": 3, "text": "x"},
            {"type": "nonsense"},
            {"type": "health"},
        )
        self.assertEqual([r["type"] for r in replies], ["error", "error", "ready"])
        self.assertEqual(replies[0]["session_id"], 3)
        self.assertIn("model exploded", replies[0]["message"])
        self.assertIn("nonsense", replies[1]["message"])

    def test_idle_unloads_the_model_then_exits(self) -> None:
        server = Server(args(unload_after=60, exit_after=300))
        server.corrector = _FakeCorrector()
        start = server.last_used
        with redirect_stderr(io.StringIO()):
            self.assertFalse(server.check_idle(start + 10))
            self.assertIsNotNone(server.corrector)
            self.assertFalse(server.check_idle(start + 61))
            self.assertIsNone(server.corrector)
            self.assertTrue(server.check_idle(start + 301))

    def test_unload_drops_the_model(self) -> None:
        server = Server(args())
        server.corrector = _FakeCorrector()
        self.assertEqual(run(server, {"type": "unload"}), [])
        self.assertIsNone(server.corrector)

    def test_long_dictations_are_corrected_one_sentence_at_a_time(self) -> None:
        corrector = Corrector.__new__(Corrector)
        calls: list[str] = []
        corrector.sentence = lambda text: calls.append(text) or text.upper()
        result = corrector.correct("first one is here. Second at 5 pm! third stays together")
        self.assertEqual(calls, ["first one is here.", "Second at 5 pm! third stays together"])
        self.assertEqual(result, "FIRST ONE IS HERE. SECOND AT 5 PM! THIRD STAYS TOGETHER")

    def test_the_model_revision_is_pinned(self) -> None:
        self.assertRegex(grammar_sidecar.DEFAULT_REVISION, r"^[0-9a-f]{40}$")


def _reference_bucket(relative: int, bidirectional: bool, num_buckets: int, max_distance: int) -> int:
    """transformers' T5Attention._relative_position_bucket, for one value."""
    bucket = 0
    if bidirectional:
        num_buckets //= 2
        bucket += num_buckets if relative > 0 else 0
        relative = abs(relative)
    else:
        relative = -min(relative, 0)
    max_exact = num_buckets // 2
    if relative < max_exact:
        return bucket + relative
    large = max_exact + int(
        math.log(relative / max_exact) / math.log(max_distance / max_exact) * (num_buckets - max_exact)
    )
    return bucket + min(large, num_buckets - 1)


try:
    import mlx.core as mx
except ImportError:  # the tests run without MLX on Linux
    mx = None


@unittest.skipIf(mx is None, "MLX is not installed")
class RelativeBucketTests(unittest.TestCase):
    def test_buckets_match_transformers(self) -> None:
        values = list(range(-300, 301))
        for bidirectional in (True, False):
            got = grammar_sidecar.relative_bucket(mx.array(values), bidirectional, 32, 128).tolist()
            expected = [_reference_bucket(v, bidirectional, 32, 128) for v in values]
            self.assertEqual(got, expected, f"bidirectional={bidirectional}")


if __name__ == "__main__":
    unittest.main()

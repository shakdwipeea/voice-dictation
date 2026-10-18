"""Word boosting: matching, bonuses, respelling, and the decode loop.

The decode-loop tests drive a scripted fake model and need MLX; the rest are
plain Python.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "services" / "asr"))

import word_boost  # noqa: E402
from nemotron_sidecar import SidecarServer  # noqa: E402
from word_boost import TokenIndex, WordBooster, word_parts  # noqa: E402

try:
    import mlx.core as mx  # noqa: F401

    HAVE_MLX = True
except ImportError:
    HAVE_MLX = False

S = word_boost.SPACE
VOCAB = [
    f"{S}s", f"{S}so", f"{S}sun", "u", "n", "o", "t", "to", "y", f"{S}the", f"{S}husky",
    f"{S}A", "I", f"{S}G", "host", "ty", f"{S}su", ".", "<|nospeech|>",
]
ID = {piece: index for index, piece in enumerate(VOCAB)}


class PartsTest(unittest.TestCase):
    def test_parts_match_the_daemon(self):
        self.assertEqual(word_parts("huskyAI"), ["husky", "AI"])
        self.assertEqual(word_parts("tinySDS3d"), ["tiny", "SDS", "3", "d"])
        self.assertEqual(word_parts("CGEventTap"), ["CG", "Event", "Tap"])
        self.assertEqual(word_parts("llama.cpp"), ["llama", "cpp"])


class BoosterTest(unittest.TestCase):
    def setUp(self):
        self.index = TokenIndex(VOCAB)

    def test_index_skips_special_and_punctuation_pieces(self):
        found = {VOCAB[t] for t, _, _ in self.index.prefixes("sunoto")}
        self.assertEqual(found, {f"{S}s", f"{S}su", f"{S}sun"})
        self.assertNotIn(ID["<|nospeech|>"], self.index.spaced_letters)

    def test_strongest_tier_wins_and_short_words_drop(self):
        booster = WordBooster(
            self.index, {"everywhere": ["Sunoto", "ab"], "screen": ["sunoto"]}
        )
        self.assertEqual([t.written for t in booster.targets], ["sunoto"])
        self.assertEqual(booster.targets[0].bonus, word_boost.TIER_BONUS["screen"])

    def test_a_busy_screen_counts_past_its_top_words_like_project_words(self):
        screen = [f"zq{chr(97 + i // 26)}{chr(97 + i % 26)}x" for i in range(40)]
        booster = WordBooster(self.index, {"screen": screen})
        bonuses = [target.bonus for target in booster.targets]
        full = word_boost.SCREEN_FULL_WORDS
        self.assertEqual(set(bonuses[:full]), {word_boost.TIER_BONUS["screen"]})
        self.assertEqual(set(bonuses[full:]), {word_boost.TIER_BONUS["project"]})

    def test_start_bonus_is_a_share_scaled_by_piece_length(self):
        booster = WordBooster(self.index, {"screen": ["sunoto"]}, bonus={"screen": 4.0, "project": 0, "everywhere": 0})
        share = word_boost.START_SHARE
        self.assertNotIn(ID[f"{S}s"], booster.start_bonus)  # one letter: no push
        self.assertIn(ID[f"{S}s"], booster.starts)  # but followed when picked
        self.assertAlmostEqual(booster.start_bonus[ID[f"{S}sun"]], 4.0 * share * 0.75)

    def test_a_match_follows_the_word_and_ends_with_it(self):
        booster = WordBooster(self.index, {"screen": ["sunoto"]})
        states = booster.advance((), ID[f"{S}sun"])
        self.assertEqual(states, ((0, 3),))
        self.assertIn(ID["o"], booster.candidates(states))
        states = booster.advance(states, ID["o"])
        self.assertEqual(states, ((0, 4),))
        self.assertEqual(booster.advance(states, ID["to"]), ())  # complete
        self.assertEqual(booster.advance(states, ID["y"]), ())  # broken

    def test_a_space_inside_a_word_only_at_its_parts(self):
        booster = WordBooster(self.index, {"screen": ["huskyAI"]})
        states = booster.advance((), ID[f"{S}husky"])
        self.assertIn(ID[f"{S}A"], booster.candidates(states))
        plain = WordBooster(self.index, {"screen": ["sunoto"]})
        states = plain.advance((), ID[f"{S}sun"])
        self.assertNotIn(ID[f"{S}so"], plain.candidates(states))

    def test_start_bonus_never_beats_silence_or_a_longer_start(self):
        booster = WordBooster(self.index, {"screen": ["sunoto"]})
        blank = len(VOCAB)
        self.assertFalse(booster.start_may_replace(blank, ID[f"{S}s"]))
        self.assertFalse(booster.start_may_replace(ID["o"], ID[f"{S}s"]))  # mid-word
        self.assertFalse(booster.start_may_replace(ID[f"{S}so"], ID[f"{S}s"]))
        self.assertTrue(booster.start_may_replace(ID[f"{S}the"], ID[f"{S}sun"]))

    def test_respell_joins_parts_and_keeps_sentence_case(self):
        booster = WordBooster(self.index, {"screen": ["huskyAI", "Ghostty", "tinySDS3d"]})
        self.assertEqual(
            booster.respell("Open Husky AI and ghostty. Tiny SDS 3D works."),
            "Open huskyAI and Ghostty. tinySDS3d works.",
        )
        plain = WordBooster(self.index, {"screen": ["minisign", "uniblox"]})
        self.assertEqual(
            plain.respell("Sign it with miniSign. Uniblox is up, unlike Uniblox."),
            "Sign it with minisign. Uniblox is up, unlike Uniblox.",
        )
        lower = WordBooster(self.index, {"screen": ["vaani livekit"]})
        self.assertEqual(lower.respell("Vaani LiveKit is up."), "Vaani livekit is up.")
        self.assertEqual(lower.respell("the vaani Livekit"), "the vaani livekit")
        self.assertEqual(lower.respell("the Vaani live kit"), "the Vaani live kit")


class ProtocolTest(unittest.TestCase):
    class Engine:
        backend = "fake"

        def __init__(self):
            self.words = []

        def start(self, profile_ms):
            pass

        def set_words(self, words):
            self.words.append(words)

        def cancel(self):
            pass

    def test_session_words_reach_the_engine_quietly(self):
        engine = self.Engine()
        server = SidecarServer(engine)
        server.handle_line('{"type":"start_session","session_id":4,"profile_ms":160}')
        events = server.handle_line(
            '{"type":"session_words","session_id":4,"screen":["Sunoto"],"project":[],"everywhere":["uniblox"]}'
        )
        self.assertEqual(events, [])
        self.assertEqual(engine.words, [{"screen": ["Sunoto"], "project": [], "everywhere": ["uniblox"]}])

    def test_stale_session_words_are_dropped(self):
        engine = self.Engine()
        server = SidecarServer(engine)
        server.handle_line('{"type":"start_session","session_id":5,"profile_ms":160}')
        self.assertEqual(server.handle_line('{"type":"session_words","session_id":4,"screen":["x"]}'), [])
        self.assertEqual(engine.words, [])


@unittest.skipUnless(HAVE_MLX, "needs MLX")
class DecodeLoopTest(unittest.TestCase):
    """A fake TDT model whose joint plays back one logit row per step."""

    class Model:
        def __init__(self, rows):
            import mlx.core as mx

            self.mx = mx
            self.vocabulary = VOCAB
            self.durations = [0, 1]
            self.max_symbols = 10
            self.time_ratio = 0.08
            self.rows = list(rows)

        def decoder(self, token, hidden):
            zeros = self.mx.zeros((1, 1, 2))
            return zeros, (zeros, zeros)

        def joint(self, feature, decoder_out):
            wanted = self.rows.pop(0)
            logits = [-20.0] * (len(VOCAB) + 1) + [-20.0, 0.0]  # always advance one frame
            for piece, value in wanted.items():
                logits[len(VOCAB) if piece == "blank" else ID[piece]] = value
            return self.mx.array(logits).reshape(1, 1, 1, -1)

    def decode(self, rows, words):
        import mlx.core as mx

        model = self.Model(rows)
        booster = WordBooster(TokenIndex(VOCAB), words)
        features = mx.zeros((1, len(rows), 2))
        tokens = word_boost.decode_greedy(model, mx, features, len(rows), booster)
        return "".join(token.text for token in tokens)

    def test_bonus_carries_a_started_word_through(self):
        rows = [{f"{S}sun": -0.1}, {"y": -0.1, "o": -2.0}, {"to": -0.1}]
        self.assertEqual(self.decode(rows, {"everywhere": ["zzzzz"]}), " suny" + "to")
        self.assertEqual(self.decode(rows, {"screen": ["sunoto"]}), " sunoto")

    def test_a_non_finite_step_falls_back_to_the_stock_choice(self):
        rows = [{f"{S}so": float("inf"), f"{S}sun": -0.1}, {"blank": -0.1}]
        self.assertEqual(self.decode(rows, {"screen": ["sunoto"]}), " so")

    def test_start_bonus_leaves_a_longer_start_and_silence_alone(self):
        # "sun" is not how "suzuki" goes on, so the model's longer start stays;
        # then the bonus for "su" cannot fill a silence.
        rows = [{f"{S}sun": -0.5, f"{S}su": -1.0}, {"blank": -0.1, f"{S}su": -1.5}]
        self.assertEqual(self.decode(rows, {"screen": ["suzuki"]}), " sun")


if __name__ == "__main__":
    unittest.main()

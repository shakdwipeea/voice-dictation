"""Favour the user's own words while Parakeet-TDT decodes ("word boosting").

For each dictation the daemon sends a short list of unusual words: names on
the screen, in the current project, and among the user's folders and apps.
While the greedy TDT decoder picks each token, tokens that spell the start or
the next part of one of those words get a bonus on their log-probability. The
bonus only decides between pieces the model already finds likely, so a word on
the list changes nothing unless what was said sounds like it.

A word is matched on its letters, ignoring case. Inside a word, a piece may
start with a space only where the written form starts a new part ("husky AI"
for ``huskyAI``), and ``respell`` then writes the word the way it is written.

With no words, callers use the stock parakeet-mlx decoder, so nothing changes.
"""

from __future__ import annotations

import re

SPACE = "▁"  # SentencePiece's word-start marker
_END = ""  # trie key holding the tokens that end at a node

# Log-probability bonus per token (nats), by where the word came from. Tuned
# with tools/vocab/bench_words.py: stronger made rare names the model was
# unsure of ("Calico", "Leighton") turn into words from the list.
TIER_BONUS = {"screen": 7.0, "project": 4.5, "everywhere": 3.0}
TIERS = tuple(TIER_BONUS)  # strongest first; a word keeps its strongest tier
# A busy screen (a terminal full of code) lists many rare words, and each one
# is another chance to misfire on a rare word said aloud. The daemon sends the
# most frequent first; past this many they count like project words.
SCREEN_FULL_WORDS = 30
# A word's first piece is a candidate at every word start, so it gets only
# part of the bonus, and less still when it is short and starts many words.
# One-letter starts ("A", "s") get none: they tipped "Uh" into "A".
START_SHARE = 0.5
START_FULL_LETTERS = 4
START_MIN_LETTERS = 2
MIN_LETTERS = 3
MAX_WORDS = 600

_PART = re.compile(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+|\d+|[^\W\d_]+")


def word_parts(word: str) -> list[str]:
    """Split a written word into the parts people say: ``huskyAI`` -> husky, AI."""
    return _PART.findall(word)


class TokenIndex:
    """The model's token pieces, in a trie keyed by their lowercase letters."""

    def __init__(self, vocabulary: list[str]) -> None:
        self.root: dict = {}
        self.spaced_letters: dict[int, str] = {}  # word-start pieces only
        for token_id, piece in enumerate(vocabulary):
            spaced = piece.startswith(SPACE)
            core = piece[1:] if spaced else piece
            if not core or not core.isalnum():
                continue
            if spaced:
                self.spaced_letters[token_id] = core.lower()
            node = self.root
            for char in core.lower():
                node = node.setdefault(char, {})
            node.setdefault(_END, []).append((token_id, spaced))

    def prefixes(self, letters: str):
        """Yield (token_id, spaced, length) for every piece that begins ``letters``."""
        node = self.root
        for length, char in enumerate(letters, start=1):
            node = node.get(char)
            if node is None:
                return
            for token_id, spaced in node.get(_END, ()):
                yield token_id, spaced, length


class Target:
    __slots__ = ("written", "letters", "boundaries", "bonus", "_pattern")

    def __init__(self, written: str, parts: list[str], bonus: float) -> None:
        self.written = written
        self.letters = "".join(parts).lower()
        self.boundaries = set()
        offset = 0
        for part in parts[:-1]:
            offset += len(part)
            self.boundaries.add(offset)
        self.bonus = bonus
        self._pattern = None

    def pattern(self):
        if self._pattern is None:
            parts = word_parts(self.written)
            body = r"[ \-]?".join(re.escape(part) for part in parts)
            self._pattern = re.compile(rf"(?<!\w){body}(?!\w)", re.IGNORECASE)
        return self._pattern


class WordBooster:
    """One dictation's words, ready for the decoder."""

    def __init__(self, index: TokenIndex, words: dict, bonus: dict | None = None) -> None:
        bonus = TIER_BONUS if bonus is None else bonus
        self.index = index
        self.targets: list[Target] = []
        seen = set()
        for tier in TIERS:
            for rank, word in enumerate(words.get(tier) or ()):
                if len(self.targets) >= MAX_WORDS:
                    break
                level = "project" if tier == "screen" and rank >= SCREEN_FULL_WORDS else tier
                if not isinstance(word, str):
                    continue
                word = word.strip()
                parts = word_parts(word)
                letters = "".join(parts).lower()
                if len(letters) < MIN_LETTERS or letters in seen:
                    continue
                seen.add(letters)
                self.targets.append(Target(word, parts, bonus[level]))
        # Pieces that start a word, and the bonus each one gets for it.
        self.starts: dict[int, list[tuple[int, int]]] = {}
        self.start_bonus: dict[int, float] = {}
        for k, target in enumerate(self.targets):
            for token_id, spaced, length in index.prefixes(target.letters):
                if not spaced:
                    continue
                self.starts.setdefault(token_id, []).append((k, length))
                if length < START_MIN_LETTERS:
                    continue  # followed when the model picks it, never pushed
                share = START_SHARE * min(1.0, length / START_FULL_LETTERS)
                value = target.bonus * share
                if value > self.start_bonus.get(token_id, 0.0):
                    self.start_bonus[token_id] = value
        self._next: dict[tuple[int, int], dict[int, int]] = {}
        self._start_vector = None
        # Compiled now, while the user is still talking, so respelling after
        # the tap is only the search. Plain lowercase words share one pattern.
        self._respell = [
            (target.pattern(), target.written)
            for target in self.targets
            if target.boundaries or not target.written.islower()
        ]
        plain = {t.written.lower(): t.written for t in self.targets if not t.boundaries and t.written.islower()}
        self._plain = plain
        self._plain_pattern = (
            re.compile(
                r"(?<!\w)(?:" + "|".join(map(re.escape, sorted(plain, key=len, reverse=True))) + r")(?!\w)",
                re.IGNORECASE,
            )
            if plain
            else None
        )

    def __bool__(self) -> bool:
        return bool(self.targets)

    def start_vector(self, mx, size: int):
        """The start bonus of every token as one MLX vector (blank gets none)."""
        if self._start_vector is None:
            values = [0.0] * size
            for token_id, value in self.start_bonus.items():
                if token_id < size - 1:
                    values[token_id] = value
            self._start_vector = mx.array(values, dtype=mx.float32)
        return self._start_vector

    def _continuations(self, k: int, offset: int) -> dict[int, int]:
        key = (k, offset)
        found = self._next.get(key)
        if found is None:
            target = self.targets[k]
            found = {}
            for token_id, spaced, length in self.index.prefixes(target.letters[offset:]):
                if spaced and offset not in target.boundaries:
                    continue
                found[token_id] = offset + length
            self._next[key] = found
        return found

    def candidates(self, states) -> dict[int, float]:
        """Tokens that carry an open match one piece further, with their bonus."""
        bonus: dict[int, float] = {}
        for k, offset in states:
            value = self.targets[k].bonus
            for token_id in self._continuations(k, offset):
                if value > bonus.get(token_id, 0.0):
                    bonus[token_id] = value
        return bonus

    def start_may_replace(self, wanted: int, boosted: int) -> bool:
        """Whether a start bonus may replace the piece the model wanted.

        Only another word start may lose to it: never silence (blank), and
        never a longer start with the same letters ("Mar" for "Ma", "sw" for
        "s"), which says the model hears a different word.
        """
        letters = self.index.spaced_letters.get(wanted)
        if letters is None:
            return False
        if wanted in self.start_bonus:
            return True
        return not letters.startswith(self.index.spaced_letters.get(boosted, "\0"))

    def advance(self, states, token_id: int) -> tuple:
        """The open matches after the decoder emits ``token_id``."""
        moved = set()
        for k, offset in states:
            reached = self._continuations(k, offset).get(token_id)
            if reached is not None and reached < len(self.targets[k].letters):
                moved.add((k, reached))
        for k, length in self.starts.get(token_id, ()):
            if length < len(self.targets[k].letters):
                moved.add((k, length))
        return tuple(moved)

    def respell(self, text: str) -> str:
        """Write each word on the list the way it is written ("husky AI" ->
        "huskyAI", "miniSign" -> "minisign"). A lowercase name keeps a capital
        the model gave its first letter (a sentence start, a brand such as
        "Airbnb"), and nothing else."""

        def replace(match, written=None):
            found = match.group(0)
            written = written or self._plain[found.lower()]
            if written.islower() and found[:1].isupper():
                return written[:1].upper() + written[1:]
            return written

        for pattern, written in self._respell:
            text = pattern.sub(lambda match, written=written: replace(match, written), text)
        if self._plain_pattern is not None:
            text = self._plain_pattern.sub(replace, text)
        return text


def decode_greedy(model, mx, features, length: int, booster: WordBooster):
    """parakeet-mlx's ``ParakeetTDT.decode_greedy`` for one utterance, with the bonus.

    Mirrors the stock loop step for step; the only change is how the next
    token is chosen.
    """
    from parakeet_mlx import tokenizer  # noqa: delayed; MLX runtime only
    from parakeet_mlx.alignment import AlignedToken  # noqa: delayed

    blank = len(model.vocabulary)
    start_vector = booster.start_vector(mx, blank + 1)
    feature = features[0:1]
    hypothesis = []
    last_token = None
    hidden_state = None
    states: tuple = ()
    step = 0
    new_symbols = 0
    while step < length:
        decoder_out, (hidden, cell) = model.decoder(
            mx.array([[last_token]]) if last_token is not None else None,
            hidden_state,
        )
        decoder_out = decoder_out.astype(feature.dtype)
        decoder_hidden = (hidden.astype(feature.dtype), cell.astype(feature.dtype))
        joint_out = model.joint(feature[:, step : step + 1], decoder_out)

        token_logits = joint_out[0, 0, :, : blank + 1]
        logits = token_logits.reshape(-1).astype(mx.float32)
        logp = logits - mx.logsumexp(logits)
        boosted = logp + start_vector
        best = mx.argmax(boosted, keepdims=True)
        plain = mx.argmax(logp)
        bonus = booster.candidates(states)
        if bonus:
            ids = list(bonus)
            extra = [max(bonus[t], booster.start_bonus.get(t, 0.0)) for t in ids]
            scores = mx.concatenate(
                [mx.take(boosted, best), mx.take(logp, mx.array(ids)) + mx.array(extra)]
            )
            mx.eval(best, plain, scores)
            scores = scores.tolist()
            pick = max(range(len(scores)), key=scores.__getitem__)
            pred_token = int(best.item()) if pick == 0 else ids[pick - 1]
        else:
            mx.eval(best, plain)
            pred_token = int(best.item())
        wanted = int(plain.item())
        if (
            pred_token != wanted
            and pred_token not in bonus
            and not booster.start_may_replace(wanted, pred_token)
        ):
            pred_token = wanted

        token_probs = mx.softmax(token_logits, axis=-1)
        entropy = -mx.sum(token_probs * mx.log(token_probs + 1e-10), axis=-1)
        max_entropy = mx.log(mx.array(blank + 1, dtype=token_probs.dtype))
        confidence = float(1.0 - (entropy / max_entropy))

        decision = int(mx.argmax(joint_out[0, 0, :, blank + 1 :]))

        if pred_token != blank:
            hypothesis.append(
                AlignedToken(
                    int(pred_token),
                    start=step * model.time_ratio,
                    duration=model.durations[decision] * model.time_ratio,
                    confidence=confidence,
                    text=tokenizer.decode([pred_token], model.vocabulary),
                )
            )
            last_token = pred_token
            hidden_state = decoder_hidden
            states = booster.advance(states, pred_token)

        step += model.durations[decision]
        new_symbols += 1
        if model.durations[decision] != 0:
            new_symbols = 0
        elif model.max_symbols is not None and model.max_symbols <= new_symbols:
            step += 1
            new_symbols = 0
    return hypothesis


def generate(model, mx, mel, booster: WordBooster):
    """``model.generate(mel)[0]`` for one utterance, decoding with ``booster``."""
    from parakeet_mlx.alignment import (  # noqa: delayed; MLX runtime only
        sentences_to_result,
        tokens_to_sentences,
    )

    if len(mel.shape) == 2:
        mel = mx.expand_dims(mel, 0)
    features, lengths = model.encoder(mel)
    mx.eval(features, lengths)
    tokens = decode_greedy(model, mx, features, int(lengths[0]), booster)
    return sentences_to_result(tokens_to_sentences(tokens))

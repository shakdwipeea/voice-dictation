from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from voice_dictation.technical_vocabulary import (
    apply_technical_corrections,
    build_vocabulary,
    build_whisper_context,
    load_vocabulary_file,
)


class TechnicalVocabularyTests(unittest.TestCase):
    def test_load_vocabulary_file_ignores_comments_and_blanks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "vocabulary.txt"
            path.write_text("\n# comment\nAmp\nSourcegraph # inline comment\n", encoding="utf-8")

            self.assertEqual(load_vocabulary_file(path), ["Amp", "Sourcegraph"])

    def test_build_vocabulary_dedupes_case_insensitively(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "vocabulary.txt"
            path.write_text("typescript\nMyProject\n", encoding="utf-8")

            terms = build_vocabulary(path)

            self.assertIn("TypeScript", terms)
            self.assertIn("MyProject", terms)
            self.assertEqual(sum(1 for term in terms if term.casefold() == "typescript"), 1)

    def test_build_whisper_context_is_none_without_terms(self) -> None:
        self.assertEqual(build_whisper_context([]), (None, None))

    def test_build_whisper_context_contains_terms(self) -> None:
        prompt, hotwords = build_whisper_context(["origin/master", "kubectl"])

        self.assertEqual(hotwords, "origin/master, kubectl")
        self.assertIsNotNone(prompt)
        self.assertIn("origin/master", prompt or "")

    def test_technical_corrections_fix_common_programming_misrecognitions(self) -> None:
        cases = {
            "checkout origin slash monsters": "checkout origin/master",
            "rebase onto origin forward slash main": "rebase onto origin/main",
            "run cube cuddle get pods": "run kubectl get pods",
            "the type script compiler wrote to standard error": "the TypeScript compiler wrote to stderr",
            "parse the java script JSON": "parse the JavaScript JSON",
            "connect to post grass q l on local host": "connect to PostgreSQL on localhost",
        }

        for raw, expected in cases.items():
            with self.subTest(raw=raw):
                self.assertEqual(apply_technical_corrections(raw), expected)

    def test_technical_corrections_are_conservative(self) -> None:
        text = "the monsters used a slash in the story"

        self.assertEqual(apply_technical_corrections(text), text)


if __name__ == "__main__":
    unittest.main()

"""End-to-end test of the corpus transcriber against the mock sidecar."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
import wave
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "tools" / "phase2"))

import transcribe_corpus  # noqa: E402


def write_silent_wav(path: Path, samples: int = 1600) -> None:
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(16_000)
        wav.writeframes(b"\x00\x00" * samples)


class TranscribeCorpusTest(unittest.TestCase):
    def test_mock_sidecar_fills_raw_fields(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sunoto-phase2-") as tmp:
            tmpdir = Path(tmp)
            write_silent_wav(tmpdir / "case-a.wav")
            manifest = {
                "kind": "recorded",
                "cases": [
                    {"id": "case-a", "audio": "case-a.wav", "raw": "", "expected": "x"},
                    {"id": "missing", "audio": "missing.wav", "raw": "", "expected": "y"},
                ],
            }
            command = transcribe_corpus.sidecar_command("mock", 160, None)
            sidecar = transcribe_corpus.Sidecar(command)
            try:
                transcribe_corpus.transcribe_all(manifest, tmpdir, sidecar)
            finally:
                sidecar.close()
            self.assertEqual(
                manifest["cases"][0]["raw"], "Sunoto Phase 1 insertion works."
            )
            # A case without audio is skipped, never invented.
            self.assertEqual(manifest["cases"][1]["raw"], "")

    def test_wav_format_is_validated(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sunoto-phase2-") as tmp:
            path = Path(tmp) / "bad.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(2)
                wav.setsampwidth(2)
                wav.setframerate(44_100)
                wav.writeframes(b"\x00\x00\x00\x00" * 100)
            with self.assertRaises(RuntimeError):
                transcribe_corpus.read_samples(path)

    def test_unknown_backend_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            transcribe_corpus.sidecar_command("imaginary", 160, None)


if __name__ == "__main__":
    unittest.main()

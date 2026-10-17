#!/usr/bin/env python3
"""Build the audio for the word-list benchmark (bench_words.py).

Writes 16 kHz mono WAVs and a manifest to build/vocab/:
  - names:   every case in name_cases.jsonl, spoken by several macOS voices;
  - ordinary: sentences with no names, to catch words the list breaks:
              your own dictation history (macOS voices), and the LibriSpeech
              dummy set (real people) when it is in the Hugging Face cache.

Nothing here is tracked: history text and audio stay in build/.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
OUT = REPO / "build" / "vocab"
VOICES = ("Rishi", "Samantha", "Daniel")
HISTORY = Path.home() / "Library/Application Support/sunoto/history/dictations"


def speak(text: str, voice: str, path: Path) -> None:
    aiff = path.with_suffix(".aiff")
    subprocess.run(["say", "-v", voice, "-o", str(aiff), text], check=True)
    subprocess.run(
        ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(path)],
        check=True,
    )
    aiff.unlink()


def history_sentences(limit: int) -> list[str]:
    seen, out = set(), []
    for day in sorted(HISTORY.glob("*.jsonl")):
        for line in day.read_text().splitlines():
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            text = record.get("text") if isinstance(record, dict) else None
            raw = text.get("raw") if isinstance(text, dict) else None
            if not raw or len(raw.split()) < 4 or raw in seen:
                continue
            seen.add(raw)
            out.append(raw)
    return out[:limit]


def librispeech(out_dir: Path) -> list[dict]:
    try:
        import io

        import numpy as np
        import soundfile
        from datasets import Audio, load_dataset
    except ImportError:
        print("datasets/numpy missing; skipping LibriSpeech", file=sys.stderr)
        return []
    try:
        rows = load_dataset("hf-internal-testing/librispeech_asr_dummy", "clean", split="validation")
        rows = rows.cast_column("audio", Audio(decode=False))
    except Exception as error:  # noqa: BLE001 - offline without the cache
        print(f"LibriSpeech unavailable ({error}); skipping", file=sys.stderr)
        return []
    cases = []
    for i, row in enumerate(rows):
        samples, rate = soundfile.read(io.BytesIO(row["audio"]["bytes"]), dtype="float32")
        if rate != 16000:
            continue
        path = out_dir / f"libri-{i:03d}.wav"
        pcm = np.clip(np.round(samples * 32768), -32768, 32767).astype("<i2")
        with wave.open(str(path), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes(pcm.tobytes())
        cases.append({"id": f"libri-{i:03d}", "set": "ordinary", "voice": "human",
                      "text": row["text"], "audio": str(path)})
    return cases


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--history", type=int, default=30, help="history sentences to use")
    args = parser.parse_args()
    audio_dir = OUT / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    manifest = []
    for line in (HERE / "name_cases.jsonl").read_text().splitlines():
        case = json.loads(line)
        for voice in VOICES:
            path = audio_dir / f"{case['id']}-{voice.lower()}.wav"
            speak(case["say"], voice, path)
            manifest.append({**case, "set": "names", "voice": voice, "audio": str(path)})
    for i, sentence in enumerate(history_sentences(args.history)):
        for voice in VOICES[:2]:
            path = audio_dir / f"history-{i:02d}-{voice.lower()}.wav"
            speak(sentence, voice, path)
            manifest.append({"id": f"history-{i:02d}", "set": "ordinary", "voice": voice,
                             "text": sentence, "audio": str(path)})
    manifest += librispeech(audio_dir)
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=1))
    counts = {}
    for case in manifest:
        counts[case["set"]] = counts.get(case["set"], 0) + 1
    print(f"wrote {len(manifest)} clips to {audio_dir}: {counts}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

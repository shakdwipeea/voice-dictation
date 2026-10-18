#!/usr/bin/env python3
"""Noisy copies of the persona clips: other people talking in the background.

Two LibriSpeech test-other speakers are mixed under each clip at a chosen
signal-to-noise ratio, with half a second of that babble before and after
the sentence (a TV or a colleague nearby). Writes build/personas/noisy-<snr>/
and a manifest that bench_personas.py takes with --manifest.

  .venv-nemotron-mac/bin/python tools/vocab/make_noisy.py --snr 10 --voices Samantha,Rishi
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import random
import wave
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq
import soundfile

OUT = Path(__file__).resolve().parents[2] / "build" / "personas"


def babble_source():
    path = glob.glob(str(Path.home() / ".cache/huggingface/hub/datasets--openslr--librispeech_asr/snapshots/*/other/test/0000.parquet"))[0]
    table = pq.read_table(path, columns=["audio"]).slice(0, 300)
    clips = []
    for audio in table.column("audio").to_pylist():
        samples, _ = soundfile.read(io.BytesIO(audio["bytes"]), dtype="float32")
        clips.append(samples)
    return clips


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snr", type=float, default=10.0)
    parser.add_argument("--voices", default="Samantha,Rishi")
    args = parser.parse_args()
    voices = set(args.voices.split(","))
    rng = random.Random(int(args.snr * 10))
    babble = babble_source()
    out_dir = OUT / f"noisy-{args.snr:g}"
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest = []
    for case in json.loads((OUT / "manifest.json").read_text()):
        if case["voice"] not in voices:
            continue
        with wave.open(case["audio"], "rb") as wav:
            speech = np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2").astype(np.float32) / 32768
        pad = 8000
        length = len(speech) + 2 * pad
        noise = np.zeros(length, dtype=np.float32)
        for _ in range(2):
            source = babble[rng.randrange(len(babble))]
            if len(source) < length:
                source = np.tile(source, length // len(source) + 1)
            start = rng.randrange(len(source) - length + 1)
            noise += source[start : start + length]
        signal = np.concatenate([np.zeros(pad, np.float32), speech, np.zeros(pad, np.float32)])
        power = lambda x: float(np.mean(x[np.abs(x) > 1e-4] ** 2)) if np.any(np.abs(x) > 1e-4) else 1e-9  # noqa: E731
        noise *= np.sqrt(power(speech) / (power(noise) * 10 ** (args.snr / 10)))
        mixed = np.clip(signal + noise, -1, 1)
        path = out_dir / Path(case["audio"]).name
        with wave.open(str(path), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes((mixed * 32767).astype("<i2").tobytes())
        manifest.append({**case, "audio": str(path), "noise": f"babble {args.snr:g} dB"})
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=1))
    print(f"wrote {len(manifest)} clips at {args.snr:g} dB SNR to {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

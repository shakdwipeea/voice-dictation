#!/usr/bin/env python3
"""Does the engine write text when nobody dictated? A dictation app pastes
whatever comes back, so invented words on a stray tap are worse than a
misspelled name. Feeds silence, faint noise, and background talk with no
speaker (two LibriSpeech voices, quiet and loud), with and without a list.

  .venv-nemotron-mac/bin/python tools/vocab/bench_silence.py --engine parakeet
  build/venv-qwen3-asr/bin/python tools/vocab/bench_silence.py --engine qwen3-bf16
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import sys
from array import array
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "services" / "asr"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"


def clips():
    import pyarrow.parquet as pq
    import soundfile

    rng = np.random.default_rng(7)
    out = []
    for seconds in (1, 3, 6):
        out.append((f"silence {seconds}s", np.zeros(16000 * seconds, np.float32)))
        out.append((f"faint noise {seconds}s", rng.normal(0, 0.003, 16000 * seconds).astype(np.float32)))
    path = glob.glob(str(Path.home() / ".cache/huggingface/hub/datasets--openslr--librispeech_asr/snapshots/*/other/test/0000.parquet"))[0]
    voices = [soundfile.read(io.BytesIO(a["bytes"]), dtype="float32")[0] for a in pq.read_table(path, columns=["audio"]).slice(400, 20).column("audio").to_pylist()]
    for i in range(10):
        a, b = voices[2 * i][: 16000 * 4], voices[2 * i + 1][: 16000 * 4]
        n = min(len(a), len(b))
        babble = a[:n] + b[:n]
        for level, gain in (("quiet", 0.05), ("loud", 0.3)):
            out.append((f"background talk {level} #{i}", (babble / (np.abs(babble).max() + 1e-6) * gain).astype(np.float32)))
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", default="parakeet", choices=("parakeet", "qwen3-bf16", "qwen3-8bit"))
    args = parser.parse_args()
    lists = json.loads((REPO / "build/vocab/words-final.json").read_text())
    if args.engine == "parakeet":
        import parakeet_mlx_offline_sidecar as sidecar

        engine = sidecar.OfflineParakeetMlxEngine(segment_min_s=0, pause_decode=False, model_name=str(LOCAL_MODEL))
        run = lambda fn, *a: engine._worker.submit(fn, *a).result()  # noqa: E731

        def decode(pcm, words):
            run(engine._set_booster, words)
            text = run(engine._transcribe_pcm_i16, pcm)
            return engine._booster.respell(text) if engine._booster else text
    else:
        import qwen3_engine

        qwen = qwen3_engine.Qwen3Engine(args.engine)

        def decode(pcm, words):
            return qwen.transcribe(pcm, qwen3_engine.context_prompt(words))

    wrote = {"no list": 0, "list": 0}
    total = 0
    print(f"{args.engine}: text written with nobody dictating")
    for name, audio in clips():
        pcm = array("h", np.clip(np.round(audio * 32768), -32768, 32767).astype("<i2").tobytes())
        total += 1
        for mode, words in (("no list", None), ("list", lists)):
            text = decode(pcm, words).strip()
            if text:
                wrote[mode] += 1
                print(f"  {name:<24} {mode:<8} {text[:90]!r}")
    print(f"  wrote text on {wrote['no list']}/{total} clips without a list, {wrote['list']}/{total} with")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

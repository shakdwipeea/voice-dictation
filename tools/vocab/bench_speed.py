#!/usr/bin/env python3
"""Speed and memory of one speech engine on the same clips, for comparing
Parakeet (the app's sidecar) with Qwen3-ASR. Run each engine in its own
environment with the app stopped, one at a time:

  .venv-nemotron-mac/bin/python tools/vocab/bench_speed.py --engine parakeet
  build/venv-qwen3-asr/bin/python tools/vocab/bench_speed.py --engine qwen3-bf16

Decodes every clip with no list, a 25-word list, a 100-word list, and (for
Qwen3-ASR) a screen's text plus the list, with idle gaps left out (back to
back, so the GPU stays awake). Prints load time, decode time per clip and per
second of audio, MLX peak memory, and the process footprint (`footprint`).
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
import wave
from array import array
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "services" / "asr"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"


def read_wav(path: str) -> array:
    with wave.open(path, "rb") as wav:
        samples = array("h")
        samples.frombytes(wav.readframes(wav.getnframes()))
    return samples


def footprint() -> str:
    out = subprocess.run(["footprint", "-p", str(__import__("os").getpid())], capture_output=True, text=True).stdout
    keep = [line.strip() for line in out.splitlines() if "phys_footprint" in line]
    return " | ".join(keep)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", default="parakeet", choices=("parakeet", "qwen3-bf16", "qwen3-8bit"))
    parser.add_argument("--model", default=None,
                        help="Parakeet checkpoint (default: the app's local v3 copy), e.g. mlx-community/parakeet-tdt-0.6b-v2")
    parser.add_argument("--clips", type=int, default=60)
    args = parser.parse_args()
    import mlx.core as mx

    manifest = json.loads((REPO / "build/vocab/manifest.json").read_text())
    names = [c for c in manifest if c["set"] == "names"][:20]
    libri = [c for c in manifest if c["voice"] == "human"][: args.clips - len(names)]
    clips = [(c, read_wav(c["audio"])) for c in names + libri]
    seconds = sum(len(pcm) for _, pcm in clips) / 16000
    lists = json.loads((REPO / "build/vocab/words-heavy.json").read_text())
    pool = list(dict.fromkeys(lists["screen"] + lists["project"] + lists["everywhere"]))
    small = {"screen": pool[:25]}
    large = {"screen": pool[:100]}
    screen_text = (REPO / "build/personas/developer/screen.txt").read_text() * 6

    started = time.perf_counter()
    if args.engine == "parakeet":
        import parakeet_mlx_offline_sidecar as sidecar

        engine = sidecar.OfflineParakeetMlxEngine(segment_min_s=0, pause_decode=False, model_name=args.model or str(LOCAL_MODEL))
        run = lambda fn, *a: engine._worker.submit(fn, *a).result()  # noqa: E731

        def decode(pcm, words, _style):
            run(engine._set_booster, words)
            text = run(engine._transcribe_pcm_i16, pcm)
            return engine._booster.respell(text) if engine._booster else text

        modes = {"no list": None, "25 words": small, "100 words": large}
    else:
        import qwen3_engine

        qwen = qwen3_engine.Qwen3Engine(args.engine)

        def decode(pcm, words, style):
            return qwen.transcribe(pcm, qwen3_engine.context_prompt(words, screen_text, style))

        modes = {"no list": None, "25 words": small, "100 words": large, "screen + 25 words": small}
    load_s = time.perf_counter() - started
    print(f"{args.engine}: loaded and warmed in {load_s:.1f}s; {len(clips)} clips, {seconds:.0f}s of audio")
    print(f"  after load: {footprint()}")
    for _, pcm in clips[:3]:
        decode(pcm, None, "list")
    for mode, words in modes.items():
        style = "screen" if mode.startswith("screen") else "list"
        mx.reset_peak_memory()
        times = []
        for _, pcm in clips:
            t0 = time.perf_counter()
            decode(pcm, words, style)
            times.append((time.perf_counter() - t0) * 1000)
        times.sort()
        print(
            f"  {mode:<18} decode median {statistics.median(times):6.0f} ms, p90 {times[int(len(times) * 0.9)]:6.0f} ms, "
            f"{sum(times) / seconds:5.1f} ms per s of audio, MLX peak {mx.get_peak_memory() / 1e6:6.0f} MB"
        )
    print(f"  after decoding: {footprint()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

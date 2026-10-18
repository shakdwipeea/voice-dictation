#!/usr/bin/env python3
"""Is beam search fast enough to pair with word boosting? Greedy vs
parakeet-mlx's beam search on LibriSpeech test-clean: decode time and WER.

  .venv-nemotron-mac/bin/python tools/vocab/bench_beam.py --limit 150
"""

from __future__ import annotations

import argparse
import glob
import io
import re
import statistics
import time
from pathlib import Path

LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"


def words_of(text):
    return re.findall(r"[a-z0-9']+", text.lower().replace("-", " "))


def edit_distance(a, b):
    row = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        prev, row[0] = row[0], i
        for j, y in enumerate(b, 1):
            prev, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, prev + (x != y))
    return row[-1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--limit", type=int, default=150)
    parser.add_argument("--beams", default="2,4")
    args = parser.parse_args()
    import mlx.core as mx
    import numpy as np
    import pyarrow.parquet as pq
    import soundfile
    from parakeet_mlx import Beam, DecodingConfig, Greedy, from_pretrained
    from parakeet_mlx.audio import get_logmel

    model = from_pretrained(str(LOCAL_MODEL), dtype=mx.bfloat16)
    path = glob.glob(str(Path.home() / ".cache/huggingface/hub/datasets--openslr--librispeech_asr/snapshots/*/clean/test/0000.parquet"))[0]
    table = pq.read_table(path, columns=["audio", "text"]).slice(0, args.limit)
    configs = {"greedy": DecodingConfig(decoding=Greedy())}
    for beam in args.beams.split(","):
        configs[f"beam {beam}"] = DecodingConfig(decoding=Beam(beam_size=int(beam)))
    errors = {k: 0 for k in configs}
    times = {k: [] for k in configs}
    ref_words = 0
    seconds = 0.0
    for audio, text in zip(table.column("audio").to_pylist(), table.column("text").to_pylist()):
        samples, _ = soundfile.read(io.BytesIO(audio["bytes"]), dtype="float32")
        mel = get_logmel(mx.array(samples), model.preprocessor_config)
        ref = words_of(text)
        ref_words += len(ref)
        seconds += len(samples) / 16000
        for name, config in configs.items():
            t0 = time.perf_counter()
            hyp = model.generate(mel, decoding_config=config)[0].text
            times[name].append((time.perf_counter() - t0) * 1000)
            errors[name] += edit_distance(ref, words_of(hyp))
    print(f"{args.limit} utterances, {seconds / 60:.1f} min of audio")
    for name in configs:
        print(f"  {name:<8} WER {100 * errors[name] / ref_words:5.2f}%   decode median {statistics.median(times[name]):6.0f} ms, "
              f"{sum(times[name]) / seconds:5.0f} ms per s of audio")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

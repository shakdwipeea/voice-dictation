#!/usr/bin/env python3
"""Word boosting on the standard LibriSpeech rare-word benchmark.

Real people reading books; every utterance comes with a list of its rare
words hidden among distractors (Le et al., Interspeech 2021,
facebookresearch/fbai-speech/is21_deep_bias). Decodes each utterance with no
list and with the 100/500/1000-word lists, then scores every run with the
benchmark's score.py: WER, U-WER (common words) and B-WER (the rare words).

  .venv-nemotron-mac/bin/python tools/vocab/bench_biasing.py --set clean --lists 100,1000

Needs build/biasing/ (ref/*.tsv, score.py) and the openslr/librispeech_asr
test parquet in the Hugging Face cache.
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import re
import subprocess
import sys
import time
from array import array
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "services" / "asr"))
BIASING = REPO / "build" / "biasing"
LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"


def normalize(text: str) -> str:
    text = text.lower().replace("-", " ")
    return " ".join(re.findall(r"[a-z0-9']+", text))


def read_refs(path: Path) -> dict:
    refs = {}
    for line in path.read_text().splitlines():
        uid, ref, rare, biasing = line.split("\t")
        refs[uid] = (ref, json.loads(rare), json.loads(biasing))
    return refs


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--set", default="clean", choices=("clean", "other"))
    parser.add_argument("--lists", default="100,1000", help="biasing list sizes to run")
    parser.add_argument("--tier", default="project", help="word list tier the list goes in")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--out", default=None)
    parser.add_argument("--filter", default="none", choices=("none", "app", "app-rare"),
                        help="pass each list through the app's own filter first (sunoto-daemon words --filter)")
    parser.add_argument("--daemon", default=str(REPO / "target/release/sunoto-daemon"))
    parser.add_argument("--engine", default="parakeet", choices=("parakeet", "qwen3-bf16", "qwen3-8bit"))
    args = parser.parse_args()
    sizes = [int(x) for x in args.lists.split(",") if x]
    out = Path(args.out or BIASING / f"run-{args.set}")
    out.mkdir(parents=True, exist_ok=True)

    import numpy as np
    import pyarrow.parquet as pq
    import soundfile

    if args.engine == "parakeet":
        import parakeet_mlx_offline_sidecar as sidecar
    else:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import qwen3_engine

    refs = {n: read_refs(BIASING / "ref" / f"test-{args.set}.biasing_{n}.tsv") for n in sizes}
    if args.filter != "none":
        # The app only ever lists words its English check calls unusual.
        for n in sizes:
            uids = list(refs[n])
            path = out / f"lists-{n}.json"
            path.write_text(json.dumps([refs[n][uid][2] for uid in uids]))
            command = [args.daemon, "words", "--filter", str(path)] + (["--keep-rare"] if args.filter == "app-rare" else [])
            kept = json.loads(subprocess.run(command, check=True, capture_output=True, text=True).stdout)
            refs[n] = {uid: (ref, rare, words) for uid, (ref, rare, _), words in zip(uids, (refs[n][u] for u in uids), kept)}
        print(f"filtered lists ({args.filter}): {sum(len(v[2]) for v in refs[sizes[0]].values()) / len(refs[sizes[0]]):.0f} words each on average", file=sys.stderr)
    parquet = glob.glob(str(Path.home() / f".cache/huggingface/hub/datasets--openslr--librispeech_asr/snapshots/*/{args.set}/test/0000.parquet"))[0]
    table = pq.read_table(parquet, columns=["id", "audio"])
    ids = table.column("id").to_pylist()
    audios = table.column("audio").to_pylist()
    if args.limit:
        ids, audios = ids[: args.limit], audios[: args.limit]

    if args.engine == "parakeet":
        kwargs = {"model_name": str(LOCAL_MODEL)} if LOCAL_MODEL.exists() else {}
        engine = sidecar.OfflineParakeetMlxEngine(segment_min_s=0, pause_decode=False, **kwargs)
        run = lambda fn, *a: engine._worker.submit(fn, *a).result()  # noqa: E731
    else:
        qwen = qwen3_engine.Qwen3Engine(args.engine)

    modes = ["off"] + [f"b{n}" for n in sizes]
    hyps = {mode: [] for mode in modes}
    times = {mode: 0.0 for mode in modes}
    seconds = 0.0
    started = time.time()
    for k, (uid, audio) in enumerate(zip(ids, audios), 1):
        samples, rate = soundfile.read(io.BytesIO(audio["bytes"]), dtype="float32")
        pcm = array("h", np.clip(np.round(samples * 32768), -32768, 32767).astype("<i2").tobytes())
        seconds += len(pcm) / 16000
        for mode in modes:
            words = None if mode == "off" else {args.tier: refs[int(mode[1:])][uid][2]}
            if args.engine != "parakeet":
                t0 = time.perf_counter()
                text = qwen.transcribe(pcm, qwen3_engine.context_prompt(words))
                times[mode] += time.perf_counter() - t0
                hyps[mode].append(f"{uid}\t{normalize(text)}")
                continue
            run(engine._set_booster, words)
            t0 = time.perf_counter()
            text = run(engine._transcribe_pcm_i16, pcm)
            if engine._booster is not None:
                text = engine._booster.respell(text)
            times[mode] += time.perf_counter() - t0
            hyps[mode].append(f"{uid}\t{normalize(text)}")
        if k % 200 == 0:
            print(f"{k}/{len(ids)} utterances, {time.time() - started:.0f}s", file=sys.stderr)

    ref_file = BIASING / "ref" / f"test-{args.set}.biasing_{sizes[0]}.tsv"
    if args.limit:
        keep = set(ids)
        lines = [l for l in ref_file.read_text().splitlines() if l.split("\t")[0] in keep]
        ref_file = out / "ref-subset.tsv"
        ref_file.write_text("\n".join(lines) + "\n")
    print(f"\nLibriSpeech test-{args.set}: {len(ids)} utterances, {seconds / 3600:.2f} h, list tier {args.tier}")
    for mode in modes:
        hyp = out / f"hyp-{mode}.tsv"
        hyp.write_text("\n".join(hyps[mode]) + "\n")
        score = subprocess.run(
            [sys.executable, str(BIASING / "score.py"), "--refs", str(ref_file), "--hyps", str(hyp)],
            capture_output=True, text=True,
        )
        result = [
            re.sub(r"error_rate=([\d.]+).*", lambda m: f"{float(m.group(1)):.2f}%", line.split(" - ")[-1])
            for line in (score.stdout + score.stderr).splitlines()
            if "WER" in line and "error_rate" in line
        ]
        print(f"  {mode:<6} decode {1000 * times[mode] / seconds:5.1f} ms/s audio | " + " | ".join(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

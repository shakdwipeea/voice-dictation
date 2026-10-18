#!/usr/bin/env python3
"""A/B benchmark for word boosting in the Parakeet-MLX sidecar.

Decodes every clip from make_audio.py through the real sidecar engine,
interleaving the modes clip by clip so GPU clocks and heat hit them alike:

  off         no word list: the stock parakeet-mlx decoder (today's app)
  background  the folder/app and project lists, nothing on screen
  screen      background plus the screen list, with each case's names on it

``--sweep 1,2,3`` runs background and screen once per bonus scale (mode names
get an ``@scale`` suffix) to pick the strength.

Each mode is scored as decoded and after the daemon's near-miss fix (``+fix``,
run through ``sunoto-daemon words --fix``). Reports name accuracy, word error
rate and changed outputs on ordinary speech, decode time per clip, and MLX
peak memory. Stop the app first: a warm sidecar
in the background skews the timings (AGENTS.md).

  .venv-nemotron-mac/bin/python tools/vocab/bench_words.py --words WORDS.json
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
import wave
from array import array
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "services" / "asr"))

LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"


def read_wav(path: str) -> array:
    with wave.open(path, "rb") as wav:
        samples = array("h")
        samples.frombytes(wav.readframes(wav.getnframes()))
    return samples


def words_of(text: str) -> list[str]:
    return re.findall(r"[a-z0-9']+", text.lower().replace("-", " "))


def edit_distance(a: list[str], b: list[str]) -> int:
    row = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        prev, row[0] = row[0], i
        for j, y in enumerate(b, 1):
            prev, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, prev + (x != y))
    return row[-1]


def name_found(name: str, text: str) -> tuple[bool, bool]:
    """(exact, loose): exact ignores case; loose also ignores spaces, dots, hyphens."""
    exact = re.search(rf"(?<![\w]){re.escape(name)}(?![\w])", text, re.IGNORECASE) is not None
    squash = lambda s: re.sub(r"[\s.\-]", "", s.lower())  # noqa: E731
    return exact, squash(name) in squash(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--words", required=True, help="JSON with everywhere/project/screen lists")
    parser.add_argument("--manifest", default=str(REPO / "build/vocab/manifest.json"))
    parser.add_argument("--out", default=str(REPO / "build/vocab/results.json"))
    parser.add_argument("--limit", type=int, default=0, help="only the first N clips")
    parser.add_argument("--sweep", default="", help="comma-separated bonus scales to compare")
    parser.add_argument("--start-share", type=float, default=None, help="override START_SHARE")
    parser.add_argument("--model", default=str(LOCAL_MODEL) if LOCAL_MODEL.exists() else None)
    parser.add_argument("--daemon", default=str(REPO / "target/release/sunoto-daemon"),
                        help="sunoto-daemon binary for the near-miss fix (words --fix)")
    parser.add_argument("--engine", default="parakeet", choices=("parakeet", "qwen3-bf16", "qwen3-8bit"),
                        help="speech model: the app's Parakeet sidecar, or Qwen3-ASR with the list as context")
    parser.add_argument("--rescore", action="store_true",
                        help="reuse the decodes in --out and only redo the fix and the scores")
    args = parser.parse_args()
    lists = json.loads(Path(args.words).read_text())
    background = {"everywhere": lists.get("everywhere", []), "project": lists.get("project", [])}
    if args.rescore:
        results = json.loads(Path(args.out).read_text())
        modes = [key for key in results[0] if isinstance(results[0][key], dict) and "ms" in results[0][key]]
        score(results, modes, lists, background, args)
        return 0

    import mlx.core as mx

    if args.engine == "parakeet":
        import parakeet_mlx_offline_sidecar as sidecar
        import word_boost
    else:
        import qwen3_engine
    if args.start_share is not None:
        word_boost.START_SHARE = args.start_share
    scales = [float(x) for x in args.sweep.split(",") if x] or [1.0]
    base_bonus = dict(word_boost.TIER_BONUS) if args.engine == "parakeet" else {}
    MODES = ["off"] + [
        f"{mode}@{scale:g}" if args.sweep else mode
        for scale in scales
        for mode in ("background", "screen")
    ]

    manifest = json.loads(Path(args.manifest).read_text())
    if args.limit:
        manifest = manifest[: args.limit]

    if args.engine != "parakeet":
        qwen = qwen3_engine.Qwen3Engine(args.engine)

        def decode(samples, words, scale=1.0):
            t0 = time.perf_counter()
            text = qwen.transcribe(samples, qwen3_engine.context_prompt(words))
            return text, (time.perf_counter() - t0) * 1000

    else:
        kwargs = {"model_name": args.model} if args.model else {}
        engine = sidecar.OfflineParakeetMlxEngine(segment_min_s=0, pause_decode=False, **kwargs)
        run = lambda fn, *a: engine._worker.submit(fn, *a).result()  # noqa: E731

    def parakeet_decode(samples, words, scale=1.0):
        word_boost.TIER_BONUS.update({tier: value * scale for tier, value in base_bonus.items()})
        run(engine._set_booster, words)
        t0 = time.perf_counter()
        text = run(engine._transcribe_pcm_i16, samples)
        ms = (time.perf_counter() - t0) * 1000
        if engine._booster is not None:
            text = engine._booster.respell(text)
        return text, ms

    if args.engine == "parakeet":
        decode = parakeet_decode
    warm = read_wav(manifest[0]["audio"])
    for words in (None, background):
        decode(warm, words)

    peak = {}
    results = []
    for n, case in enumerate(manifest, 1):
        samples = read_wav(case["audio"])
        screen = list(lists.get("screen", []))
        if case["set"] == "names":
            screen = case["names"] + screen
        lists_for = {"off": None, "background": background, "screen": {**background, "screen": screen}}
        row = {**case, "seconds": len(samples) / 16000}
        for mode in MODES:
            kind, _, scale = mode.partition("@")
            mx.reset_peak_memory()
            text, ms = decode(samples, lists_for[kind], float(scale or 1))
            peak[mode] = max(peak.get(mode, 0), mx.get_peak_memory())
            row[mode] = {"text": text, "ms": ms}
        results.append(row)
        if n % 25 == 0:
            print(f"{n}/{len(manifest)} clips", file=sys.stderr)
    score(results, MODES, lists, background, args)

    print("\nDecode time per clip (ms): median / p90 / mean, and ms per second of audio:")
    for mode in MODES:
        times = sorted(r[mode]["ms"] for r in results)
        rate = sum(times) / sum(r["seconds"] for r in results)
        print(
            f"  {mode:<11} {statistics.median(times):6.1f} / {times[int(len(times) * 0.9)]:6.1f}"
            f" / {statistics.mean(times):6.1f}   {rate:5.1f} ms/s"
        )
    print("\nMLX peak memory during a decode (MB):")
    for mode in MODES:
        print(f"  {mode:<11} {peak[mode] / 1e6:7.1f}")
    print(f"\nwrote {args.out}")
    return 0


def score(results, MODES, lists, background, args) -> None:
    """Run the near-miss fix on every decode, then print name and ordinary scores."""
    # The daemon's near-miss fix (Rust) runs on every decode that had a list,
    # and on today's decode alone to see what the fix does without boosting.
    fixes = []
    for row in results:
        screen = (row["names"] if row["set"] == "names" else []) + list(lists.get("screen", []))
        for mode in MODES:
            kind = mode.partition("@")[0]
            words = {**background, "screen": screen} if kind == "screen" else background
            source = "off" if kind == "off" else mode
            fixes.append((row, f"{mode}+fix", {"text": row[source]["text"], "words": words}))
    fix_input = Path(args.out).with_suffix(".fix-input.json")
    fix_input.write_text(json.dumps([item for _, _, item in fixes]))
    fixed = json.loads(
        subprocess.run(
            [str(args.daemon), "words", "--fix", str(fix_input)],
            check=True, capture_output=True, text=True,
        ).stdout
    )
    for (row, key, _), text in zip(fixes, fixed):
        row[key] = {"text": text}
    variants = [v for mode in MODES for v in (mode, f"{mode}+fix")]
    Path(args.out).write_text(json.dumps(results, indent=1))

    print(f"\n{len(results)} clips, {sum(r['seconds'] for r in results):.0f}s of audio")
    print("\nNames spelled exactly / with the right letters (ignoring spaces):")
    for source in ("everywhere", "project", "screen", None):
        rows = [r for r in results if r["set"] == "names" and (source is None or r["source"] == source)]
        if not rows:
            continue
        total = sum(len(r["names"]) for r in rows)
        print(f"  {source or 'all'} ({total} names):")
        for variant in variants:
            hits = [name_found(name, r[variant]["text"]) for r in rows for name in r["names"]]
            print(f"    {variant:<20} {sum(h[0] for h in hits):>3} / {sum(h[1] for h in hits):<3}")

    ordinary = [r for r in results if r["set"] == "ordinary"]
    if ordinary:
        print(f"\nOrdinary speech ({len(ordinary)} clips): word error rate, outputs changed vs off")
        ref = sum(len(words_of(r["text"])) for r in ordinary)
        for variant in variants:
            errors = sum(
                edit_distance(words_of(r["text"]), words_of(r[variant]["text"])) for r in ordinary
            )
            changed = [r for r in ordinary if r[variant]["text"] != r["off"]["text"]]
            print(f"  {variant:<20} WER {100 * errors / ref:5.2f}%   changed {len(changed)}")
            for r in changed[:12]:
                print(f"      {r['id']} {r['voice']}: {r['off']['text']!r}\n{'':>10}-> {r[variant]['text']!r}")



if __name__ == "__main__":
    raise SystemExit(main())

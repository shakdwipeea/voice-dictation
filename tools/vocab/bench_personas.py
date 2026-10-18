#!/usr/bin/env python3
"""Word-list benchmark across six kinds of users (tools/vocab/personas.json).

Lists come from the real collector (`sunoto-daemon words --home ... --json`)
run on each persona's pretend home, so the filtering is the app's own:

  today      project folders and apps (and the project, for the developer)
  more       today + Documents/Desktop folders, recent files, contacts
  rare       more, also keeping rare English words ("hidradenitis")

each without the screen (bg) and with the persona's screen. Every clip is
decoded with no list and with each list; each decode is then fixed by the
daemon three ways: the first shipped fix (two-word joins), the new default
(exact joins of up to four words), and the new default plus sound matching.

  .venv-nemotron-mac/bin/python tools/vocab/bench_personas.py
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
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "services" / "asr"))
OUT = REPO / "build" / "personas"
LOCAL_MODEL = Path.home() / "Library/Application Support/sunoto/models/parakeet-tdt-0.6b-v3-bf16"
DECODES = ("off", "today-bg", "today-screen", "more-bg", "more-screen", "rare-bg", "rare-screen")
ISOLATE = ("off", "today-bg", "contacts-bg", "recent-bg", "docs-bg", "rare-only-bg", "rare-bg")
# The shipped defaults (rare words kept, nothing opted in) and with both
# opt-ins (contacts, documents); bg = nothing relevant on screen.
FINAL = ("off", "default-bg", "default-screen", "optin-bg", "optin-screen")
FIXES = {
    "": None,
    "+fix1": {"max_join": 2, "phonetic": False},
    "+fix": {"max_join": 4, "phonetic": False},
    "+fix+sound": {"max_join": 4, "phonetic": True},
}


def read_wav(path: str) -> array:
    with wave.open(path, "rb") as wav:
        samples = array("h")
        samples.frombytes(wav.readframes(wav.getnframes()))
    return samples


def words_of(text: str) -> list[str]:
    return re.findall(r"[a-z0-9']+", text.lower().replace("-", " "))


def edit_distance(a, b) -> int:
    row = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        prev, row[0] = row[0], i
        for j, y in enumerate(b, 1):
            prev, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, prev + (x != y))
    return row[-1]


def found(name: str, text: str) -> bool:
    return re.search(rf"(?<!\w){re.escape(name)}(?!\w)", text, re.IGNORECASE) is not None


def persona_lists(daemon: str, name: str, project: str | None, modes) -> dict:
    base = [daemon, "words", "--home", str(OUT / name / "home"), "--json"]
    if project:
        base += ["--document", f"file://{project}/"]
    more = ["--documents", "--recent", str(OUT / name / "recent.txt"), "--contacts", str(OUT / name / "contacts.txt")]
    screen = ["--text", str(OUT / name / "screen.txt")]
    lists = {}
    rare = more + ["--keep-rare"]
    flags = {
        "today-bg": [], "today-screen": screen, "more-bg": more,
        "more-screen": more + screen, "rare-bg": rare, "rare-screen": rare + screen,
        "contacts-bg": ["--contacts", str(OUT / name / "contacts.txt")],
        "recent-bg": ["--recent", str(OUT / name / "recent.txt")],
        "docs-bg": ["--documents"], "rare-only-bg": ["--keep-rare"],
        "default-bg": ["--keep-rare"], "default-screen": ["--keep-rare"] + screen,
        "optin-bg": rare, "optin-screen": rare + screen,
    }
    for mode in modes:
        if mode == "off":
            continue
        extra = flags[mode]
        words = json.loads(subprocess.run(base + extra, check=True, capture_output=True, text=True).stdout)
        lists[mode] = {tier: words.get(tier, []) for tier in ("screen", "project", "everywhere")}
    return lists


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", default=str(REPO / "target/release/sunoto-daemon"))
    parser.add_argument("--out", default=str(OUT / "results.json"))
    parser.add_argument("--rescore", action="store_true", help="reuse decodes in --out")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--manifest", default=str(OUT / "manifest.json"), help="e.g. a noisy copy")
    parser.add_argument("--isolate", action="store_true", help="each new source on its own, no screen")
    parser.add_argument("--start-shares", default="", help="e.g. 0.5,0.25,0: rare-screen once per START_SHARE")
    parser.add_argument("--final", action="store_true", help="the shipped defaults, with and without opt-ins")
    parser.add_argument("--engine", default="parakeet", choices=("parakeet", "qwen3-bf16", "qwen3-8bit"),
                        help="Qwen3-ASR gets the list as context; screen modes also get a '~text' run "
                        "with the persona's screen text itself in the prompt")
    args = parser.parse_args()
    manifest = json.loads(Path(args.manifest).read_text())
    if args.limit:
        manifest = manifest[: args.limit]
    personas = sorted({case["persona"] for case in manifest})
    global DECODES
    shares = [float(x) for x in args.start_shares.split(",") if x]
    if args.isolate:
        DECODES = ISOLATE
    if shares:
        DECODES = ("off",) + tuple(f"rare-screen@{x:g}" for x in shares)
    if args.final:
        DECODES = FINAL
    if args.engine != "parakeet":
        DECODES = tuple(DECODES) + tuple(f"{m}~text" for m in DECODES if m.endswith("-screen"))
    base_modes = sorted({mode.partition("@")[0].partition("~")[0] for mode in DECODES})
    lists = {p: persona_lists(args.daemon, p, next(c["project"] for c in manifest if c["persona"] == p), base_modes) for p in personas}
    for p in personas:
        for mode in DECODES:
            lists[p][mode] = lists[p].get(mode.partition("@")[0].partition("~")[0])
    (OUT / "lists.json").write_text(json.dumps(lists, indent=1))

    if args.rescore:
        results = json.loads(Path(args.out).read_text())
    else:
        if args.engine == "parakeet":
            import parakeet_mlx_offline_sidecar as sidecar
            import word_boost

            kwargs = {"model_name": str(LOCAL_MODEL)} if LOCAL_MODEL.exists() else {}
            engine = sidecar.OfflineParakeetMlxEngine(segment_min_s=0, pause_decode=False, **kwargs)
            run = lambda fn, *a: engine._worker.submit(fn, *a).result()  # noqa: E731
        else:
            import qwen3_engine

            qwen = qwen3_engine.Qwen3Engine(args.engine)
            screens = {p: (OUT / p / "screen.txt").read_text() for p in personas}
        results = []
        for n, case in enumerate(manifest, 1):
            samples = read_wav(case["audio"])
            row = {**case, "seconds": len(samples) / 16000}
            for mode in DECODES:
                words = None if mode == "off" else lists[case["persona"]][mode]
                if args.engine != "parakeet":
                    style = "screen" if mode.endswith("~text") else "list"
                    prompt = qwen3_engine.context_prompt(words, screens[case["persona"]], style)
                    t0 = time.perf_counter()
                    text = qwen.transcribe(samples, prompt)
                    row[mode] = {"text": text, "ms": (time.perf_counter() - t0) * 1000}
                    continue
                if "@" in mode:
                    word_boost.START_SHARE = float(mode.partition("@")[2])
                run(engine._set_booster, words)
                t0 = time.perf_counter()
                text = run(engine._transcribe_pcm_i16, samples)
                if engine._booster is not None:
                    text = engine._booster.respell(text)
                row[mode] = {"text": text, "ms": (time.perf_counter() - t0) * 1000}
            results.append(row)
            if n % 50 == 0:
                print(f"{n}/{len(manifest)} clips", file=sys.stderr)

    # The daemon's fix runs only when there is a list, so "off" stays as is.
    items, keys = [], []
    for row in results:
        for mode in DECODES[1:]:
            for suffix, options in FIXES.items():
                if options is None:
                    continue
                items.append({"text": row[mode]["text"], "words": lists[row["persona"]][mode], "options": options})
                keys.append((row, mode + suffix))
    fix_input = Path(args.out).with_suffix(".fix-input.json")
    fix_input.write_text(json.dumps(items))
    fixed = json.loads(subprocess.run([args.daemon, "words", "--fix", str(fix_input)], check=True, capture_output=True, text=True).stdout)
    for (row, key), text in zip(keys, fixed):
        row[key] = {"text": text}
    Path(args.out).write_text(json.dumps(results, indent=1))

    variants = ["off"] + [m + s for m in DECODES[1:] for s in FIXES]
    names = [r for r in results if r["set"] == "names"]
    ordinary = [r for r in results if r["set"] == "ordinary"]
    total = sum(len(r["names"]) for r in names)
    print(f"{len(results)} clips ({len(names)} with names, {len(ordinary)} ordinary), {len(personas)} personas, 8 voices")
    print(f"\nNames spelled right (of {total}), by persona, and ordinary clips changed vs off (of {len(ordinary)}):")
    header = "  " + f"{'variant':<24}" + "".join(f"{p[:9]:>10}" for p in personas) + f"{'all':>8}{'ordinary':>10}"
    print(header)
    for variant in variants:
        line = f"  {variant:<24}"
        for p in personas:
            rows = [r for r in names if r["persona"] == p]
            hits = sum(found(n, r[variant]["text"]) for r in rows for n in r["names"])
            line += f"{hits:>4}/{sum(len(r['names']) for r in rows):<5}"
        hits = sum(found(n, r[variant]["text"]) for r in names for n in r["names"])
        changed = sum(r[variant]["text"] != r["off"]["text"] for r in ordinary)
        line += f"{hits:>4}/{total:<4}{changed:>6}"
        print(line)

    if args.isolate or shares or args.final:
        ref = sum(len(words_of(r["text"])) for r in ordinary)
        for variant in ("off", "default-screen+fix+sound", "optin-screen+fix+sound"):
            errors = sum(edit_distance(words_of(r["text"]), words_of(r[variant]["text"])) for r in ordinary)
            print(f"  ordinary WER {variant:<22} {100 * errors / ref:5.2f}%")
            for r in ordinary:
                if r[variant]["text"] != r["off"]["text"] and variant != "off":
                    print(f"      {r['id']} {r['voice']}: {r['off']['text'][:80]!r} -> {r[variant]['text'][:80]!r}")
        if not args.rescore:
            for mode in DECODES:
                print(f"  decode median {mode:<16} {statistics.median(r[mode]['ms'] for r in results):6.1f} ms")
        return 0
    print("\nBy where the name lives (off / today-screen+fix1 / rare-screen+fix+sound):")
    by_source = defaultdict(lambda: [0, 0, 0, 0])
    for r in names:
        for n in r["names"]:
            for source in r["in"] or ["nowhere"]:
                by_source[source][0] += 1
                by_source[source][1] += found(n, r["off"]["text"])
                by_source[source][2] += found(n, r["today-screen+fix1"]["text"])
                by_source[source][3] += found(n, r["rare-screen+fix+sound"]["text"])
    for source, (count, off, today, best) in sorted(by_source.items()):
        print(f"  {source:<13} {count:>4} names   off {off:>4}   today {today:>4}   best {best:>4}")

    ref = sum(len(words_of(r["text"])) for r in ordinary)
    print("\nOrdinary speech word error rate:")
    for variant in ("off", "today-screen+fix1", "more-screen+fix", "rare-screen+fix", "rare-screen+fix+sound"):
        errors = sum(edit_distance(words_of(r["text"]), words_of(r[variant]["text"])) for r in ordinary)
        print(f"  {variant:<24} {100 * errors / ref:5.2f}%")
        for r in ordinary:
            if r[variant]["text"] != r["off"]["text"]:
                print(f"      {r['id']} {r['voice']}: {r['off']['text']!r} -> {r[variant]['text']!r}")
    if not args.rescore:
        print("\nDecode time per clip, median ms:")
        for mode in DECODES:
            print(f"  {mode:<14} {statistics.median(r[mode]['ms'] for r in results):6.1f}")
    print("\nList sizes (screen / project / everywhere):")
    for p in personas:
        print("  " + p.ljust(10) + "  ".join(f"{m}: {len(lists[p][m]['screen'])}/{len(lists[p][m]['project'])}/{len(lists[p][m]['everywhere'])}" for m in DECODES[1:]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

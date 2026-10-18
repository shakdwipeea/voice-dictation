#!/usr/bin/env python3
"""Rescore bench_biasing.py runs: optionally drop utterances, and apply the
daemon's near-miss fix (sunoto-daemon words --fix) to each hypothesis with
that utterance's list, so fix options are compared without decoding again.

  python3 tools/vocab/rescore_biasing.py build/biasing/today-clean --set clean --size 100 --drop 4507-16021-0012
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIASING = REPO / "build" / "biasing"


def score(ref_lines, hyp_lines, tmp: Path) -> str:
    (tmp / "r.tsv").write_text("\n".join(ref_lines) + "\n")
    (tmp / "h.tsv").write_text("\n".join(hyp_lines) + "\n")
    out = subprocess.run([sys.executable, str(BIASING / "score.py"), "--refs", str(tmp / "r.tsv"), "--hyps", str(tmp / "h.tsv")],
                         capture_output=True, text=True)
    parts = []
    for line in (out.stdout + out.stderr).splitlines():
        m = re.search(r"(\w-?WER|WER): error_rate=([\d.]+)", line)
        if m:
            parts.append(f"{m.group(1)} {float(m.group(2)):.2f}%")
    return " | ".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run")
    parser.add_argument("--set", default="clean")
    parser.add_argument("--size", type=int, default=100)
    parser.add_argument("--drop", default="", help="comma-separated utterance ids to leave out")
    parser.add_argument("--fix", action="store_true", help="also score the near-miss fix variants")
    parser.add_argument("--only-from", default="", help="score only the utterances another run decoded")
    parser.add_argument("--drop-stuck", action="store_true",
                        help="leave out utterances any run in --stuck-from decoded as a run of <unk>")
    parser.add_argument("--stuck-from", default="", help="comma-separated run folders to check for <unk> runs")
    parser.add_argument("--daemon", default=str(REPO / "target/release/sunoto-daemon"))
    args = parser.parse_args()
    run = Path(args.run)
    drop = set(filter(None, args.drop.split(",")))
    refs = {l.split("\t")[0]: l for l in (BIASING / "ref" / f"test-{args.set}.biasing_{args.size}.tsv").read_text().splitlines()}
    hyps = {}
    for path in sorted(run.glob("hyp-*.tsv")):
        hyps[path.stem[4:]] = dict(l.split("\t", 1) for l in path.read_text().splitlines() if "\t" in l)
    if args.drop_stuck:
        for folder in [run] + [Path(f) for f in args.stuck_from.split(",") if f]:
            for path in folder.glob("hyp-*.tsv"):
                for line in path.read_text().splitlines():
                    uid, _, text = line.partition("\t")
                    if re.search(r"(\bunk\b ){4}", text):
                        drop.add(uid)
    uids = [u for u in hyps["off"] if u not in drop]
    if args.only_from:
        keep = {l.split("\t")[0] for l in (Path(args.only_from) / "hyp-off.tsv").read_text().splitlines() if l}
        uids = [u for u in uids if u in keep]
    ref_lines = [refs[u] for u in uids]
    print(f"{run.name}: {len(uids)} utterances (dropped {len(drop & set(hyps['off']))})")
    for mode, hyp in hyps.items():
        print(f"  {mode:<14} {score(ref_lines, [f'{u}\t{hyp[u]}' for u in uids], run)}")
    if args.fix:
        lists = {u: json.loads(refs[u].split("\t")[3]) for u in uids}
        for mode in [m for m in hyps if m != "off"]:
            for name, options in (("+fix", {"max_join": 4, "phonetic": False}), ("+fix+sound", {"max_join": 4, "phonetic": True})):
                items = [{"text": hyps[mode][u], "words": {"project": lists[u]}, "options": options} for u in uids]
                (run / "fix.json").write_text(json.dumps(items))
                fixed = json.loads(subprocess.run([args.daemon, "words", "--fix", str(run / "fix.json")], check=True, capture_output=True, text=True).stdout)
                # Hypotheses are scored lowercase and without punctuation.
                fixed = [" ".join(re.findall(r"[a-z0-9']+", t.lower().replace("-", " "))) for t in fixed]
                print(f"  {mode + name:<14} {score(ref_lines, [f'{u}\t{t}' for u, t in zip(uids, fixed)], run)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

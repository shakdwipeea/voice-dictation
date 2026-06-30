#!/usr/bin/env python3
"""Minimal standalone eval of the constrained OK/EDIT polish prompt.

Loads the GGUF model ONCE (reuses the sidecar's load_model + DiagnosticLlamaRAMCache
+ constrained_payload hot path, so the EXACT prompt + grammar + repeat_penalty the
daemon uses are exercised). Honors all SUNOTO_LLM_POLISH_* env overrides, so:

    SUNOTO_LLM_POLISH_REPEAT_PENALTY=1.0 .venv-llm-polish-mac/bin/python \\
        llm-polish-bench/eval_constrained.py --dataset llm-polish-bench/dataset.jsonl

A/B's clean: same code path, only the env knob differs.

Grades:
  - exact_match      : final text == expected (normalized)
  - content_drop     : gold-required significant words missing from output
                       (the user's bug class). HARD metric.
  - over_edit        : gold OK but model EDITed (false-positive).

This is the §3 eval module of the eval-loop plan; `evaluate()` is imported by
`optimize_constrained_loop.py` to score candidate prompts.
"""
from __future__ import annotations
import argparse, json, os, re, sys, time, statistics
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "services" / "polish"))

import importlib.util
_spec = importlib.util.spec_from_file_location("polish_sidecar", ROOT / "services" / "polish" / "llm_polish_sidecar.py")
polish = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(polish)

SIGNIFICANT_STOP = set("""
a an the of to in on at for and or but nor so as if then than with from by be is am are was were
been being do does did doing done have has had having i you he she it we they me him her them us my
your his its our their this that these those not no yes too very just only also still already yet
now well um uh er hmm mhm uhm umm sorry wait actually never mind mean means meant instead however
will would could should can may might must shall do
""".split())

def sig_words(s: str) -> set[str]:
    return {w for w in re.sub(r"[^a-z0-9 ]", " ", s.lower()).split()
            if w not in SIGNIFICANT_STOP and len(w) > 1}

def normalize(s: str) -> str:
    return re.sub(r"\s+", " ", re.sub(r"[^a-z0-9 ]", " ", s.lower())).strip()

CUE_RE = re.compile(r"\b(sorry sorry|sorry|no wait|no, wait|i mean|i meant|i meen|actually never mind|never mind|scratch that|strike that|let me start over|wait no|no,)\b", re.I)

def content_dropped(expected: str, out: str) -> list[str]:
    """Significant words the GOLD requires that are missing from the model output.

    A word counts as dropped only if the gold expected text contains it (so it
    must survive) and the model omitted it. Aborted-clause words the gold ALSO
    drops (restart/cancel) are never flagged — only gold-required content matters.
    """
    return sorted(sig_words(expected) - sig_words(out))

def load_dataset(path: Path) -> list[dict]:
    rows = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows

FewShot = tuple[tuple[str, str], ...]

def _grade(rows, results):
    exact = sum(1 for r in results if r["exact"])
    content_drop_cases = [r for r in results if r["dropped"]]
    over_edit = sum(1 for r in results if r["gold"] == "OK" and r["decision"] == "EDIT")
    malformed = sum(1 for r in results if r.get("malformed"))
    lat = [r["lat"] for r in results]
    by_cat: dict[str, dict[str, int]] = {}
    for r in results:
        c = by_cat.setdefault(r["cat"], {"n": 0, "exact": 0, "drop": 0})
        c["n"] += 1
        if r["exact"]:
            c["exact"] += 1
        if r["dropped"]:
            c["drop"] += 1
    return {
        "n": len(rows),
        "exact": exact,
        "exact_pct": 100.0 * exact / max(1, len(rows)),
        "content_drop": len(content_drop_cases),
        "malformed": malformed,
        "malformed_rate": malformed / max(1, len(results)),
        "over_edit": over_edit,
        "p50_ms": statistics.median(lat) if lat else 0,
        "p95_ms": sorted(lat)[int(0.95 * len(lat)) - 1] if len(lat) > 1 else (lat[0] if lat else 0),
        "by_cat": by_cat,
        "rows": results,
    }

def evaluate(
    llm,
    rows: list[dict],
    system_prompt: str | None = None,
    few_shot: FewShot | None = None,
    warmup: int = 2,
    k: int = 1,
) -> dict[str, Any]:
    """Score a (system_prompt, few_shot) candidate against `rows` via the
    live constrained_payload hot path. Pass k>1 for worst-of-K (stability)."""
    # Warm the KV cache with the CANDIDATE prompt (2 throwaway calls) so timing
    # is warm and not contaminated by the previous candidate's prefix.
    wt = [r["input"] for r in rows[:max(warmup, 2)]]
    for t in wt:
        polish.constrained_payload(llm, t, "warmup", system_prompt, few_shot)

    runs: list[dict[str, Any]] = []
    for _ in range(max(1, k)):
        results = []
        for r in rows:
            rid, cat, inp, exp = r["id"], r["category"], r["input"], r["expected"]
            p = polish.constrained_payload(llm, inp, rid, system_prompt, few_shot)
            out = p["text"]
            decision = "OK" if normalize(out) == normalize(inp) else "EDIT"
            gold_decision = "OK" if normalize(inp) == normalize(exp) else "EDIT"
            ex = (normalize(out) == normalize(exp))
            dropped = content_dropped(exp, out)
            results.append({
                "id": rid, "cat": cat, "decision": decision, "gold": gold_decision,
                "exact": ex, "dropped": dropped, "lat": p["latency_ms"],
                "out": out, "expected": exp, "input": inp,
                "malformed": bool(p.get("decision_malformed")),
            })
        runs.append(_grade(rows, results))

    if k <= 1:
        return runs[0]
    # worst-of-K: take the min exact_pct run, and max content_drop/over_edit.
    worst = min(runs, key=lambda r: (r["exact_pct"], -r["content_drop"]))
    # NOTE: `worst` is itself an element of `runs`, so we must NOT assign
    # `runs` back into `worst` (that creates a reference cycle that breaks
    # json.dump). Store a rows-stripped copy of each run instead.
    worst["runs"] = [{k: v for k, v in r.items() if k != "rows"} for r in runs]
    worst["exact_stdev"] = statistics.pstdev([r["exact_pct"] for r in runs])
    return worst

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default=str(ROOT / "llm-polish-bench" / "dataset.jsonl"))
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--limit", type=int, default=0, help="0=all")
    ap.add_argument("--system-prompt-file", default=None, help="override CONSTRAINED_SYSTEM_PROMPT")
    ap.add_argument("--few-shot-file", default=None, help="override few-shot (JSON list)")
    args = ap.parse_args()

    rows = load_dataset(Path(args.dataset))
    if args.limit:
        rows = rows[:args.limit]
    print(f"dataset: {args.dataset} ({len(rows)} rows)", flush=True)
    print(f"repeat_penalty={os.environ.get('SUNOTO_LLM_POLISH_REPEAT_PENALTY','1.05')} "
          f"temperature={os.environ.get('SUNOTO_LLM_POLISH_TEMPERATURE','0.1')} "
          f"grammar={os.environ.get('SUNOTO_LLM_POLISH_GRAMMAR','0')}", flush=True)

    sp = None
    fs = None
    if args.system_prompt_file:
        sp = Path(args.system_prompt_file).read_text().strip()
    if args.few_shot_file:
        fs = tuple(tuple(x) for x in json.loads(Path(args.few_shot_file).read_text()))

    t0 = time.time()
    llm = polish.load_model()
    print(f"model loaded in {round((time.time()-t0)*1000)}ms", flush=True)

    mode = polish.polish_mode()
    print(f"polish_mode={mode}  prompt={'override' if sp else 'live'}  few_shot={'override' if fs else 'live'}", flush=True)

    res = evaluate(llm, rows, system_prompt=sp, few_shot=fs, warmup=args.warmup)
    n = res["n"]
    print(f"\n=== RESULTS (n={n}) ===")
    print(f"exact_match    : {res['exact']}/{n} ({res['exact_pct']:.1f}%)")
    print(f"content_drop   : {res['content_drop']} cases  <- HARD gate wants 0")
    print(f"over_edit      : {res['over_edit']} (gold OK but model EDITed)")
    print(f"latency p50/p95 : {res['p50_ms']:.0f}/{res['p95_ms']:.0f} ms (warm)")
    print("\n=== by category (exact%, drop_count) ===")
    for cat in sorted(res["by_cat"]):
        c = res["by_cat"][cat]
        print(f"  {cat:28s} n={c['n']:2d}  exact={100*c['exact']/c['n']:5.1f}%  drop={c['drop']}")
    drops = [r for r in res["rows"] if r["dropped"]]
    if drops:
        print("\n=== CONTENT-DROP DETAIL (the bug class) ===")
        for c in drops:
            print(f"  {c['id']} [{c['cat']}] dropped={c['dropped']}")
            print(f"      in : {c['input']}")
            print(f"      out: {c['out']}")
            print(f"      exp: {c['expected']}")
    out_dir = Path(os.environ.get("SUNOTO_EVAL_OUT_DIR", ROOT / "llm-polish-bench" / "out" / "constrained-eval"))
    out_dir.mkdir(parents=True, exist_ok=True)
    rp = os.environ.get("SUNOTO_LLM_POLISH_REPEAT_PENALTY", "1.05")
    tag = "override" if (sp or fs) else "live"
    dump = out_dir / f"results_{tag}_{mode}_rp{rp}.json"
    res_dump = {k: v for k, v in res.items() if k != "rows"}
    res_dump["rows"] = res["rows"]
    json.dump(res_dump, open(dump, "w"), indent=2)
    print(f"\ndump -> {dump}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())

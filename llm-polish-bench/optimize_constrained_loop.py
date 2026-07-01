#!/usr/bin/env python3
"""Failure-driven prompt-eval loop for the constrained OK/EDIT polish prompt.

Architecture (see docs/llm-polish-prompt-eval-loop-plan-2026-06-30.md §2):

  seed (live CONSTRAINED_SYSTEM_PROMPT + CONSTRAINED_REPAIR_FEW_SHOT)
    │
    ▼
  ┌──────────────────────────────────────────────────────────┐
  │ 1. EVALUATE current best (worst-of-K, via eval_constrained)│
  │ 2. GATE: content_drop==0 & exact≥88 & latency holds?      │
  │       yes → PROMOTE (write prompt into source) & stop      │
  │ 3. COLLECT FAILURES (content_drop first, then over_edit,   │
  │       then exact-miss) — capped at FAILURE_LIMIT            │
  │ 4. PROPOSE: cloud optimizer (codex/gpt-5.5) rewrites the   │
  │       prompt MINIMALLY to fix the shown failures           │
  │ 5. EVALUATE each candidate (worst-of-K); GATE; rank        │
  │ 6. best-improving candidate becomes next round's best      │
  │ 7. STAGNATION_ROUNDS with no gain → stop                   │
  └──────────────────────────────────────────────────────────┘

The optimizer is failure-driven by construction (step 3): it only ever sees
the model's OWN mistakes, never an arbitrary "prompt we want". No cloud call
at inference time — gpt-5.5 runs only inside this offline loop.
"""
from __future__ import annotations
import argparse, json, os, re, subprocess, sys, time, statistics
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "services" / "polish"))
sys.path.insert(0, str(ROOT / "llm-polish-bench"))

import importlib.util
_spec = importlib.util.spec_from_file_location("polish_sidecar", ROOT / "services" / "polish" / "llm_polish_sidecar.py")
polish = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(polish)
import llm_polish_once as once  # the constants + helpers

import eval_constrained as ev

FewShot = tuple[tuple[str, str], ...]

# ---- gate (§2.5) --------------------------------------------------------
EXACT_FLOOR = 88.0          # ship floor on worst-of-K exact%
LATENCY_P50_BUDGET = 30     # ms over baseline
LATENCY_P95_BUDGET = 50     # ms over baseline
PROMPT_WORD_CAP = 110
FEW_SHOT_CAP = 3
FAILURE_LIMIT = 12
MAX_ROUNDS = 6
STAGNATION_ROUNDS = 2

def passes_gate(m: dict, base: dict) -> tuple[bool, list[str]]:
    reasons = []
    if m["content_drop"] != 0:
        reasons.append(f"content_drop={m['content_drop']} (want 0)")
    if m["exact_pct"] < EXACT_FLOOR:
        reasons.append(f"exact={m['exact_pct']:.1f}%<{EXACT_FLOOR}")
    if m["p50_ms"] > base["p50_ms"] + LATENCY_P50_BUDGET:
        reasons.append(f"p50 {m['p50_ms']:.0f}>{base['p50_ms']:.0f}+{LATENCY_P50_BUDGET}")
    if m["p95_ms"] > base["p95_ms"] + LATENCY_P95_BUDGET:
        reasons.append(f"p95 {m['p95_ms']:.0f}>{base['p95_ms']:.0f}+{LATENCY_P95_BUDGET}")
    return (not reasons), reasons

def objective(m: dict, base: dict, prompt_words: int, few_shot_n: int) -> float:
    """Higher is better. Ranks candidates that pass / nearly pass the gate."""
    sim_proxy = m["exact_pct"]  # we have no semantic-sim here; exact is the proxy
    return (sim_proxy
            - m["content_drop"] * 8.0
            - m["over_edit"] * 3.0
            - (m["p95_ms"] - base["p95_ms"]) / 1000.0
            - prompt_words * 0.02
            - few_shot_n * 0.5)

def word_count(s: str) -> int:
    return len(re.findall(r"\S+", s))

# ---- failure collection (step 3) ----------------------------------------
def collect_failures(rows: list[dict], limit: int = FAILURE_LIMIT) -> list[dict]:
    """Prioritize: content_drop (bug class) > over_edit > exact-miss."""
    drops = [r for r in rows if r["dropped"]]
    over = [r for r in rows if r["gold"] == "OK" and r["decision"] == "EDIT" and not r["dropped"]]
    # under-edit: gold EDIT but model OK (e.g. filler not stripped)
    under = [r for r in rows if r["gold"] == "EDIT" and r["decision"] == "OK"]
    miss = [r for r in rows if not r["exact"] and r not in drops and r not in over and r not in under]
    out = []
    for bucket, kind in [(drops, "content_drop"), (over, "over_edit"),
                         (under, "under_edit"), (miss, "exact_miss")]:
        for r in bucket:
            out.append({"id": r["id"], "category": r["cat"], "kind": kind,
                        "input": r["input"], "expected": r["expected"],
                        "output": r["out"], "dropped": r["dropped"]})
            if len(out) >= limit:
                return out
    return out

# ---- cloud optimizer via codex exec (gpt-5.5) ---------------------------
CODEX_TIMEOUT = 240

def call_codex_optimizer(instruction: str) -> str:
    """Run codex exec (gpt-5.5) read-only, return its final text answer."""
    cmd = ["codex", "exec", "-c", "sandbox_mode=read-only", "-c", "approval_policy=never",
           "-c", "model_reasoning_effort=medium", "-c", "service_tier=fast",
           "-c", "features.web_search=false", instruction]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=CODEX_TIMEOUT)
    except subprocess.TimeoutExpired:
        return ""
    txt = r.stdout
    # codex prints a transcript; the assistant's final answer follows the last
    # 'codex' marker. Be tolerant: just return everything after the last
    # 'codex' line if present, else the whole stdout.
    if "\ncodex\n" in txt:
        txt = txt.rsplit("\ncodex\n", 1)[-1]
    # strip a trailing 'tokens used' block
    txt = re.split(r"\ntokens used\n", txt)[0]
    return txt.strip()

OPTIMIZER_INSTR = """You are optimizing a small-model (Phi-4-mini, 3.8B) voice-dictation polish prompt.
The model reads a disfluent transcript and outputs either `OK` (return it verbatim) or `EDIT: <cleaned>`.

CURRENT SYSTEM PROMPT:
---
{system_prompt}
---

CURRENT FEW-SHOT EXAMPLES (input -> output):
{few_shot}

The model made these ERRORS on an eval set (input | expected | model_output | issue):
{failures}

Rules for your rewrite:
- Fix the SHOWN failures with MINIMAL prompt changes. Do NOT rewrite the whole prompt; keep its structure and most wording.
- A restart cue ("sorry sorry", "no wait", "i mean") authorizes dropping the SUPERSEDED clause only; the kept clause MUST be returned verbatim (preserve its typos, NO autocorrection).
- Contrast connectors (but, however, instead, whereas, on the other hand, rather than) join two DISTINCT statements — both stay; NOT a correction cue.
- Leading disfluency fillers ("um", "uh", "well,", "so,", "right,") at the START of an utterance ARE stripped on EDIT, but the same word mid-sentence as content stays.
- Never add apostrophes/capitalization the speaker didn't say. ASR-style lowercase + missing punctuation is preserved verbatim on OK.
- System prompt: <= {word_cap} words. Few-shot: <= {fs_cap} examples; you may ADD one targeted example to fix a shown failure, but do not delete existing ones without reason.

Reply with ONLY a JSON object (no prose):
{{"system_prompt": "<rewritten system prompt>", "few_shot": [["input1","OK"], ["input2","EDIT: ..."]]}}"""

def propose(system_prompt: str, few_shot: FewShot, failures: list[dict]) -> list[dict]:
    fs_str = "\n".join(f"- {i!r} -> {o!r}" for i, o in few_shot) or "(none)"
    fail_str = "\n".join(
        f"- [{f['kind']}] {f['input']!r} | expected {f['expected']!r} | got {f['output']!r}"
        + (f" | dropped={f['dropped']}" if f['dropped'] else "")
        for f in failures) or "(none)"
    instr = OPTIMIZER_INSTR.format(system_prompt=system_prompt, few_shot=fs_str,
                                   failures=fail_str, word_cap=PROMPT_WORD_CAP, fs_cap=FEW_SHOT_CAP)
    raw = call_codex_optimizer(instr)
    # extract first {...} JSON block
    m = re.search(r"\{[\s\S]*\}", raw)
    if not m:
        return []
    try:
        obj = json.loads(m.group(0))
    except json.JSONDecodeError:
        return []
    sp = obj.get("system_prompt")
    fs_raw = obj.get("few_shot", [])
    if not isinstance(sp, str) or not sp.strip():
        return []
    fs: FewShot = tuple((str(a), str(b)) for a, b in fs_raw if isinstance(a, str) and isinstance(b, str))
    if word_count(sp) > PROMPT_WORD_CAP:
        return []
    return [{"system_prompt": sp, "few_shot": fs, "raw": raw}]

# ---- main loop ----------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default=str(ROOT / "llm-polish-bench" / "dataset.jsonl"))
    ap.add_argument("--max-rounds", type=int, default=MAX_ROUNDS)
    ap.add_argument("--k", type=int, default=3, help="worst-of-K eval runs")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default=str(ROOT / "llm-polish-bench" / "out" / "constrained-loop"))
    args = ap.parse_args()

    rows = ev.load_dataset(Path(args.dataset))
    if args.limit:
        rows = rows[:args.limit]
    out_dir = Path(args.out); out_dir.mkdir(parents=True, exist_ok=True)

    print(f"corpus: {len(rows)} rows | k={args.k} | max_rounds={args.max_rounds}", flush=True)
    t0 = time.time()
    llm = polish.load_model()
    print(f"model loaded in {round((time.time()-t0)*1000)}ms: {polish.model_label()}", flush=True)
    print(f"repeat_penalty={os.environ.get('SUNOTO_LLM_POLISH_REPEAT_PENALTY','1.05')} "
          f"grammar={os.environ.get('SUNOTO_LLM_POLISH_GRAMMAR','0')}", flush=True)

    # seed = live prompt
    seed_sp = once.CONSTRAINED_SYSTEM_PROMPT
    seed_fs = tuple(once.CONSTRAINED_REPAIR_FEW_SHOT)
    print(f"\nseed: system_prompt={word_count(seed_sp)}w, few_shot={len(seed_fs)}", flush=True)

    print("\n[baseline] evaluating seed (worst-of-K)...", flush=True)
    base = ev.evaluate(llm, rows, seed_sp, seed_fs, k=args.k)
    _print_summary("BASELINE", base, seed_sp, seed_fs)
    baseline = {"p50_ms": base["p50_ms"], "p95_ms": base["p95_ms"]}
    json.dump({"round": "baseline", "system_prompt": seed_sp, "few_shot": list(seed_fs),
               "metrics": _strip(base)}, open(out_dir / "round00_baseline.json", "w"), indent=2)

    # gate the baseline itself
    ok, reasons = passes_gate(base, baseline)
    if ok:
        print("\n>>> BASELINE already passes the gate; nothing to do.")
        return 0

    best_sp, best_fs, best_m = seed_sp, seed_fs, base
    best_obj = objective(base, baseline, word_count(seed_sp), len(seed_fs))
    stagnation = 0
    history = [{"round": 0, "exact_pct": base["exact_pct"], "content_drop": base["content_drop"],
                "over_edit": base["over_edit"], "obj": best_obj, "promoted": False}]

    for rnd in range(1, args.max_rounds + 1):
        print(f"\n{'='*60}\n[round {rnd}] collecting failures from current best...", flush=True)
        failures = collect_failures(best_m["rows"])
        print(f"  {len(failures)} failures ({sum(1 for f in failures if f['kind']=='content_drop')} content_drop, "
              f"{sum(1 for f in failures if f['kind']=='over_edit')} over_edit, "
              f"{sum(1 for f in failures if f['kind']=='under_edit')} under_edit)", flush=True)
        for f in failures[:8]:
            print(f"    [{f['kind']}] {f['id']}: {f['input'][:50]!r} -> got {f['output'][:45]!r} | exp {f['expected'][:40]!r}")

        print(f"\n[round {rnd}] proposing candidate via codex (gpt-5.5)...", flush=True)
        cands = propose(best_sp, best_fs, failures)
        print(f"  {len(cands)} candidate(s) parsed", flush=True)
        if not cands:
            print("  (no valid candidate; stopping)")
            break

        round_best = None  # (sp, fs, metrics, obj)
        for i, c in enumerate(cands):
            cand_sp, cand_fs = c["system_prompt"], c["few_shot"]
            print(f"\n  [cand {i+1}] sp={word_count(cand_sp)}w fs={len(cand_fs)} -> evaluating (k={args.k})...", flush=True)
            m = ev.evaluate(llm, rows, cand_sp, cand_fs, k=args.k)
            _print_summary(f"  CAND-{i+1}", m, cand_sp, cand_fs)
            ok, reasons = passes_gate(m, baseline)
            json.dump({"round": rnd, "candidate": i+1, "system_prompt": cand_sp,
                       "few_shot": list(cand_fs), "metrics": _strip(m),
                       "passes_gate": ok, "gate_reasons": reasons},
                      open(out_dir / f"round{rnd:02d}_cand{i+1}.json", "w"), indent=2)
            if ok:
                print(f"\n>>> CANDIDATE {i+1} PASSES THE GATE — promoting.", flush=True)
                _promote(cand_sp, cand_fs, out_dir, rnd, i+1)
                history.append({"round": rnd, "exact_pct": m["exact_pct"],
                                "content_drop": m["content_drop"], "over_edit": m["over_edit"],
                                "obj": objective(m, baseline, word_count(cand_sp), len(cand_fs)),
                                "promoted": True})
                _write_report(out_dir, history, base, best_m, round_best=(cand_sp, cand_fs, m) if False else None)
                print(f"\n[loop done in {round((time.time()-t0)/60,1)}m] promoted prompt written to {out_dir/'promoted_prompt.json'}")
                return 0
            obj = objective(m, baseline, word_count(cand_sp), len(cand_fs))
            if round_best is None or obj > round_best[3]:
                round_best = (cand_sp, cand_fs, m, obj)

        # did the round-best improve on current best?
        if round_best and round_best[3] > best_obj + 0.5:
            best_sp, best_fs, best_m, best_obj = round_best
            stagnation = 0
            history.append({"round": rnd, "exact_pct": best_m["exact_pct"],
                            "content_drop": best_m["content_drop"], "over_edit": best_m["over_edit"],
                            "obj": round_best[3], "promoted": False})
            print(f"\n  -> new best (obj={round_best[3]:.2f})", flush=True)
        else:
            stagnation += 1
            history.append({"round": rnd, "exact_pct": best_m["exact_pct"],
                            "content_drop": best_m["content_drop"], "over_edit": best_m["over_edit"],
                            "obj": best_obj, "promoted": False, "stagnant": True})
            print(f"\n  -> no improvement (stagnation={stagnation}/{STAGNATION_ROUNDS})", flush=True)
            if stagnation >= STAGNATION_ROUNDS:
                print("\n>>> STAGNATION limit reached; stopping.", flush=True)
                break

    print("\n>>> No candidate passed the gate within the round budget.", flush=True)
    _write_report(out_dir, history, base, best_m)
    print(f"\n[loop done in {round((time.time()-t0)/60,1)}m] best non-passing metrics saved.")
    print(f"    residual failures -> seed for future finetune (§1.4 of plan).")
    return 1

def _strip(m: dict) -> dict:
    return {k: v for k, v in m.items() if k != "rows"}

def _promote(sp: str, fs: FewShot, out_dir: Path, rnd: int, cand: int) -> None:
    json.dump({"round": rnd, "candidate": cand, "system_prompt": sp, "few_shot": list(fs),
               "note": "passes gate; review then paste into llm_polish_once.py CONSTRAINED_*"},
              open(out_dir / "promoted_prompt.json", "w"), indent=2)

def _write_report(out_dir: Path, history: list, base: dict, best_m: dict, round_best=None) -> None:
    json.dump({"history": history, "baseline": _strip(base), "best_non_promoted": _strip(best_m)},
              open(out_dir / "report.json", "w"), indent=2)

def _print_summary(tag: str, m: dict, sp: str, fs: FewShot) -> None:
    print(f"  [{tag}] exact={m['exact_pct']:.1f}%  drop={m['content_drop']}  over_edit={m['over_edit']}  "
          f"p50={m['p50_ms']:.0f}ms p95={m['p95_ms']:.0f}ms  (sp={word_count(sp)}w fs={len(fs)})", flush=True)
    # show worst categories
    worst = sorted(m["by_cat"].items(), key=lambda kv: kv[1]["exact"]/kv[1]["n"])[:4]
    print(f"           worst cats: " + ", ".join(f"{c}={100*d['exact']/d['n']:.0f}%" for c, d in worst), flush=True)

if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Polish-prompt eval loop with a single-call gpt-5.5 adjudicator.

Per round (see docs/llm-polish-judge-eval-loop-plan-2026-06-30.md):
  1. LOCAL: target model (Phi-4-mini) runs current prompt, worst-of-K.
     deterministic metrics: exact%, per-row content_drop, over_edit,
     malformed_rate, latency p50/p95.
  2. DETERMINISTIC HARD PRE-FILTER (no gpt): malformed_rate==0 and latency
     in budget, else reject candidate (counts toward review-pause).
  3. ONE gpt-5.5 adjudicator call (judge every row + aggregate + decide
     action + propose next prompt if continue). Fail closed on borderline.
  4. ACT: pass->promote&stop; improvement->new best; regression->revert;
     neutral->keep; stop->converged.

Stops only when: adjudicator returns promote_and_stop (gate passed),
adjudicator returns stop (judges best-so-far unbeatable), or a high safety
cap (default 30) is reached -> PAUSES for review (does not hard-stop). The
user directed: loop does not end until most-optimized prompt is reached.
"""
from __future__ import annotations
import argparse, json, os, re, sys, time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "services" / "polish"))
sys.path.insert(0, str(ROOT / "llm-polish-bench"))

import importlib.util
_spec = importlib.util.spec_from_file_location("polish_sidecar", ROOT / "services" / "polish" / "llm_polish_sidecar.py")
polish = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(polish)
import llm_polish_once as once

import eval_constrained as ev
import adjudicator as adj

FewShot = tuple[tuple[str, str], ...]

SAFETY_CAP = 30          # pause-for-review (not hard stop); user wants no premature end
LATENCY_P50_BUDGET = 30
LATENCY_P95_BUDGET = 50

def word_count(s: str) -> int:
    return len(re.findall(r"\S+", s))

def det_metrics(res: dict, k: int) -> dict:
    return {"exact_pct": res["exact_pct"], "content_drop": res["content_drop"],
            "over_edit": res["over_edit"], "malformed_rate": res.get("malformed_rate", 0.0),
            "p50": res["p50_ms"], "p95": res["p95_ms"], "k": k}

def det_prefilter(dm: dict, baseline: dict) -> tuple[bool, list[str]]:
    reasons = []
    if dm["malformed_rate"] != 0:
        reasons.append(f"malformed_rate={dm['malformed_rate']} (want 0)")
    if dm["p50"] > baseline["p50"] + LATENCY_P50_BUDGET:
        reasons.append(f"p50 {dm['p50']:.0f}>{baseline['p50']:.0f}+{LATENCY_P50_BUDGET}")
    if dm["p95"] > baseline["p95"] + LATENCY_P95_BUDGET:
        reasons.append(f"p95 {dm['p95']:.0f}>{baseline['p95']:.0f}+{LATENCY_P95_BUDGET}")
    return (not reasons), reasons

def dump_round(out_dir: Path, rnd: int, sp: str, fs: tuple, res: dict, dm: dict,
               adjudication: dict | None, baseline: dict, notes: str) -> None:
    rec = {"round": rnd, "system_prompt": sp, "few_shot": list(fs),
           "det_metrics": dm, "baseline": baseline, "notes": notes}
    if adjudication:
        rec["adjudication"] = {k: v for k, v in adjudication.items() if k != "per_row"}
        rec["adjudication"]["per_row"] = adjudication.get("per_row", [])
    json.dump(rec, open(out_dir / f"round{rnd:02d}.json", "w"), indent=2)

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default=str(ROOT / "llm-polish-bench" / "dataset.jsonl"))
    ap.add_argument("--max-rounds", type=int, default=SAFETY_CAP)
    ap.add_argument("--k", type=int, default=2, help="worst-of-K target model runs")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default=str(ROOT / "llm-polish-bench" / "out" / "adjudicator-loop"))
    ap.add_argument("--resume", action="store_true", help="resume from out-dir history")
    args = ap.parse_args()

    rows = ev.load_dataset(Path(args.dataset))
    if args.limit:
        rows = rows[:args.limit]
    out_dir = Path(args.out); out_dir.mkdir(parents=True, exist_ok=True)

    print(f"corpus: {len(rows)} rows | k={args.k} | safety_cap={args.max_rounds} (pause-for-review)", flush=True)
    t0 = time.time()
    llm = polish.load_model()
    print(f"model loaded in {round((time.time()-t0)*1000)}ms: {polish.model_label()}", flush=True)
    print(f"repeat_penalty={os.environ.get('SUNOTO_LLM_POLISH_REPEAT_PENALTY','1.05')} "
          f"grammar={os.environ.get('SUNOTO_LLM_POLISH_GRAMMAR','0')}", flush=True)

    seed_sp = once.CONSTRAINED_SYSTEM_PROMPT
    seed_fs = tuple(once.CONSTRAINED_REPAIR_FEW_SHOT)
    print(f"seed: system_prompt={word_count(seed_sp)}w, few_shot={len(seed_fs)}", flush=True)

    history: list[dict] = []
    best_sp, best_fs = seed_sp, seed_fs
    best_quality: float | None = None
    best_adj: dict | None = None
    baseline: dict | None = None
    start_round = 1
    tried: set[str] = set()   # normalized system_prompts already evaluated

    if args.resume and (out_dir / "state.json").exists():
        st = json.load(open(out_dir / "state.json"))
        best_sp, best_fs = st["best_sp"], tuple(tuple(x) for x in st["best_fs"])
        best_quality = st.get("best_quality")
        best_adj = st.get("best_adj")
        baseline = st.get("baseline")
        history = st.get("history", [])
        tried = set(st.get("tried", []))
        start_round = st.get("next_round", 1)
        print(f"resumed: best_so_far quality={best_quality} round={start_round-1}, history={len(history)} rounds", flush=True)

    cur_sp, cur_fs = best_sp, best_fs
    def norm_prompt(s: str) -> str:
        return re.sub(r"\s+", " ", s.strip().lower())

    for rnd in range(start_round, args.max_rounds + 1):
        # anti-repetition: skip re-evaluating an already-tried prompt
        if norm_prompt(cur_sp) in tried:
            print(f"  [round {rnd}] proposed prompt is a near-duplicate of one already tried — reverting to best, will explore fresh direction", flush=True)
            cur_sp, cur_fs = best_sp, best_fs
            if norm_prompt(cur_sp) in tried and len(tried) > 1:
                # best itself was tried; nothing new to eval, advance to force a re-propose
                # by running an adjudication that sees history and must diverge
                pass
        tried.add(norm_prompt(cur_sp))
        print(f"\n{'='*60}\n[round {rnd}] target eval (worst-of-K={args.k}) on current prompt...", flush=True)
        res = ev.evaluate(llm, rows, cur_sp, cur_fs, k=args.k)
        dm = det_metrics(res, args.k)
        print(f"  det: exact={dm['exact_pct']:.1f}% content_drop={dm['content_drop']} over_edit={dm['over_edit']} "
              f"malformed={dm['malformed_rate']} p50={dm['p50']:.0f}ms p95={dm['p95']:.0f}ms "
              f"(sp={word_count(cur_sp)}w fs={len(cur_fs)})", flush=True)

        if baseline is None:
            # round 1 establishes the baseline against which latency+exact are compared
            baseline = {"p50": dm["p50"], "p95": dm["p95"], "exact_pct": dm["exact_pct"]}

        # deterministic hard pre-filter
        ok, reasons = det_prefilter(dm, baseline)
        if not ok:
            print(f"  PRE-FILTER REJECTED: {reasons} — skipping gpt call, revert to best", flush=True)
            dump_round(out_dir, rnd, cur_sp, cur_fs, res, dm, None, baseline, "prefilter_rejected: " + "; ".join(reasons))
            history.append({"round": rnd, "exact_pct": dm["exact_pct"], "quality_score": None,
                            "violations": None, "verdict": "rejected", "action": "prefilter_reject",
                            "sp_words": word_count(cur_sp), "fs_n": len(cur_fs)})
            cur_sp, cur_fs = best_sp, best_fs  # revert
            continue

        # ONE gpt-5.5 adjudicator call
        print(f"  adjudicating via gpt-5.5 (medium reasoning)...", flush=True)
        tc = time.time()
        worst_k_rows = res["rows"]
        corpus = [{"id": r["id"], "category": r.get("category", r.get("cat","")), "input": r["input"], "expected": r["expected"]} for r in rows]
        adjudication, usage = adj.adjudicate(
            corpus, cur_sp, cur_fs, worst_k_rows, dm, history,
            {"quality_score": best_quality, "violations": (adj.violations(best_adj) if best_adj else None),
             "exact_pct": baseline.get("exact_pct"), "p50": baseline["p50"], "p95": baseline["p95"]} if best_adj else None,
        )
        print(f"  gpt-5.5: {round(time.time()-tc,1)}s  tokens={usage['total_tokens']} (reasoning {usage['reasoning_tokens']})", flush=True)
        v = adjudication.get("verdict"); action = adjudication.get("action")
        quality = adjudication.get("quality_score"); viols = adj.violations(adjudication)
        print(f"  VERDICT: {v}  action={action}  quality={quality}  violations={viols}", flush=True)

        he = adj.history_entry(rnd, cur_sp, cur_fs, dm, adjudication)
        history.append(he)
        dump_round(out_dir, rnd, cur_sp, cur_fs, res, dm, adjudication, baseline, f"verdict={v} action={action}")

        # gate check (hybrid)
        if action == "promote_and_stop" or (v == "pass"):
            gate_ok, greasons = adj.passes_gate(adjudication, dm, baseline)
            if gate_ok:
                print(f"\n>>> PROMOTE & STOP: candidate passed the hybrid gate.", flush=True)
                if adjudication.get("next_prompt", {}).get("system_prompt"):
                    np = adjudication["next_prompt"]
                    cur_sp, cur_fs = np["system_prompt"], tuple(tuple(x) for x in np.get("few_shot", []))
                json.dump({"round": rnd, "system_prompt": cur_sp, "few_shot": list(cur_fs),
                           "note": "passed hybrid gate; review then paste into llm_polish_once.py CONSTRAINED_*"},
                          open(out_dir / "promoted_prompt.json", "w"), indent=2)
                _save_state(out_dir, cur_sp, cur_fs, quality, adjudication, baseline, history, rnd+1, tried)
                print(f"    promoted -> {out_dir/'promoted_prompt.json'}", flush=True)
                print(f"\n[loop done in {round((time.time()-t0)/60,1)}m]", flush=True)
                return 0
            else:
                print(f"  action=pass but hybrid gate failed: {greasons} — treating as improvement, continue", flush=True)

        if action == "stop":
            print(f"\n>>> adjudicator STOP: judges best-so-far cannot be beaten (prior rounds better).", flush=True)
            json.dump({"round": rnd, "system_prompt": best_sp, "few_shot": list(best_fs),
                       "note": "adjudicator judged convergence; best-so-far promoted"},
                      open(out_dir / "promoted_prompt.json", "w"), indent=2)
            _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, rnd+1, tried)
            print(f"    best-so-far -> {out_dir/'promoted_prompt.json'}", flush=True)
            print(f"\n[loop done in {round((time.time()-t0)/60,1)}m]", flush=True)
            return 0

        # track best-so-far: MINIMIZE violations first, then MAXIMIZE quality.
        # (The gate is violations==0 ∧ quality≥0.9; a 5-violation candidate
        # must never beat a 2-violation one on quality alone.)
        better = False
        if quality is not None:
            best_viols = adj.violations(best_adj) if best_adj else None
            if best_quality is None or best_viols is None:
                better = True
            elif viols < best_viols:
                better = True  # fewer violations = strictly closer to the gate
            elif viols == best_viols and quality > best_quality + 0.005:
                better = True  # same violations, higher quality
        if better:
            best_sp, best_fs, best_quality, best_adj = cur_sp, cur_fs, quality, adjudication
            print(f"  -> new best-so-far (quality={quality} violations={viols})", flush=True)
        elif v == "regression":
            print(f"  -> regression: reverting to best-so-far for next round's base", flush=True)
            cur_sp, cur_fs = best_sp, best_fs
        # neutral/improvement: adopt the proposed next_prompt as the candidate

        np = adjudication.get("next_prompt") or {}
        if np.get("system_prompt"):
            cur_sp = np["system_prompt"]
            cur_fs = tuple(tuple(x) for x in np.get("few_shot", [])) if np.get("few_shot") else best_fs
            print(f"  -> next prompt: sp={word_count(cur_sp)}w fs={len(cur_fs)} | {np.get('rationale','')[:120]}", flush=True)
        else:
            print(f"  -> no next_prompt provided; reusing best-so-far", flush=True)
            cur_sp, cur_fs = best_sp, best_fs

        _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, rnd+1, tried)

    # safety cap -> pause for review (not hard stop)
    print(f"\n>>> SAFETY CAP ({args.max_rounds}) reached — pausing for review (NOT ended).", flush=True)
    print(f"    best-so-far quality={best_quality}  sp={word_count(best_sp)}w fs={len(best_fs)}", flush=True)
    json.dump({"round": args.max_rounds, "system_prompt": best_sp, "few_shot": list(best_fs),
               "note": "safety-cap pause; resume with --resume"},
              open(out_dir / "promoted_prompt.json", "w"), indent=2)
    _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, args.max_rounds+1, tried)
    print(f"    resume: --resume  | best -> {out_dir/'promoted_prompt.json'}", flush=True)
    print(f"\n[paused in {round((time.time()-t0)/60,1)}m]", flush=True)
    return 2

def _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, next_round, tried=None):
    # best_adj may contain per_row (large); strip for state but keep verdict/quality
    best_adj_slim = {k: v for k, v in (best_adj or {}).items() if k != "per_row"} if best_adj else None
    json.dump({"best_sp": best_sp, "best_fs": list(best_fs), "best_quality": best_quality,
               "best_adj": best_adj_slim, "baseline": baseline, "history": history,
               "tried": list(tried or []), "next_round": next_round},
              open(out_dir / "state.json", "w"), indent=2)

if __name__ == "__main__":
    raise SystemExit(main())

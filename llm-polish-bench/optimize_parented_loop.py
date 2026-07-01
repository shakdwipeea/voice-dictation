#!/usr/bin/env python3
"""Polish-prompt eval loop — parented by the current pi conversation.

ARCHITECTURE
  Two parallel tmux lanes; THIS pi conversation (the user's coding assistant)
  is the parent of both. The eval loop (this file) is lane A.

  Per round:
    1. LOCAL target eval — Phi-4-mini runs current prompt, worst-of-K.
       deterministic metrics: exact%, per-row content_drop, over_edit,
       malformed_rate, latency p50/p95.
    2. DETERMINISTIC HARD PRE-FILTER — malformed_rate==0 and latency in
       budget, else reject candidate.
    3. ONE gpt-5.5 ADJUDICATOR call — judges every row + aggregates vs
       best-so-far + decides action (promote_and_stop|continue|stop) and
       proposes the next prompt (one call, docs/llm-polish-judge-eval-loop-plan-2026-06-30.md).
       The adjudicator is the PER-CANDIDATE judge+proposer.
    4. PARENT DIRECTIVE POLL — the loop reads out_dir/directive.json (written
       by the current pi conversation, the PARENT). If present, the parent's
       decision overrides the adjudicator's: continue | change_strategy | stop
       | promote_and_stop. Used to kill bad directions, switch model, raise K,
       force divergence, or terminate. The parent is the SOLE true terminator.

  NO CAP. Dead-man's switch at 1000 rounds only. A stagnation watchdog
  (N rounds with no best improvement AND no directive consumed) PAUSES and
  emits out_dir/REVIEW_REQUEST.json, then polls for a directive from the
  parent — it does NOT keep burning rounds while stuck, and does NOT stop.

  Fail-closed everywhere: borderline content_preserved counts as a pass;
  malformed candidates never ship; latency regressions reject candidates.
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
polish = importlib.util.module_from_spec(_spec); _spec.loader.exec_module(polish)
import llm_polish_once as once

import eval_constrained as ev
import adjudicator as adj

FewShot = tuple[tuple[str, str], ...]

DEADMAN_CAP = 1000      # pure safety; parent is the real terminator
STAG_PAUSE = 4          # rounds w/o best improvement & no directive -> pause for parent review
POLL_INTERVAL = 60     # seconds between directive polls while paused
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
        rec["adjudication"] = adjudication
    json.dump(rec, open(out_dir / f"round{rnd:04d}.json", "w"), indent=2)

def read_directive(out_dir: Path) -> dict | None:
    p = out_dir / "directive.json"
    if not p.exists():
        return None
    try:
        d = json.load(open(p))
    except Exception:
        return None
    # consume: rename so it's not re-applied
    try: p.rename(out_dir / f"directive.applied.{int(time.time())}.json")
    except Exception: p.unlink(missing_ok=True)
    return d

def write_review_request(out_dir: Path, rnd: int, reason: str, history: list, best: dict | None) -> None:
    req = {"round": rnd, "reason": reason, "best_so_far": best, "history_tail": history[-6:],
           "instruction": "Write out_dir/directive.json with {decision, reasoning, optional: model,k,divergence,override_prompt}. decision in continue|change_strategy|stop|promote_and_stop"}
    json.dump(req, open(out_dir / "REVIEW_REQUEST.json", "w"), indent=2)

def apply_directive(d: dict, best_sp, best_fs, cur_sp, cur_fs, k: int) -> tuple[str, str, FewShot, int, str]:
    """Returns (action, next_sp, next_fs, k, note)."""
    dec = d.get("decision", "continue")
    note = f"directive[{dec}]: " + (d.get("reasoning","")[:160] or "")
    if dec == "stop":
        return "stop", best_sp, best_fs, k, note
    if dec == "promote_and_stop":
        return "promote_and_stop", cur_sp, cur_fs, k, note
    if dec == "change_strategy":
        n_sp, n_fs, n_k = cur_sp, cur_fs, k
        if d.get("override_prompt"):
            n_sp = d["override_prompt"]
            n_fs = tuple(tuple(x) for x in d["few_shot"]) if d.get("few_shot") else cur_fs
        if d.get("k"):
            n_k = int(d["k"])
        # divergence hint: handled by caller forcing exploration
        return "continue", n_sp, n_fs, n_k, note + (" [override]" if d.get("override_prompt") else " [divergence]")
    # continue: keep whatever the loop was going to do
    return "continue", cur_sp, cur_fs, k, note

def _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, next_round):
    best_adj_slim = {k: v for k, v in (best_adj or {}).items() if k != "per_row"} if best_adj else None
    json.dump({"best_sp": best_sp, "best_fs": list(best_fs), "best_quality": best_quality,
               "best_adj": best_adj_slim, "baseline": baseline, "history": history,
               "tried": list(tried), "next_round": next_round},
              open(out_dir / "state.json", "w"), indent=2)

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default=str(ROOT / "llm-polish-bench" / "dataset.jsonl"))
    ap.add_argument("--k", type=int, default=2)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default=str(ROOT / "llm-polish-bench" / "out" / "parented-loop"))
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--deadman", type=int, default=DEADMAN_CAP)
    args = ap.parse_args()

    rows = ev.load_dataset(Path(args.dataset))
    if args.limit:
        rows = rows[:args.limit]
    out_dir = Path(args.out); out_dir.mkdir(parents=True, exist_ok=True)
    print(f"corpus={len(rows)} k={args.k} deadman={args.deadman} | parent: current pi via {out_dir/'directive.json'}", flush=True)

    t0 = time.time()
    llm = polish.load_model()
    print(f"model: {polish.model_label()} ({round((time.time()-t0)*1000)}ms)", flush=True)
    print(f"repeat_penalty={os.environ.get('SUNOTO_LLM_POLISH_REPEAT_PENALTY','1.05')} grammar={os.environ.get('SUNOTO_LLM_POLISH_GRAMMAR','0')}", flush=True)

    seed_sp = once.CONSTRAINED_SYSTEM_PROMPT
    seed_fs = tuple(once.CONSTRAINED_REPAIR_FEW_SHOT)
    print(f"seed: sp={word_count(seed_sp)}w fs={len(seed_fs)}", flush=True)

    history: list[dict] = []
    best_sp, best_fs = seed_sp, seed_fs
    best_quality: float | None = None; best_adj: dict | None = None
    baseline: dict | None = None
    tried: set[str] = set()
    start_round = 1
    last_improved_round = 0
    directives_consumed = 0
    k = args.k

    if args.resume and (out_dir / "state.json").exists():
        st = json.load(open(out_dir / "state.json"))
        best_sp, best_fs = st["best_sp"], tuple(tuple(x) for x in st["best_fs"])
        best_quality = st.get("best_quality"); best_adj = st.get("best_adj")
        baseline = st.get("baseline"); history = st.get("history", [])
        tried = set(st.get("tried", [])); start_round = st.get("next_round", 1)
        last_improved_round = st.get("last_improved_round", 0)
        directives_consumed = st.get("directives_consumed", 0)
        k = st.get("k", k)
        print(f"resumed: best quality={best_quality} round={start_round-1} history={len(history)}", flush=True)

    cur_sp, cur_fs = best_sp, best_fs
    def norm_prompt(s: str) -> str: return re.sub(r"\s+", " ", s.strip().lower())

    rnd = start_round
    while rnd <= args.deadman:
        print(f"\n{'='*60}\n[round {rnd}] target eval (worst-of-K={k}) ...", flush=True)
        res = ev.evaluate(llm, rows, cur_sp, cur_fs, k=k)
        dm = det_metrics(res, k)
        print(f"  det: exact={dm['exact_pct']:.1f}% drops={dm['content_drop']} over={dm['over_edit']} "
              f"malformed={dm['malformed_rate']} p50={dm['p50']:.0f}ms p95={dm['p95']:.0f}ms "
              f"(sp={word_count(cur_sp)}w fs={len(cur_fs)})", flush=True)
        if baseline is None:
            baseline = {"p50": dm["p50"], "p95": dm["p95"], "exact_pct": dm["exact_pct"]}

        ok, reasons = det_prefilter(dm, baseline)
        if not ok:
            print(f"  PRE-FILTER rejected: {reasons} -> revert to best, no gpt call", flush=True)
            dump_round(out_dir, rnd, cur_sp, cur_fs, res, dm, None, baseline, "prefilter_reject: " + "; ".join(reasons))
            history.append({"round": rnd, "exact_pct": dm["exact_pct"], "content_drop": dm["content_drop"],
                            "over_edit": dm["over_edit"], "malformed_rate": dm["malformed_rate"],
                            "p50": dm["p50"], "quality_score": None, "violations": None,
                            "verdict": "rejected", "action": "prefilter_reject",
                            "sp_words": word_count(cur_sp), "fs_n": len(cur_fs)})
            cur_sp, cur_fs = best_sp, best_fs
            _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, rnd+1)
            rnd += 1; continue

        print(f"  adjudicating via gpt-5.5 (codex)...", flush=True)
        tc = time.time()
        corpus = [{"id": r["id"], "category": r.get("category", r.get("cat","")),
                   "input": r["input"], "expected": r["expected"]} for r in rows]
        try:
            adjudication, usage = adj.adjudicate(
                corpus, cur_sp, cur_fs, res["rows"], dm, history,
                {"quality_score": best_quality,
                 "violations": (adj.violations(best_adj) if best_adj else None),
                 "exact_pct": baseline.get("exact_pct"), "p50": baseline["p50"], "p95": baseline["p95"]} if best_adj else None)
        except Exception as e:
            # FAIL SAFE: do not crash the loop. Log, dump det metrics, pause for
            # parent review (adjudicator outage -> parent decides next step).
            msg = f"adjudicator failed: {str(e)[:300]}"
            print(f"  {msg}", flush=True)
            dump_round(out_dir, rnd, cur_sp, cur_fs, res, dm, None, baseline, "adjudicator_error: " + msg)
            history.append({"round": rnd, "exact_pct": dm["exact_pct"], "content_drop": dm["content_drop"],
                            "over_edit": dm["over_edit"], "malformed_rate": dm["malformed_rate"],
                            "p50": dm["p50"], "quality_score": None, "violations": None,
                            "verdict": "adjudicator_error", "action": "pause",
                            "sp_words": word_count(cur_sp), "fs_n": len(cur_fs)})
            _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, rnd+1)
            write_review_request(out_dir, rnd, msg, history,
                                 {"round": rnd, "quality_score": best_quality,
                                  "violations": (adj.violations(best_adj) if best_adj else None),
                                  "sp_words": word_count(best_sp), "fs_n": len(best_fs),
                                  "verdict": best_adj.get("verdict") if best_adj else None})
            print(f"  PAUSED (adjudicator error). Polling {out_dir/'directive.json'} every {POLL_INTERVAL}s...", flush=True)
            while True:
                time.sleep(POLL_INTERVAL)
                d2 = read_directive(out_dir)
                if d2:
                    directives_consumed += 1
                    dact, cur_sp, cur_fs, k, note = apply_directive(d2, best_sp, best_fs, cur_sp, cur_fs, k)
                    print(f"  parent (after adjudicator error): {note}", flush=True)
                    if dact == "stop":
                        _promote(out_dir, best_sp, best_fs, "parent stop after adj error"); return 0
                    if dact == "promote_and_stop":
                        _promote(out_dir, cur_sp, cur_fs, "parent promote after adj error"); return 0
                    break
            (out_dir / "REVIEW_REQUEST.json").unlink(missing_ok=True)
            _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, rnd+1)
            rnd += 1; continue
        print(f"  gpt-5.5: {round(time.time()-tc,1)}s tokens={usage.get('total_tokens')}", flush=True)
        v = adjudication.get("verdict"); action = adjudication.get("action")
        quality = adjudication.get("quality_score"); viols = adj.violations(adjudication)
        print(f"  VERDICT: {v}  action={action}  quality={quality}  violations={viols}", flush=True)
        he = adj.history_entry(rnd, cur_sp, cur_fs, dm, adjudication)
        history.append(he)
        dump_round(out_dir, rnd, cur_sp, cur_fs, res, dm, adjudication, baseline, f"verdict={v} action={action}")

        # parent directive poll (current pi writes directive.json)
        d = read_directive(out_dir)
        if d:
            directives_consumed += 1
            dact, cur_sp, cur_fs, k, note = apply_directive(d, best_sp, best_fs, cur_sp, cur_fs, k)
            print(f"  PARENT DIRECTIVE: {note}", flush=True)
            _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, rnd+1)
            if dact == "stop":
                _promote(out_dir, best_sp, best_fs, "parent said stop; best-so-far promoted")
                print(f"\n>>> parent STOP. done in {round((time.time()-t0)/60,1)}m", flush=True); return 0
            if dact == "promote_and_stop":
                _promote(out_dir, cur_sp, cur_fs, "parent said promote_and_stop")
                print(f"\n>>> parent PROMOTE. done in {round((time.time()-t0)/60,1)}m", flush=True); return 0
            # continue / change_strategy: fall through to best-tracking + adopt proposed/overridden prompt
        else:
            # gate check on adjudicator's own action
            if action == "promote_and_stop" or v == "pass":
                gate_ok, greasons = adj.passes_gate(adjudication, dm, baseline)
                if gate_ok:
                    _promote(out_dir, cur_sp, cur_fs, "adjudicator passed hybrid gate")
                    _save_state(out_dir, cur_sp, cur_fs, quality, adjudication, baseline, history, tried, rnd+1)
                    print(f"\n>>> adjudicator PROMOTE & STOP. done in {round((time.time()-t0)/60,1)}m", flush=True); return 0
                else:
                    print(f"  action=pass but gate failed: {greasons} -> continue", flush=True)
            if action == "stop":
                print(f"  adjudicator stop-action: but deferring to parent. continuing so parent can confirm.", flush=True)

        # best-so-far: MINIMIZE violations, then MAXIMIZE quality (the bug-fix)
        better = False
        if quality is not None:
            best_viols = adj.violations(best_adj) if best_adj else None
            if best_quality is None or best_viols is None: better = True
            elif viols < best_viols: better = True
            elif viols == best_viols and quality > best_quality + 0.005: better = True
        if better:
            best_sp, best_fs, best_quality, best_adj = cur_sp, cur_fs, quality, adjudication
            last_improved_round = rnd
            print(f"  -> new best (quality={quality} viol={viols})", flush=True)
        elif v == "regression":
            cur_sp, cur_fs = best_sp, best_fs

        # adopt next prompt (adjudicator-proposed, unless parent overrode above)
        if not d or d.get("decision") == "continue":
            np = adjudication.get("next_prompt") or {}
            if np.get("system_prompt"):
                cur_sp = np["system_prompt"]
                cur_fs = tuple(tuple(x) for x in np.get("few_shot", [])) if np.get("few_shot") else best_fs
                print(f"  next: sp={word_count(cur_sp)}w fs={len(cur_fs)} | {np.get('rationale','')[:120]}", flush=True)

        tried.add(norm_prompt(cur_sp))
        # stagnation watchdog -> pause for parent review (NOT a stop, NOT a cap)
        if rnd - last_improved_round >= STAG_PAUSE and directives_consumed == 0:
            reason = f"no best improvement in {STAG_PAUSE} rounds and no parent directive consumed"
            write_review_request(out_dir, rnd, reason, history,
                                 {"round": rnd, "quality_score": best_quality,
                                  "violations": (adj.violations(best_adj) if best_adj else None),
                                  "exact_pct": best_adj and None, "sp_words": word_count(best_sp), "fs_n": len(best_fs),
                                  "verdict": best_adj.get("verdict") if best_adj else None})
            print(f"  STAGNATION: {reason}. PAUSED, polling {out_dir/'directive.json'} every {POLL_INTERVAL}s...", flush=True)
            waited = 0
            while True:
                time.sleep(POLL_INTERVAL); waited += POLL_INTERVAL
                d2 = read_directive(out_dir)
                if d2:
                    directives_consumed += 1
                    dact, cur_sp, cur_fs, k, note = apply_directive(d2, best_sp, best_fs, cur_sp, cur_fs, k)
                    print(f"  parent (after {waited}s): {note}", flush=True)
                    last_improved_round = rnd  # reset window
                    if dact == "stop":
                        _promote(out_dir, best_sp, best_fs, "parent stop after stagnation"); return 0
                    if dact == "promote_and_stop":
                        _promote(out_dir, cur_sp, cur_fs, "parent promote after stagnation"); return 0
                    break
                if waited % 300 == 0:
                    print(f"  ...still waiting for parent directive ({waited}s)", flush=True)
            (out_dir / "REVIEW_REQUEST.json").unlink(missing_ok=True)

        _save_state(out_dir, best_sp, best_fs, best_quality, best_adj, baseline, history, tried, rnd+1)
        rnd += 1

    print(f"\n>>> deadman {args.deadman} reached — this should never happen; parent is the real terminator.", flush=True)
    _promote(out_dir, best_sp, best_fs, "deadman reached")
    return 2

def _promote(out_dir: Path, sp: str, fs: tuple, note: str) -> None:
    json.dump({"system_prompt": sp, "few_shot": list(fs), "note": note},
              open(out_dir / "promoted_prompt.json", "w"), indent=2)
    print(f"    promoted -> {out_dir/'promoted_prompt.json'}", flush=True)

if __name__ == "__main__":
    raise SystemExit(main())

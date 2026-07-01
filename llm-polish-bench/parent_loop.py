#!/usr/bin/env python3
"""Detached PARENT-AGENT loop (Option B).

Watches Lane A (optimize_parented_loop.py) state. When a new round completes
(or Lane A pauses for review), invokes codex/gpt-5.5 to decide:
  continue | change_strategy | stop | promote_and_stop
and writes out_dir/directive.json. Lane A consumes it next round.

This is the SOLE terminator of the eval loop — it runs until the seed prompt
converges to the best achievable quality per the gate criteria. Runs forever
in tmux (session: parent-agent).
"""
from __future__ import annotations
import json, os, re, subprocess, sys, time
from pathlib import Path
import parent_agent as pa

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "llm-polish-bench" / "out" / "parented-loop"
POLL_SECS = 20           # how often to re-check for a new completed round
CODEX_TIMEOUT = 300      # seconds per codex decision call
BASELINE_EXACT = 60.0     # ~ seed baseline; parent judges improvement against this

SYSTEM = f"""You are the PARENT AGENT of an autonomous LLM-polish prompt-eval loop.
You run detached in tmux and are the SOLE terminator — the child loop
(optimize_parented_loop.py) keeps running until you judge the seed prompt has
converged to the best achievable quality.

Each invocation: review the child's state + latest round, emit ONE directive
as a single fenced ```json block (no prose outside it).

OUTPUT SCHEMA (strict JSON):
{{
  "decision": "continue | change_strategy | stop | promote_and_stop",
  "reasoning": "<terse; cite round numbers and metric deltas>",
  "k": <int, optional>,               // worst-of-K; bump only if you suspect variance masking real failures
  "divergence": <bool, optional>,     // force the adjudicator to propose a structurally-different prompt
  "override_prompt": "<str, optional, only with change_strategy>",
  "few_shot": [["in","EDIT: out"], ...]  // optional, only with override_prompt
}}

CONVERGENCE GATE (declare 'promote_and_stop' only when ALL hold):
- content_drop_violations == 0  (no major/critical content loss on gold rows)
- exact_pct >= 80  AND improved >= 8pp over baseline (~{BASELINE_EXACT:.0f}%)
- p50 latency < 700ms, p95 < 1500ms
- quality_score stable across the last 2-3 rounds (no regression, no over-edit surge)

DECISION GUIDANCE:
- 'continue': round healthy / progressing / near-converged. DEFAULT to this
  unless you see stagnation, oscillation, a recurring failure class, or gate met.
  Do NOT micro-manage every round.
- 'change_strategy': stuck (stagnation, oscillation, recurring failure the
  adjudicator keeps failing to fix). Prefer 'divergence':true to delegate to
  the adjudicator; provide 'override_prompt' only if you have a specific
  surgical fix the adjudicator isn't landing. Target the dominant recurring
  failure category by name.
- 'promote_and_stop': convergence gate met. Promote best-so-far and stop.
- 'stop': abort immediately — only on fundamental breakage, never on plateau.

RULES:
- Never stop on a quality plateau alone. Run until the gate is met OR you
  judge genuine exhaustion after multiple, distinct strategy changes.
- You do NOT rewrite prompts unless using change_strategy+override_prompt.
  Prefer divergence:true so the adjudicator (gpt-5.5) proposes the edit.
- Be terse. One directive per invocation.
"""

def read_json(p: Path):
    try:
        return json.loads(p.read_text())
    except Exception:
        return None

def latest_round_num(out: Path) -> int:
    nums = []
    for p in out.glob("round*.json"):
        m = re.search(r"round(\d+)", p.name)
        if m:
            nums.append(int(m.group(1)))
    return max(nums) if nums else 0

def extract_json(text: str):
    blocks = list(re.finditer(r"```(?:json)?\s*(.*?)\s*```", text, re.DOTALL))
    cand = blocks[-1].group(1).strip() if blocks else text.strip()
    i = cand.find("{")
    if i < 0:
        return None
    depth = 0; in_str = False; esc = False; out = []
    for ch in cand[i:]:
        out.append(ch)
        if in_str:
            if esc: esc = False
            elif ch == "\\": esc = True
            elif ch == '"': in_str = False
            continue
        if ch == '"': in_str = True
        elif ch == "{": depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return "".join(out)
    return None

def codex_decide(prompt: str) -> dict | None:
    cmd = ["codex", "exec", "-c", "sandbox_mode=read-only", "-c", "approval_policy=never",
           "-c", "model_reasoning_effort=medium", "-c", "service_tier=fast"]
    for attempt in range(3):
        try:
            proc = subprocess.run(cmd, input=prompt, capture_output=True,
                                  text=True, timeout=CODEX_TIMEOUT)
            raw = extract_json(proc.stdout or "")
            if not raw:
                print(f"  [parent] no json in codex output; head={(proc.stdout or '')[:200]}", flush=True)
                continue
            obj = json.loads(raw)
            if "decision" in obj and obj["decision"] in ("continue", "change_strategy", "stop", "promote_and_stop"):
                return obj
            print(f"  [parent] bad decision field: {obj.get('decision')}", flush=True)
        except subprocess.TimeoutExpired:
            print(f"  [parent] codex timeout (attempt {attempt+1})", flush=True)
        except KeyboardInterrupt:
            print(f"  [parent] codex interrupted by SIGINT (attach-peek); retry {attempt+1}", flush=True)
        except Exception as e:
            print(f"  [parent] codex error: {str(e)[:160]}", flush=True)
        time.sleep(2 ** attempt)
    return None

def trajectory_block(state: dict) -> str:
    """Summarize the recent trajectory + best-so-far for the parent."""
    lines = []
    hist = state.get("history", []) or []
    lines.append(f"rounds_completed: {len(hist)}")
    lines.append(f"best_quality: {state.get('best_quality')}")
    best = state.get("best_adjudication") or {}
    if best:
        pr = best.get("per_row", [])
        viols = [v.get("id") for v in pr if v.get("content_preserved") == "no"
                 and v.get("severity") in ("major", "critical")]
        lines.append(f"best_violations: {len(viols)} ids={viols}")
        lines.append(f"best_verdict: {best.get('verdict')} action={best.get('action')}")
    lines.append(f"tried_prompts: {len(state.get('tried', []))}")
    lines.append("\nrecent 5 rounds (round | exact% | drops | over | malformed | p50 | quality | verdict):")
    for h in hist[-5:]:
        lines.append(f"  r{h.get('round')}: exact={h.get('exact_pct')}% drops={h.get('content_drop')} "
                     f"over={h.get('over_edit')} malformed={h.get('malformed_rate')} p50={h.get('p50')}ms "
                     f"q={h.get('quality_score')} v={h.get('verdict')} sp={h.get('sp_words')}w")
    return "\n".join(lines)

def build_prompt(state: dict, last_round: dict | None, review: dict | None) -> str:
    parts = [SYSTEM, "", "=== CURRENT LOOP STATE ===", trajectory_block(state), ""]
    if review:
        parts.append(f"=== STAGNATION REVIEW REQUEST (Lane A paused) ===")
        parts.append(json.dumps(review, indent=2)[:1500])
        parts.append("")
    if last_round:
        pr = last_round.get("per_row") if isinstance(last_round, dict) else None
        # round dump stores adjudication under "adjudication"
        adj = last_round.get("adjudication", {}) if isinstance(last_round, dict) else {}
        dm = last_round.get("det_metrics", {}) if isinstance(last_round, dict) else {}
        parts.append("=== LATEST ROUND ===")
        parts.append(f"round: {last_round.get('round')}")
        parts.append(f"det_metrics: exact={dm.get('exact_pct')}% drops={dm.get('content_drop')} "
                     f"over={dm.get('over_edit')} malformed={dm.get('malformed_rate')} "
                     f"p50={dm.get('p50')}ms p95={dm.get('p95')}ms")
        parts.append(f"adjudication: quality={adj.get('quality_score')} "
                     f"violations={len([v for v in adj.get('per_row',[]) if v.get('content_preserved')=='no' and v.get('severity') in ('major','critical')])} "
                     f"verdict={adj.get('verdict')} action={adj.get('action')}")
        # surface the proposed next_prompt (the adjudicator's edit) so the parent knows what's coming
        np = adj.get("next_prompt")
        if isinstance(np, dict):
            parts.append(f"adjudicator_proposed_next_prompt_words: {len((np.get('system','') or '').split())} "
                         f"fs={len(np.get('few_shot',[]))}")
        elif isinstance(np, str):
            parts.append(f"adjudicator_proposed_next_prompt_words: {len(np.split())}")
        parts.append("")
    parts.append("Emit your directive now as a single fenced ```json block.")
    return "\n".join(parts)

def main():
    out = OUT
    out.mkdir(parents=True, exist_ok=True)
    seen_round = 0
    print(f"[parent-agent] watching {out} (poll={POLL_SECS}s, pi -p headless decisions)", flush=True)
    while True:
        state = read_json(out / "state.json")
        if not state:
            time.sleep(POLL_SECS); continue
        rnd = latest_round_num(out)
        review = read_json(out / "REVIEW_REQUEST.json")
        # act when: a new round completed since our last decision, OR Lane A is paused
        if rnd > seen_round or review:
            last = read_json(out / f"round{rnd:04d}.json") if rnd else None
            prompt = build_prompt(state, last, review)
            t0 = time.time()
            print(f"\n[parent-agent] round {rnd} complete (review={'yes' if review else 'no'}); consulting pi -p...", flush=True)
            # map state -> parent_agent.parent_review(history, best, current, baseline)
            history = state.get("history", []) or []
            ba = state.get("best_adjudication") or {}
            best = None
            if history:
                last_h = history[-1]
                best = {"round": last_h.get("round"),
                        "quality_score": state.get("best_quality"),
                        "violations": (len([v for v in ba.get("per_row", []) if v.get("content_preserved") == "no" and v.get("severity") in ("major", "critical")]) if ba else None),
                        "exact_pct": last_h.get("exact_pct"),
                        "p50": last_h.get("p50"),
                        "sp_words": last_h.get("sp_words"),
                        "fs_n": last_h.get("fs_n"),
                        "verdict": ba.get("verdict") if ba else last_h.get("verdict")}
            current = best or history[-1] if history else None
            dec, raw = pa.parent_review(history, best, current, state.get("baseline"))
            dt = time.time() - t0
            if dec:
                # write directive.json atomically; Lane A will consume it
                tmp = out / "directive.json.tmp"
                tmp.write_text(json.dumps(dec, indent=2))
                tmp.replace(out / "directive.json")
                print(f"[parent-agent] {dt:.1f}s -> directive[{dec.get('decision')}] "
                      f"{dec.get('reasoning','')[:140]}", flush=True)
                seen_round = rnd
                if review:
                    print(f"[parent-agent] consumed REVIEW_REQUEST and wrote directive; "
                          f"Lane A will resume on next poll", flush=True)
                if dec.get("decision") in ("stop", "promote_and_stop"):
                    print(f"[parent-agent] TERMINATOR fired ({dec.get('decision')}). "
                          f"Staying alive to confirm Lane A exits; will idle.", flush=True)
                    # keep watching; if Lane A actually stops, we see no new rounds. stay up.
            else:
                print(f"[parent-agent] pi returned no directive after {dt:.1f}s ({str(raw)[:120]}); "
                      f"will retry on next poll", flush=True)
        time.sleep(POLL_SECS)

if __name__ == "__main__":
    sys.exit(main())

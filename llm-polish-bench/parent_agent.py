"""Pi-as-parent control agent for the polish-prompt eval loop.

After each round (target eval + gpt-5.5 adjudicator), the loop invokes THIS
module: it builds the full trajectory context and calls `pi -p` HEADLESS (the
pi coding agent, non-interactive print mode) to judge whether the LOOP should
continue, change strategy, promote+stop, or stop. No fixed cap — the parent
agent is the sole terminator.

Why pi and not another gpt-5.5 call: the user wants the AI assistant itself
(parent agent) reviewing each round's results and deciding loop fate; pi runs
headless via `pi -p --no-tools` so it can't edit code, only reason and emit
a directive.

Output protocol: pi must emit a single fenced ```json block (validated by
smoke test) so the loop can parse it deterministically.
"""
from __future__ import annotations
import json, re, subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

PARENT_SYSTEM = """You are the PARENT CONTROL AGENT for an automated prompt-optimization loop. The loop is optimizing a small-model (Phi-4-mini, 3.8B, llama.cpp) voice-dictation polish prompt against an 84-row eval corpus.

Every round, another agent (gpt-5.5 adjudicator) already judged the single candidate prompt and proposed the next one. YOUR job is different: you judge the LOOP's trajectory — not one candidate — and decide what to do next.

You receive: round history (each round's deterministic metrics: exact%, content_drop rows, over_edit, malformed_rate, p50/p95 latency; plus gpt-5.5's judged quality_score and content_drop_violations and verdict and action); the best-so-far summary; the current candidate's summary. Reason over the GROUNDED numbers, not vibes.

Decide ONE of:
- "continue": the loop is still making progress (or genuinely trying); run the next round with the adjudicator's proposed next prompt.
- "change_strategy": the loop is stuck/oscillating/regressing repeatedly; the next round should NOT just run the proposed next prompt — instead apply the specific strategy_directive (e.g. force a structurally-different prompt, raise or lower K, target a specific failing category, abandon a direction). The loop will do its best to honor strategy_directive in how it seeds the next round.
- "promote_and_stop": a candidate has clearly passed (gpt-5.5 content_drop_violations==0 AND adjudicator action==promote_and_stop, or the metrics are unambiguously passing: violations==0, quality>=0.9, latency in budget, exact not regressed >3pp). Ship it.
- "stop": converged or definitively stuck; the loop is flailing without learning across >=3 rounds; promote best-so-far and end. Use sparingly — prefer change_strategy when there is ANY untried angle.

Fail-closed when judging content safety: if unsure whether a candidate is actually safe, do NOT promote it.

KEY invariants the loop must never violate (call them out if a proposed prompt would): no cloud LLM at dictation time; deterministic guard logic must stay; system prompt should stay terse (under ~600 words) but quality matters more than token count.

Output ONLY a single fenced ```json block, no prose outside it. Schema:
{"decision":"continue|change_strategy|promote_and_stop|stop","reasoning":"...","strategy_directive":"only if change_strategy; concrete actionable instruction"}`"""

def build_parent_context(history, best, current, baseline) -> str:
    lines = []
    if baseline:
        lines.append(f"BASELINE (round 1, seed prompt): exact={baseline.get('exact_pct')}% "
                     f"p50={baseline.get('p50')}ms p95={baseline.get('p95')}ms")
    if best:
        lines.append(f"BEST-SO-FAR: round={best.get('round')} quality={best.get('quality_score')} "
                     f"violations={best.get('violations')} exact={best.get('exact_pct')}% "
                     f"sp_words={best.get('sp_words')} fs_n={best.get('fs_n')} verdict={best.get('verdict')}")
    lines.append("")
    lines.append("ROUND HISTORY (oldest first):")
    for h in history:
        lines.append(f"- r{h.get('round')}: exact={h.get('exact_pct')}% det_drops={h.get('content_drop', '?')} "
                     f"over_edit={h.get('over_edit','?')} malformed={h.get('malformed_rate','?')} "
                     f"p50={h.get('p50','?')}ms quality={h.get('quality_score','?')} "
                     f"violations={h.get('violations','?')} verdict={h.get('verdict','?')} "
                     f"action={h.get('action','?')} sp_words={h.get('sp_words','?')} fs_n={h.get('fs_n','?')}")
    lines.append("")
    lines.append(f"CURRENT candidate (just evaluated): quality={current.get('quality_score')} "
                 f"violations={current.get('violations')} exact={current.get('exact_pct')}% "
                 f"p50={current.get('p50')}ms verdict={current.get('verdict')} action={current.get('action')}")
    return "\n".join(lines)

def _parse_fenced_json(text: str) -> dict | None:
    # balanced-brace extraction (non-greedy regex truncates at first }); handles
    # both fenced ```json blocks and bare JSON objects.
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
                try:
                    return json.loads("".join(out))
                except Exception:
                    return None
    return None

DEFAULTS = []  # use pi's configured provider/model (neuralwatt/glm-5.2/high)

import time as _time

def parent_review(history, best, current, baseline, timeout: int = 360) -> tuple[dict | None, str]:
    """Invoke `pi -p` headless. Returns (directive_dict_or_None, raw_stdout)."""
    ctx = build_parent_context(history, best, current, baseline)
    prompt = PARENT_SYSTEM + "\n\n" + ctx + "\n\nOutput ONLY a single fenced ```json block."
    last_err = ""
    for attempt in range(3):
        try:
            proc = subprocess.run(
                ["pi", "-p", "--no-tools", "--no-session", *DEFAULTS],
                input=prompt, capture_output=True, text=True, timeout=timeout,
            )
            out = proc.stdout
            parsed = _parse_fenced_json(out)
            if parsed and parsed.get("decision") in ("continue", "change_strategy", "promote_and_stop", "stop"):
                return parsed, out
            last_err = f"no valid decision in pi output (head={out[:200]})"
        except subprocess.TimeoutExpired:
            last_err = "pi -p timeout"
        except KeyboardInterrupt:
            last_err = "pi -p interrupted by SIGINT (attach-peek); retry"
        except Exception as e:
            last_err = f"pi -p error: {str(e)[:160]}"
        _time.sleep(2 ** attempt)
    return None, last_err

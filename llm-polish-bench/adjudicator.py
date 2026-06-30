"""Single-call gpt-5.5 adjudicator for the polish-prompt eval loop.

One call per round does three jobs (see
docs/llm-polish-judge-eval-loop-plan-2026-06-30.md §3.2):
  1. judge each row's output (content_preserved / appropriately_cleaned /
     over_edited / severity / note),
  2. aggregate vs best-so-far (verdict: pass|improvement|regression|neutral),
  3. decide action (promote_and_stop | continue | stop) and, if continue,
     propose the next prompt + few-shot.

Fail-closed: content_preserved=="borderline" counts as a pass (no violation).
Deterministic numbers + actual outputs + round history are passed IN as
context so gpt-5.5 reasons over grounded data, not vibes.
"""
from __future__ import annotations
import json, re, subprocess, time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent

# ---- strict JSON schema for the single response -------------------------
PER_ROW = {
    "type": "object",
    "properties": {
        "id": {"type": "string"},
        "content_preserved": {"type": "string", "enum": ["yes", "no", "borderline"]},
        "appropriately_cleaned": {"type": "number"},
        "over_edited": {"type": "boolean"},
        "severity": {"type": "string", "enum": ["none", "minor", "major", "critical"]},
        "note": {"type": "string"},
    },
    "required": ["id", "content_preserved", "appropriately_cleaned", "over_edited", "severity", "note"],
    "additionalProperties": False,
}
NEXT_PROMPT = {
    "type": "object",
    "properties": {
        "system_prompt": {"type": "string"},
        "few_shot": {"type": "array", "items": {"type": "array", "items": {"type": "string"}, "maxItems": 2}, "description": "list of [input, output] pairs; output is 'OK' or 'EDIT: ...'"},
        "rationale": {"type": "string"},
    },
    "required": ["system_prompt", "few_shot", "rationale"],
    "additionalProperties": False,
}
SCHEMA = {
    "type": "object",
    "properties": {
        "per_row": {"type": "array", "items": PER_ROW},
        "quality_score": {"type": "number", "description": "0.0-1.0, per-row mean of (appropriately_cleaned on edit-needing rows, 1-over_edited on verbatim rows)"},
        "content_drop_violations": {"type": "integer", "description": "count of rows with content_preserved=='no' AND severity in [major,critical]"},
        "verdict": {"type": "string", "enum": ["pass", "improvement", "regression", "neutral"]},
        "action": {"type": "string", "enum": ["promote_and_stop", "continue", "stop"]},
        "next_prompt": NEXT_PROMPT,
    },
    "required": ["per_row", "quality_score", "content_drop_violations", "verdict", "action", "next_prompt"],
    "additionalProperties": False,
}

SYSTEM = """You are the optimizer for a small-model (Phi-4-mini, 3.8B, llama.cpp) voice-dictation polish prompt. You receive, in one call: the eval corpus, the current prompt + few-shot, the target model's actual per-row outputs, deterministic metrics (exact-match %, per-row content-drop flags, over-edit count, format/malformed rate, latency), and the round history (prior prompts' metrics and your prior verdicts).

You must do three things in ONE structured response:

1. JUDGE every row's output. content_preserved: did it keep the speaker's intended meaning and ALL key entities, facts, numbers, emails, and verbatim-required text? When UNCERTAIN whether meaning was lost, answer "borderline" (treated as a pass — fail closed). appropriately_cleaned (0.0-1.0): did it strip GENUINE disfluency (fillers like um/uh/well at utterance start, restarts via sorry/no wait/i mean, redundant repeats) without inventing changes? over_edited: did it change already-clean or verbatim-required text? Typos, casual spellings (thx, idk, gonna), lowercase, missing punctuation, and ASR artifacts are CONTENT, not errors to fix — do not reward "correcting" them. severity: none/minor/major/critical, where major+ means real meaning loss.

2. AGGREGATE vs best-so-far. quality_score is the per-row mean of: appropriately_cleaned on rows the gold marks as needing edit; (1.0 if not over_edited else 0.0) on rows the gold marks OK/verbatim. content_drop_violations = rows where content_preserved=="no" AND severity in [major, critical]. verdict: "pass" if content_drop_violations==0 AND quality_score is acceptable (>=0.9) AND better-than-or-equal best-so-far; "improvement" if better than best-so-far but not yet passing; "regression" if worse than best-so-far; "neutral" if roughly equal.

3. DECIDE action. "promote_and_stop" iff verdict=="pass" — the prompt ships. "stop" if you judge best-so-far cannot be beaten (prior rounds scored better and no further change is worth trying). Otherwise "continue" and provide next_prompt: a MINIMAL edit of the current prompt to fix the SHOWN failures (contrast/restart/verbatim rules apply; system prompt stays terse under ~600 words; few-shot small — you may ADD one targeted example to fix a shown failure, do not delete existing ones without reason; each few-shot output strictly "OK" or "EDIT: <text>"). If action is promote_and_stop or stop, you MAY still provide next_prompt (ignored) — but keep it minimal.

Many valid cleanups exist (exact-match is NOT the gate). Judge semantic content-preservation primarily. Gold is ONE acceptable answer, not the only one.

Return ONLY the JSON schema. No prose outside it."""

ANCHORED = [
    # session-8 chatbot-refusal: MALFORMED, chatbot reply pasted. Major violation.
    {"input": "in our config", "gold": "in our config", "output": "It seems like the transcript is incomplete. Could you please provide the full sentence?",
     "verdict": {"id": "refusal1", "content_preserved": "no", "appropriately_cleaned": 0.0, "over_edited": True,
                 "severity": "critical", "note": "model broke the OK/EDIT contract and emitted a chatbot refusal; content replaced entirely."}},
    # dropped "however" connector: minor, not a violation.
    {"input": "She did not call back, however I left a voicemail.", "gold": "She did not call back, however I left a voicemail.",
     "output": "She did not call back, I left a voicemail.",
     "verdict": {"id": "contrast1", "content_preserved": "borderline", "appropriately_cleaned": 0.7, "over_edited": True,
                 "severity": "minor", "note": "dropped contrastive 'however'; meaning mostly preserved but over-edited."}},
    # over-edit: thx -> Thanks. regression.
    {"input": "thx for the help", "gold": "thx for the help", "output": "Thanks for the help",
     "verdict": {"id": "over1", "content_preserved": "yes", "appropriately_cleaned": 0.3, "over_edited": True,
                 "severity": "minor", "note": "autocorrected casual 'thx' to 'Thanks'; verbatim text should be preserved."}},
    # filler stripped correctly: improvement.
    {"input": "Um, I went to the store.", "gold": "I went to the store.", "output": "I went to the store.",
     "verdict": {"id": "filler1", "content_preserved": "yes", "appropriately_cleaned": 1.0, "over_edited": False,
                 "severity": "none", "note": "leading filler 'Um,' stripped; content preserved; good cleanup."}},
]

def _gold_decision(gold: str, inp: str) -> str:
    from eval_constrained import normalize
    return "EDIT" if normalize(gold) != normalize(inp) else "OK"

def build_context(
    corpus: list[dict],
    current_sp: str,
    current_fs: tuple,
    worst_k_rows: list[dict],   # list of {id, input, expected, out, dropped, lat, cat}
    det: dict,                  # {exact_pct, content_drop, over_edit, malformed_rate, p50, p95, k}
    history: list[dict],        # [{round, sp, fs_n, exact_pct, quality_score, violations, verdict, action}]
    baseline: dict | None,
) -> str:
    """Assemble the user-message context block. Compact to keep tokens tight."""
    lines = []
    lines.append("CORPUS (id | category | gold_decision | input | gold):")
    for r in corpus:
        gd = _gold_decision(r["expected"], r["input"])
        lines.append(f"- {r['id']} | {r['category']} | {gd} | in:{r['input']!r} | gold:{r['expected']!r}")
    lines.append("")
    lines.append("CURRENT PROMPT (system_prompt):")
    lines.append(current_sp)
    lines.append("CURRENT FEW-SHOT:")
    for i, (a, b) in enumerate(current_fs):
        lines.append(f"  {i+1}. {a!r} -> {b!r}")
    lines.append("")
    lines.append(f"TARGET MODEL OUTPUTS (worst-of-K={det['k']}; this round, det exact={det['exact_pct']:.1f}% "
                 f"content_drop_rows={det['content_drop']} over_edit={det['over_edit']} malformed_rate={det['malformed_rate']} "
                 f"p50={det['p50']:.0f}ms p95={det['p95']:.0f}ms):")
    for r in worst_k_rows:
        drop = f" DET_FLAGGED_DROPPED={r.get('dropped')}" if r.get("dropped") else ""
        lines.append(f"- {r['id']} | out:{r['out']!r}{drop}")
    if baseline:
        lines.append("")
        lines.append(f"BASELINE (round 0): quality={baseline.get('quality_score','?')} violations={baseline.get('violations','?')} "
                     f"exact={baseline.get('exact_pct','?')}% p50={baseline.get('p50','?')}ms p95={baseline.get('p95','?')}ms")
    if history:
        lines.append("")
        lines.append("ROUND HISTORY (most recent last):")
        for h in history:
            lines.append(f"- round {h.get('round')}: exact={h.get('exact_pct','?')}% quality={h.get('quality_score','?')} "
                         f"violations={h.get('violations','?')} verdict={h.get('verdict','?')} action={h.get('action','?')} "
                         f"sp_words={h.get('sp_words','?')} fs_n={h.get('fs_n','?')}")
    lines.append("")
    lines.append("ANCHORED EXAMPLES (how to judge):")
    for a in ANCHORED:
        v = a["verdict"]
        lines.append(f"- {v['id']}: in:{a['input']!r} gold:{a['gold']!r} out:{a['output']!r} -> content_preserved={v['content_preserved']} "
                     f"cleaned={v['appropriately_cleaned']} over_edited={v['over_edited']} severity={v['severity']} | {v['note']}")
    return "\n".join(lines)

def _extract_json(text: str) -> str | None:
    """Extract the largest balanced JSON object. codex emits multi-line indented
    JSON inside a ```json fence; a non-greedy regex truncates at the first }.
    We grab the span from the last opening ```json to its fence close, then
    within that, from the first '{' to its matching '}' via brace counting."""
    # prefer a fenced ```json ... ``` block (last one if several)
    blocks = list(re.finditer(r"```(?:json)?\s*(.*?)\s*```", text, re.DOTALL))
    candidate = blocks[-1].group(1).strip() if blocks else text.strip()
    # walk to first '{' then balance braces (ignoring strings)
    i = candidate.find("{")
    if i < 0:
        return None
    depth = 0; in_str = False; esc = False
    out = []
    for ch in candidate[i:]:
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

def _normalize_codex_verdict(obj: dict) -> dict:
    """Map codex's actual output shape to our schema. Codex emits:
      {rows:[{...notes...}], aggregate:{quality_score,content_drop_violations,verdict,...}, action, next_prompt}.
    We also accept our canonical shape as-is."""
    if "per_row" not in obj and "rows" in obj:
        obj["per_row"] = obj.pop("rows")
    for v in obj.get("per_row", []):
        if "note" not in v and "notes" in v:
            v["note"] = v.pop("notes")
    agg = obj.get("aggregate")
    if isinstance(agg, dict):
        for k in ("quality_score", "content_drop_violations", "verdict"):
            if k not in obj and k in agg:
                obj[k] = agg[k]
    return obj

def _codex_complete(prompt: str, reasoning_effort: str = "medium", retries: int = 3, timeout: int = 600) -> dict:
    """Run codex exec (gpt-5.5) and parse a fenced ```json block from stdout.
    codex stdout is the model response only (banner/tokens go to stderr)."""
    cmd = ["codex", "exec", "-c", "sandbox_mode=read-only", "-c", "approval_policy=never",
           "-c", f"model_reasoning_effort={reasoning_effort}", "-c", "service_tier=fast"]
    last = None
    for i in range(retries):
        try:
            proc = subprocess.run(cmd, input=prompt, capture_output=True, text=True, timeout=timeout)
            raw = _extract_json(proc.stdout or "")
            if not raw:
                last = "no json block found in codex output; head=" + (proc.stdout or "")[:300]
                continue
            obj = _normalize_codex_verdict(json.loads(raw))
            missing = [k for k in ("per_row", "quality_score", "content_drop_violations",
                                   "verdict", "action", "next_prompt") if k not in obj]
            if not missing:
                return obj
            last = f"schema missing keys after normalize: {missing}"
        except subprocess.TimeoutExpired:
            last = "codex exec timeout"
        except Exception as e:
            last = str(e)[:200]
        time.sleep(2 ** i)
    raise RuntimeError(f"codex adjudicator failed after {retries} tries: {last}")

def adjudicate(
    corpus, current_sp, current_fs, worst_k_rows, det, history, baseline,
) -> tuple[dict, dict]:
    """One gpt-5.5 (codex) adjudicator call. Returns (adjudication_obj, usage)."""
    ctx = build_context(corpus, current_sp, current_fs, worst_k_rows, det, history, baseline)
    prompt = SYSTEM + "\n\n" + ctx + "\n\nOutput ONLY a single fenced ```json block. No prose outside it."
    obj = _codex_complete(prompt, reasoning_effort="medium")
    # codex doesn't expose token usage cleanly on stdout; mark unknown
    usage = {"total_tokens": None, "reasoning_tokens": None}
    obj["usage"] = usage
    return obj, usage

# ---- action application -------------------------------------------------
def violations(adj: dict) -> int:
    """HARD content-preservation count: content_preserved=='no' AND severity in [major,critical]."""
    n = 0
    for v in adj.get("per_row", []):
        if v.get("content_preserved") == "no" and v.get("severity") in ("major", "critical"):
            n += 1
    return n

def passes_gate(adj: dict, det: dict, baseline: dict) -> tuple[bool, list[str]]:
    reasons = []
    if det.get("malformed_rate", 0) != 0:
        reasons.append(f"malformed_rate={det['malformed_rate']} (want 0)")
    if violations(adj) != 0:
        reasons.append(f"content_drop_violations={violations(adj)} (want 0)")
    if det.get("p50", 0) > baseline["p50"] + 30:
        reasons.append(f"p50 {det['p50']:.0f}>{baseline['p50']:.0f}+30")
    if det.get("p95", 0) > baseline["p95"] + 50:
        reasons.append(f"p95 {det['p95']:.0f}>{baseline['p95']:.0f}+50")
    if adj.get("quality_score", 0) < 0.9:
        reasons.append(f"quality_score={adj['quality_score']:.2f}<0.9")
    if det.get("exact_pct", 100) < baseline.get("exact_pct", det.get("exact_pct", 0)) - 3:
        reasons.append(f"exact {det['exact_pct']:.1f}% dropped >3pp below baseline")
    return (not reasons), reasons

def history_entry(rnd: int, sp: str, fs: tuple, det: dict, adj: dict) -> dict:
    return {"round": rnd, "sp_words": len(re.findall(r"\S+", sp)), "fs_n": len(fs),
            "exact_pct": det["exact_pct"], "quality_score": adj.get("quality_score"),
            "violations": violations(adj), "verdict": adj.get("verdict"),
            "action": adj.get("action")}

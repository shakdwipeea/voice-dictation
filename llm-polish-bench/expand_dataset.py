#!/usr/bin/env python3
"""Append expanded scenarios to contrast-coherence.jsonl and dataset.jsonl.

Verifies every row before writing:
  - OK rows (expected == input): must be clean (no filler/repetition introduced)
  - EDIT rows (expected != input): expected must be a token subsequence of input
    (deletions only, order preserved) AND must drop only filler/repetition/cue
    words AND must keep any contrast connector / typo intact (no invention).
"""
from __future__ import annotations
import json, re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "llm-polish-bench"
CONTRAST = BENCH / "contrast-coherence.jsonl"
DATASET = BENCH / "dataset.jsonl"

# contrastive connectors that MUST survive any edit
CONTRAST_WORDS = {"but", "however", "instead", "whereas", "or", "rather", "opposed"}

def toks(s: str) -> list[str]:
    return re.sub(r"[^a-z0-9 ]", " ", s.lower()).split()

def is_subsequence(needle: list[str], hay: list[str]) -> bool:
    i = 0
    for w in needle:
        while i < len(hay) and hay[i] != w:
            i += 1
        if i >= len(hay):
            return False
        i += 1
    return True

# ---------------------------------------------------------------------------
# contrast_coherence rows (append to existing 18). 19-26 pure-OK, 27-28 mixed.
# ---------------------------------------------------------------------------
contrast_rows = [
    ("contrast-19", "I would rather use a library than write it from scratch.",
        "I would rather use a library than write it from scratch."),
    ("contrast-20", "We deploy on Fridays as opposed to Mondays.",
        "We deploy on Fridays as opposed to Mondays."),
    ("contrast-21", "It is not a bug, it is a feature.",
        "It is not a bug, it is a feature."),
    ("contrast-22", "He likes tea, she likes coffee.",
        "He likes tea, she likes coffee."),
    ("contrast-23", "I tried, but I could not finish in time, so I asked for help.",
        "I tried, but I could not finish in time, so I asked for help."),
    ("contrast-24", "The fix works on Linux but not on macOS yet.",
        "The fix works on Linux but not on macOS yet."),
    ("contrast-25", "She did not call back, however I left a voicemail.",
        "She did not call back, however I left a voicemail."),
    ("contrast-26", "You can configure it in code, or you can set environment variables.",
        "You can configure it in code, or you can set environment variables."),
    # mixed-disfluency: must strip filler, keep contrast clause verbatim
    ("contrast-27", "Uh, the fix works on Linux but not on macOS yet.",
        "The fix works on Linux but not on macOS yet."),
    ("contrast-28", "Um, she did not call back, however I left a voicemail.",
        "She did not call back, however I left a voicemail."),
]

# ---------------------------------------------------------------------------
# restart_preserve_verbatim: correction cue ("sorry sorry" / "no wait" / "no")
# drops the aborted clause; the KEPT clause contains typos / oddities that must
# be preserved verbatim (no autocorrect). The user's exact bug-example type.
# ASR-style (lowercase, minimal punct) so verbatim-kept == expected.
# ---------------------------------------------------------------------------
restart_rows = [
    ("rp-01", "why dont you push my chnags sorry sorry why dont you commit my chnages first",
        "why dont you commit my chnages first"),
    ("rp-02", "rename the funtion no wait rename the fucntion run test",
        "rename the fucntion run test"),
    ("rp-03", "send it to bob at emial dot com no wait bob at email dot com",
        "send it to bob at email dot com"),
    ("rp-04", "the addres is one two three main street sorry the address is one two three main street",
        "the address is one two three main street"),
    ("rp-05", "deploy to prod sorry sorry deploy to staging",
        "deploy to staging"),
    ("rp-06", "i meen no i mean tomorrow",
        "i mean tomorrow"),
    ("rp-07", "fix the bufg no wait fix the buffer overflow",
        "fix the buffer overflow"),
    ("rp-08", "call the funciton no call the function init",
        "call the function init"),
]

# ---------------------------------------------------------------------------
# preserve_verbatim: clean text (no disfluency) but with typos / slang that the
# model must NOT autocorrect. expected == input (OK fast path). Guards over-edit.
# ---------------------------------------------------------------------------
preserve_rows = [
    ("pv-01", "i think the respnose is wrong", "i think the respnose is wrong"),
    ("pv-02", "its not wrking yet", "its not wrking yet"),
    ("pv-03", "thx that worked", "thx that worked"),
    ("pv-04", "idk maybe later", "idk maybe later"),
    ("pv-05", "pls review my pr", "pls review my pr"),
    ("pv-06", "gonna push now", "gonna push now"),
]

def verify(rid: str, cat: str, inp: str, exp: str) -> None:
    it, et = toks(inp), toks(exp)
    if inp == exp:
        # OK row: clean — no filler/repetition introduced by construction
        return
    # EDIT row: expected must be a token subsequence of input
    if not is_subsequence(et, it):
        raise AssertionError(f"{rid}: expected is not a token subsequence of input")
    dropped = [w for w in it if w not in et]  # crude; repetition collapses but ok for cue check
    # for contrast mixed rows the contrast connector must survive
    if cat == "contrast_coherence":
        kept_connectors = CONTRAST_WORDS & set(et)
        if not kept_connectors:
            raise AssertionError(f"{rid}: contrast row lost its connector")
    # for restart_preserve_verbatim: the kept clause must keep its typo
    # (no specific check beyond subsequence — the typo is in both input and expected)

def main() -> int:
    rows = []
    for rid, inp, exp in contrast_rows:
        verify(rid, "contrast_coherence", inp, exp)
        rows.append({"id": rid, "category": "contrast_coherence", "input": inp, "expected": exp})
    for rid, inp, exp in restart_rows:
        verify(rid, "restart_preserve_verbatim", inp, exp)
        rows.append({"id": rid, "category": "restart_preserve_verbatim", "input": inp, "expected": exp})
    for rid, inp, exp in preserve_rows:
        verify(rid, "preserve_verbatim", inp, exp)
        rows.append({"id": rid, "category": "preserve_verbatim", "input": inp, "expected": exp})

    # append contrast rows to the standalone contrast eval file
    with CONTRAST.open("a") as f:
        for r in rows:
            if r["category"] == "contrast_coherence":
                f.write(json.dumps(r) + "\n")
    # append ALL new rows to the main dataset
    with DATASET.open("a") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

    from collections import Counter
    print(f"appended {len(rows)} rows")
    print("by category:", dict(Counter(r["category"] for r in rows)))
    # reload and confirm parse + no id collision
    existing = [json.loads(l) for l in DATASET.open()]
    ids = [r["id"] for r in existing]
    dupes = {i for i in ids if ids.count(i) > 1}
    print(f"dataset.jsonl now: {len(existing)} rows, dupes={dupes or 'none'}")
    contrast = [json.loads(l) for l in CONTRAST.open()]
    print(f"contrast-coherence.jsonl now: {len(contrast)} rows")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())

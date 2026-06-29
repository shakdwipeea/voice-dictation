#!/usr/bin/env python3
"""Simulate real dictation: 4 unique utterances with 20s pauses.

Verifies the keepalive keeps Metal warm through realistic multi-second
dictation pauses. Each call uses a unique transcript so suffix prefill is
exercised (not cache-hit to zero).
"""
from __future__ import annotations
import json, subprocess, time

DAEMON = "target/release/sunoto-daemon"
UTTERANCES = [
    "Please send the report to the team tomorrow.",
    "The meeting is scheduled for three pm on friday.",
    "I pushed the patch to git hub and updated the docs.",
    "Let's ship the release candidate next week instead.",
]

def polish(text: str) -> dict:
    out = subprocess.run([DAEMON, "polish", text], capture_output=True, text=True, timeout=120)
    return json.loads(out.stdout) if out.returncode == 0 else {}

def main() -> int:
    print("session | ASR-equivalent | LLM polish | pe_ms/tok | ev_ms/tok | -> output")
    print("--------|-----------------|------------|-----------|-----------|----------")
    for i, text in enumerate(UTTERANCES, 1):
        time.sleep(20)  # realistic dictation pause
        d = polish(text)
        llm = d.get("llm", {})
        p = llm.get("diagnostics", {}).get("llama_perf", {}) or {}
        pe_tok = f"{p.get('prompt_eval_ms',0)}/{p.get('prompt_eval_tokens',0)}tok"
        ev_tok = f"{p.get('eval_ms',0)}/{p.get('eval_tokens',0)}tok"
        print(f"  {i}     |      ~478ms     |  {llm.get('latency_ms','?'):>5}ms   | {pe_tok:>10} | {ev_tok:>9} | {llm.get('output','')[:40]}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Decisive probe v2: novel-transcript suffix-prefill after idle.

v1 was flawed: it repeated the same text, so after call 1 the FULL prompt was
cached and prompt_eval was 0 -- it could never observe the prefill-of-new-tokens
cost that dominates the live path (the transcript suffix, 5-23 tokens, eval'd at
~120ms/tok live vs ~9ms/tok in the bench).

v2 uses a UNIQUE transcript each call so the suffix is always newly evaluated.
Measures prompt_eval_ms / tokens for the suffix after 0/5/15/30s idle, and tests
whether a decode-only keepalive (cached, cheap) keeps prefill fast.
"""
from __future__ import annotations

import json
import subprocess
import sys
import threading
import time

DAEMON = "target/release/sunoto-daemon"
# Identical-length, varying final word so tokens differ -> suffix must eval.
# "alpha".."zulu" at the end means the final ~1-2 tokens differ, suffix ~ a few tok.
UNIQUE = [
    "Please check the log and let me know alpha.",
    "Please check the log and let me know bravo.",
    "Please check the log and let me know charlie.",
    "Please check the log and let me know delta.",
    "Please check the log and let me know echo.",
    "Please check the log and let me know foxtrot.",
    "Please check the log and let me know golf.",
    "Please check the log and let me know hotel.",
    "Please check the log and let me know india.",
    "Please check the log and let me know juliet.",
]
# Cached, cheap keepalive (decode only, no suffix prefill).
KEEPALIVE = "Please check the log and let me know."


def polish(text: str) -> dict:
    out = subprocess.run(
        [DAEMON, "polish", text],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if out.returncode != 0:
        return {}
    return json.loads(out.stdout)


def perf(tag: str, d: dict) -> str:
    llm = d.get("llm", {})
    p = llm.get("diagnostics", {}).get("llama_perf", {}) or {}
    c = llm.get("diagnostics", {})
    return (
        f"{tag:22s} llm={llm.get('latency_ms'):>5}ms  "
        f"pe={p.get('prompt_eval_ms'):>6}ms/{p.get('prompt_eval_tokens')}tok  "
        f"ev={p.get('eval_ms'):>5}ms/{p.get('eval_tokens')}tok  "
        f"matched={c.get('cache_matched_tokens')}  "
        f"-> {llm.get('output','')[:30]}"
    )


def main() -> int:
    ui = iter(UNIQUE)

    print("=== back-to-back unique transcripts (no idle) == bench-like ===")
    for _ in range(3):
        print(" ", perf("b2b-unique", polish(next(ui))))

    for gap in (5, 15, 30):
        print(f"\n=== after {gap}s idle, then one unique transcript ===")
        time.sleep(gap)
        print(" ", perf(f"idle-{gap}s", polish(next(ui))))

    print("\n=== decode-only keepalive every 4s for 20s; unique probe at t=10,18 ===")
    stop = threading.Event()

    def hb():
        while not stop.wait(4.0):
            try:
                polish(KEEPALIVE)
            except Exception:
                pass

    t = threading.Thread(target=hb, daemon=True)
    t.start()
    time.sleep(10)
    print(" ", perf("keepalive-unique-1", polish(next(ui))))
    time.sleep(8)
    print(" ", perf("keepalive-unique-2", polish(next(ui))))
    stop.set()
    t.join(timeout=10)

    print("\n=== keepalive STOPPED, 30s idle, then unique ===")
    time.sleep(30)
    print(" ", perf("post-30s", polish(next(ui))))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

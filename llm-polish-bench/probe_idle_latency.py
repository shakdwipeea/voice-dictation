#!/usr/bin/env python3
"""Controlled probe: does a heartbeat keep the LLM polish sidecar warm?

Decisive experiment for the live-path latency root cause. The live daemon showed
warm ~26ms/tok generation but 1.3-2.8s prompt_eval for a tiny suffix after idle,
vs ~0ms prompt_eval in back-to-back bench calls. Hypothesis: Metal GPU cold-ramp
on the first prefill batch after idle; a periodic keepalive should keep it hot.

Runs through the daemon's `polish` control socket (the real polish path). Prints
per-probe llama-perf so we can see prompt_eval_ms vs eval_ms separately.

Usage: probe_idle_latency.py
"""
from __future__ import annotations

import json
import subprocess
import sys
import threading
import time

DAEMON = "target/release/sunoto-daemon"
# A clean, unchanged transcript (cache-hits the shared prefix; tiny suffix).
PROBE_TEXT = "Please check the log and let me know."
# A different keepalive string; still a cache-prefix hit, exercises the path.
KEEPALIVE_TEXT = "Send the report tomorrow."


def polish(text: str) -> dict:
    t0 = time.time()
    out = subprocess.run(
        [DAEMON, "polish", text],
        capture_output=True,
        text=True,
        timeout=120,
    )
    wall = (time.time() - t0) * 1000
    if out.returncode != 0:
        print(f"  ERROR rc={out.returncode}: {out.stderr[:200]}", file=sys.stderr)
        return {}
    d = json.loads(out.stdout)
    d["_wall_ms"] = round(wall)
    return d


def perf_line(label: str, d: dict) -> str:
    p = (d.get("llm") or {}).get("diagnostics", {}).get("llama_perf", {}) or {}
    llm = (d.get("llm") or {})
    cache = llm.get("diagnostics", {})
    return (
        f"{label:24s} wall={d.get('_wall_ms'):>5}ms "
        f"llm_latency={llm.get('latency_ms'):>5}ms  "
        f"pe={p.get('prompt_eval_ms'):>6}ms/{p.get('prompt_eval_tokens')}tok  "
        f"ev={p.get('eval_ms'):>5}ms/{p.get('eval_tokens')}tok  "
        f"reused={p.get('reused_tokens')}  "
        f"cache_matched={cache.get('cache_matched_tokens')} "
        f"-> {llm.get('output','')[:40]}"
    )


def main() -> int:
    print("=== baseline (immediate, warm from restart) ===")
    print(" ", perf_line("baseline-warm", polish(PROBE_TEXT)))

    for gap in (5, 15, 30):
        print(f"\n=== after {gap}s idle ===")
        time.sleep(gap)
        print(" ", perf_line(f"idle-{gap}s", polish(PROBE_TEXT)))

    print("\n=== heartbeat every 4s for 20s, probe at t=10 and t=18 ===")
    stop = threading.Event()

    def heartbeat() -> None:
        while not stop.is_set():
            try:
                polish(KEEPALIVE_TEXT)
            except Exception as e:
                print(f"  heartbeat err: {e}", file=sys.stderr)
            stop.wait(4.0)

    hb = threading.Thread(target=heartbeat, daemon=True)
    hb.start()
    time.sleep(10)
    print(" ", perf_line("heartbeat-probe-1", polish(PROBE_TEXT)))
    time.sleep(8)
    print(" ", perf_line("heartbeat-probe-2", polish(PROBE_TEXT)))
    stop.set()
    hb.join(timeout=10)

    print("\n=== after heartbeat STOPPED: 30s idle, then probe ===")
    time.sleep(30)
    print(" ", perf_line("post-heartbeat-30s", polish(PROBE_TEXT)))

    print("\n=== verdict ===")
    print("If 'heartbeat-probe-*' pe_ms is small (<200) but 'idle-*' and "
          "'post-heartbeat-30s' pe_ms is large (>1000), heartbeat IS the fix.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

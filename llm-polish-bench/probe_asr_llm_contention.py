#!/usr/bin/env python3
"""Probe ASR<->LLM Metal GPU contention (cross-process, like the real daemon).

The ASR sidecar (parakeet-mlx, .venv-nemotron-mac) and LLM polish sidecar
(llama.cpp, .venv-llm-polish-mac) are SEPARATE processes sharing one Metal GPU.

Experiments:
  1. Warm LLM baseline (unique transcripts, cached prefix).
  2. Run an ASR burst subprocess (parakeet generate) -> immediately measure LLM.
     Does ASR freight cold the LLM prefill?
  3. VARIANT A: ASR burst -> tiny 4-token LLM re-warm ping -> real LLM polish.
     Does the re-warm restore warm prefill, cheaper than the cold polish it
     replaces?

Usage:
  .venv-llm-polish-mac/bin/python llm-polish-bench/probe_asr_llm_contention.py
"""
from __future__ import annotations

import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "services", "polish"))
os.environ.setdefault("SUNOTO_LLM_POLISH_KEEPALIVE_S", "0")  # disable keepalive

import llm_polish_sidecar as s  # noqa: E402

ASR_PY = os.path.join(ROOT, ".venv-nemotron-mac/bin/python")
ASR_SCRIPT = os.path.join(ROOT, "llm-polish-bench/_asr_burst.py")

UNIQUE = [
    "Please send the report to the team tomorrow.",
    "The meeting is at three pm on friday afternoon.",
    "I pushed the patch to the repo and updated the docs.",
    "Let us ship the release candidate next week instead.",
    "Can you review the pull request before standup today?",
    "We should schedule the design review for next tuesday.",
    "The deployment failed because of a missing config flag.",
    "I will send the invoice to accounting by end of day.",
    "Could you double check the merge conflict resolution?",
    "The quarterly metrics look better than we projected yesterday.",
    "Remind me to follow up with the vendor about pricing.",
    "Let us pause the rollout until the patch lands.",
]
_idx = 0


def next_unique() -> str:
    """Cycle through fresh transcripts so suffix prefill is always exercised."""
    global _idx
    t = UNIQUE[_idx % len(UNIQUE)]
    _idx += 1
    return t

LLM = None


def llm_perf(label: str, text: str, max_tokens: int | None = None) -> dict:
    s.begin_cache_request(LLM)
    s.reset_llama_timings(LLM)
    t0 = time.time()
    s.run_chat_completion(
        LLM,
        s.constrained_messages_for(text),
        max_tokens if max_tokens is not None else s.constrained_max_tokens(text),
        grammar=s.constrained_output_grammar(),
    )
    wall = round((time.time() - t0) * 1000)
    perf = s.llama_timings(LLM)
    pe_tok = perf.get("prompt_eval_tokens", 0)
    pe_ms = perf.get("prompt_eval_ms", 0)
    per_tok = round(pe_ms / pe_tok, 1) if pe_tok else 0
    ev = f"{perf.get('eval_ms')}ms/{perf.get('eval_tokens')}tok"
    print(f"  {label:26s} wall={wall}ms  pe={pe_ms}ms/{pe_tok}tok ({per_tok}/tok)  ev={ev}")
    return {"wall": wall, "pe_ms": pe_ms, "pe_tok": pe_tok, "per_tok": per_tok}


def asr_burst(mode: str = "burst"):
    """Run parakeet in a subprocess; block until it signals 'ASRBURST done'."""
    proc = subprocess.Popen(
        [ASR_PY, ASR_SCRIPT, mode],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    done_at = None
    elapsed = None
    for line in proc.stdout:  # type: ignore
        if "ASRBURST done" in line:
            done_at = time.time()
            parts = dict(p.split("=") for p in line.split() if "=" in p)
            elapsed = parts.get("elapsed")
    proc.wait()
    if done_at is None:
        print("  [asr] NO done signal; stderr:", proc.stderr.read()[:300])
    else:
        print(f"  [asr] {mode} burst elapsed={elapsed}ms (parent saw done at +{round((time.time()-done_at)*1000)}ms)")


def main() -> int:
    global LLM
    print("[load] llama.cpp LLM ...")
    os.environ["SUNOTO_LLM_POLISH_MODEL_PATH"] = os.path.join(
        ROOT, "models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf"
    )
    LLM = s.load_model()
    # warm cache: do a couple real completions so prefix is cached + prefill warm
    print("\n=== warmup (prime prompt cache + warm prefill) ===")
    llm_perf("warmup-1", next_unique())
    llm_perf("warmup-2", next_unique())

    # Experiment 2: ASR burst then immediate LLM (fresh unique)
    print("\n=== EXP2: one ASR generate burst, then immediate LLM polish (fresh) ===")
    llm_perf("pre-asr-warm", next_unique())
    asr_burst("burst")
    llm_perf("post-asr-cold?", next_unique())

    # Experiment 3 (VARIANT A): ASR burst -> re-warm ping -> real polish (FRESH)
    print("\n=== EXP3-VARIANT A: ASR burst -> 4-tok re-warm -> fresh polish ===")
    llm_perf("pre-asr-warm2", next_unique())
    asr_burst("burst")
    t0 = time.time()
    llm_perf("re-warm-ping(4tok)", "rewarm ping token " + str(_idx) + ".", max_tokens=4)
    print(f"  ^ re-warm itself cost wall={round((time.time()-t0)*1000)}ms")
    llm_perf("post-rewarm-fresh?", next_unique())

    # Experiment 4: sustained STREAMING ASR (real recording shape), then LLM
    print("\n=== EXP4: sustained parakeet STREAMING (~3s), then LLM ===")
    llm_perf("pre-stream-warm", next_unique())
    asr_burst("stream 5")
    llm_perf("post-stream-cold?", next_unique())

    # Experiment 4b: LONGER streaming (8 chunks ~4.5s) — does duration worsen cold?
    print("\n=== EXP4b: longer streaming (8 chunks ~4.5s), then LLM ===")
    llm_perf("pre-stream8-warm", next_unique())
    asr_burst("stream 8")
    llm_perf("post-stream8-cold?", next_unique())

    # Experiment 5: cold-fixed-ramp check — how many tokens before it warms?
    print("\n=== EXP5: ASR burst then BACK-TO-BACK fresh polishes (does 2nd warm up?) ===")
    llm_perf("pre5-warm", next_unique())
    asr_burst("burst")
    llm_perf("post5-first(cold)", next_unique())
    llm_perf("post5-second(warm?)", next_unique())
    llm_perf("post5-third(warm?)", next_unique())

    # Experiment 6: short delay after ASR (idle cooling叠加?)
    print("\n=== EXP6: ASR burst -> 300ms delay -> polish (idle cooling叠?) ===")
    llm_perf("pre6-warm", next_unique())
    asr_burst("burst")
    time.sleep(0.3)
    llm_perf("post6-300ms-delay", next_unique())

    # Experiment 7 (KEY FIX HYPOTHESIS): interleaved keepalive during ASR
    # Start a 1s-interval LLM keepalive thread, run a LONG streaming ASR,
    # then measure post-ASR polish. Does keeping the LLM firing during ASR
    # prevent the cold snap?
    print("\n=== EXP7: interleaved 1s LLM keepalive during 8-chunk ASR stream ===")
    import threading
    stop_flag = threading.Event()

    def ka_loop():
        i = 0
        while not stop_flag.is_set():
            i += 1
            try:
                s.begin_cache_request(LLM)
                s.reset_llama_timings(LLM)
                s.run_chat_completion(
                    LLM,
                    s.constrained_messages_for(f"keepalive {i}."),
                    4,
                    grammar=s.constrained_output_grammar(),
                )
            except Exception:
                pass
            time.sleep(0.2)

    llm_perf("pre7-warm", next_unique())
    t = threading.Thread(target=ka_loop, daemon=True)
    t.start()
    time.sleep(0.5)  # let keepalive get going
    asr_burst("stream 8")
    stop_flag.set()
    t.join(timeout=5)
    llm_perf("post7-interleaved?", next_unique())

    # Experiment 8: interleaved keepalive during stream + final generate burst
    # (models real finish(): streaming partials THEN a direct-final generate
    # burst right before polish). Does the final generate re-cold despite
    # keepalive having kept it warm during streaming?
    print("\n=== EXP8: keepalive during stream, then FINAL generate burst, then polish ===")
    stop_flag2 = threading.Event()

    def ka_loop2():
        i = 0
        while not stop_flag2.is_set():
            i += 1
            try:
                s.begin_cache_request(LLM)
                s.reset_llama_timings(LLM)
                s.run_chat_completion(
                    LLM,
                    s.constrained_messages_for(f"keepalive {i}."),
                    4,
                    grammar=s.constrained_output_grammar(),
                )
            except Exception:
                pass
            time.sleep(0.2)

    llm_perf("pre8-warm", next_unique())
    t2 = threading.Thread(target=ka_loop2, daemon=True)
    t2.start()
    time.sleep(0.5)
    asr_burst("stream 5")  # streaming partials (recording)
    asr_burst("burst")     # direct-final generate burst
    stop_flag2.set()
    t2.join(timeout=5)
    llm_perf("post8-stream+final?", next_unique())

    # Experiment 9: identical but with final_mode='streaming' semantics --
    # i.e. NO final generate burst (only streaming), keepalive interleaved.
    print("\n=== EXP9: keepalive during stream, NO final generate, then polish ===")
    stop_flag3 = threading.Event()

    def ka_loop3():
        i = 0
        while not stop_flag3.is_set():
            i += 1
            try:
                s.begin_cache_request(LLM)
                s.reset_llama_timings(LLM)
                s.run_chat_completion(
                    LLM,
                    s.constrained_messages_for(f"keepalive {i}."),
                    4,
                    grammar=s.constrained_output_grammar(),
                )
            except Exception:
                pass
            time.sleep(0.2)

    llm_perf("pre9-warm", next_unique())
    t3 = threading.Thread(target=ka_loop3, daemon=True)
    t3.start()
    time.sleep(0.5)
    asr_burst("stream 5")  # streaming only, no final generate
    stop_flag3.set()
    t3.join(timeout=5)
    llm_perf("post9-stream-only?", next_unique())

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

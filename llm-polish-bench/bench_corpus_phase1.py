#!/usr/bin/env python3
"""Phase 1 corpus-driven LLM polish latency baseline (live daemon).

Reads `corpus-live-20260630.jsonl` (10 scenarios across the difficulty axes:
clean short/long/dict-term, equal/unequal/chained rewordings, discourse
false-positive, sensitive tokens, fillers, ASR-tense-not-ours). For each
scenario, sends the spoken text through the LIVE daemon's `polish`
control-socket command N times and records:

  - total_latency_ms (the control command's wall-clock for the polish call)
  - llm.latency_ms  (sidecar-reported polish latency)
  - llm.diagnostics.llama_perf  (prompt_eval_ms / eval_ms / reused_tokens
                                 -- the Phase 1 instrumentation fix)
  - correctness vs `expected` (recorded, not hard-gated -- ASR-free path so
    deterministic+LLM output is deterministic per text; mismatches are
    *observations*, useful to detect scope creep e.g. grammar_not_ours)

This is the apples-to-apples "live LLM polish" measurement (text in,
latency + llama_perf out) that the 2026-06-30 live run was missing. It
isolates the LLM-invocation environment from ASR variance.

Gate (exit non-zero): per-scenario llm p50 <= scenario.gate_llm_p50_ms.
Correctness mismatches are *printed and recorded* but don't fail the gate
in Phase 1 -- e.g. grammar_not_ours is EXPECTED to mismatch pre-Phase-5,
and recording it proves the scope fix is needed.

Usage (macOS, with daemon already running via the bare binary and warmed):
    .venv-llm-polish-mac/bin/python llm-polish-bench/bench_corpus_phase1.py \
        --runs 5 --summary llm-polish-bench/out/phase1-baseline-20260630.json
"""
from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[1]
BINARY = REPO / "target" / "release" / "sunoto-daemon"
CORPUS = REPO / "llm-polish-bench" / "corpus-live-20260630.jsonl"
DEFAULT_SUMMARY = REPO / "llm-polish-bench" / "out" / "phase1-baseline-20260630.json"

WARMUP_GRACE_MS = 1500  # pause between cases so GPU/Metal has a moment to settle


def die(msg: str) -> None:
    print(f"[phase1] FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def load_corpus(path: Path) -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    with path.open() as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            cases.append(json.loads(line))
    return cases


def run_polish(text: str, timeout_s: float = 60.0) -> dict[str, Any]:
    """Send a `polish TEXT` control command to the running daemon."""
    started = time.perf_counter()
    proc = subprocess.run(
        [str(BINARY), "polish", text],
        capture_output=True,
        text=True,
        timeout=timeout_s,
    )
    wall_ms = round((time.perf_counter() - started) * 1000)
    if proc.returncode != 0:
        die(f"sunoto-daemon polish failed (rc={proc.returncode}): {proc.stderr.strip()}")
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        die(f"non-JSON polish response: {proc.stdout!r}: {error}")
    payload["_wall_ms"] = wall_ms
    return payload


def nan_to_none(value: float | None) -> float | None:
    if value is None:
        return None
    return None if value != value else value


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    k = (len(ordered) - 1) * pct
    lo = int(k)
    hi = min(lo + 1, len(ordered) - 1)
    frac = k - lo
    return ordered[lo] + (ordered[hi] - ordered[lo]) * frac


def collect_perf(perf: dict[str, Any] | None) -> dict[str, float | None]:
    if not isinstance(perf, dict):
        perf = {}
    keys = (
        "prompt_eval_ms",
        "prompt_eval_tokens",
        "eval_ms",
        "eval_tokens",
        "reused_tokens",
    )
    return {key: nan_to_none(_as_float(perf.get(key))) for key in keys}


def _as_float(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def evaluate_case(case: dict[str, Any], runs: list[dict[str, Any]]) -> dict[str, Any]:
    name = case["case"]
    expected = case.get("expected")
    observed_outputs = [run.get("output") for run in runs]
    correctness_ok: bool | str
    if expected is None:
        # sensitive_tokens: correctness = preserved raw OR explicitly rejected.
        validation_rejected = any(
            run.get("llm", {}).get("diagnostics", {}).get("validation_rejected")
            for run in runs
        )
        preserved = all(
            run.get("output") == run.get("llm", {}).get("input")
            or run.get("output") == case["spoken"]
            for run in runs
        )
        correctness_ok = "preserved-or-rejected" if (preserved or validation_rejected) else "REWORDING_DETECTED"
    else:
        correctness_ok = all(out == expected for out in observed_outputs)

    llm_latencies = [
        _as_float(run.get("llm", {}).get("latency_ms")) or 0.0 for run in runs
    ]
    wall_latencies = [float(run.get("_wall_ms", 0)) for run in runs]
    perfs = [collect_perf(run.get("llm", {}).get("diagnostics", {}).get("llama_perf")) for run in runs]

    def p50(values: list[float]) -> float:
        return percentile(values, 0.50)

    def p90(values: list[float]) -> float:
        return percentile(values, 0.90)

    return {
        "case": name,
        "axis": case.get("axis"),
        "gate_llm_p50_ms": case.get("gate_llm_p50_ms"),
        "runs": runs,
        "n": len(runs),
        "llm_latency_ms": {
            "p50": round(p50(llm_latencies), 1),
            "p90": round(p90(llm_latencies), 1),
            "min": round(min(llm_latencies), 1),
            "max": round(max(llm_latencies), 1),
            "all": [round(v, 1) for v in llm_latencies],
        },
        "wall_latency_ms": {
            "p50": round(p50(wall_latencies), 1),
            "p90": round(p90(wall_latencies), 1),
        },
        "llama_perf_p50": {
            key: round(p50([p[key] or 0.0 for p in perfs]), 1) for key in perfs[0]
        },
        "decision": runs[-1].get("llm", {}).get("diagnostics", {}).get("decision_label"),
        "validation_rejected": any(
            run.get("llm", {}).get("diagnostics", {}).get("validation_rejected")
            for run in runs
        ),
        "completion_tokens": runs[-1]
        .get("llm", {})
        .get("diagnostics", {})
        .get("completion_tokens"),
        "correctness_ok": correctness_ok,
        "outputs": observed_outputs,
        "expected": expected,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=5, help="iterations per scenario")
    parser.add_argument("--summary", type=Path, default=DEFAULT_SUMMARY)
    parser.add_argument(
        "--only", help="comma-separated case names to run (default: all)"
    )
    args = parser.parse_args()

    if not BINARY.exists():
        die(f"release binary not found: {BINARY}; run `cargo build --release`")
    corpus = load_corpus(CORPUS)
    if args.only:
        wanted = {name.strip() for name in args.only.split(",")}
        corpus = [c for c in corpus if c["case"] in wanted]
        if not corpus:
            die(f"--only matched no cases; known: {[c['case'] for c in load_corpus(CORPUS)]}")

    # Sanity: confirm the daemon is reachable and warmed (one throwaway call).
    probe = run_polish("phase one warmup probe")
    if not probe.get("ok"):
        die(f"daemon polish probe failed: {probe}")
    llm_block = probe.get("llm", {})
    if not llm_block.get("accepted"):
        die(
            "daemon not warmed yet (LLM polish not accepted); "
            "wait for the 'LLM polish post-ASR warmup complete' log line first"
        )

    results: list[dict[str, Any]] = []
    gate_failures: list[str] = []
    for case in corpus:
        name = case["case"]
        print(f"\n=== {name} (runs={args.runs}) ===", flush=True)
        print(f"    spoken: {case['spoken']}", flush=True)
        runs: list[dict[str, Any]] = []
        for i in range(args.runs):
            time.sleep(WARMUP_GRACE_MS / 1000.0)
            result = run_polish(case["spoken"])
            llm = result.get("llm", {})
            diag = llm.get("diagnostics", {})
            perf = diag.get("llama_perf") or {}
            print(
                f"    [{i+1}/{args.runs}] llm={llm.get('latency_ms')}ms "
                f"wall={result.get('_wall_ms')}ms "
                f"pe={perf.get('prompt_eval_ms')}ms/{perf.get('prompt_eval_tokens')}tok "
                f"ev={perf.get('eval_ms')}ms/{perf.get('eval_tokens')}tok "
                f"reused={perf.get('reused_tokens')} "
                f"-> {result.get('output')!r}",
                flush=True,
            )
            runs.append(result)
        evaluated = evaluate_case(case, runs)
        results.append(evaluated)
        gate = case.get("gate_llm_p50_ms")
        p50 = evaluated["llm_latency_ms"]["p50"]
        ok = isinstance(evaluated["correctness_ok"], bool) and evaluated["correctness_ok"]
        cstr = "OK" if ok else (
            f"SUPPRESSED({evaluated['correctness_ok']})"
            if isinstance(evaluated["correctness_ok"], str)
            else "MISMATCH"
        )
        pe = evaluated["llama_perf_p50"].get("prompt_eval_ms")
        ev = evaluated["llama_perf_p50"].get("eval_ms")
        ru = evaluated["llama_perf_p50"].get("reused_tokens")
        print(
            f"  -> p50 llm={p50}ms (gate {gate}ms) "
            f"perf: pe={pe}ms ev={ev}ms reused={ru} "
            f"correctness={cstr}",
            flush=True,
        )
        if gate is not None and p50 > gate:
            gate_failures.append(f"{name}: llm p50 {p50}ms > gate {gate}ms")

    overall_p50 = percentile(
        [float(r.get("llm", {}).get("latency_ms", 0)) for case in results for r in case["runs"]],
        0.50,
    )
    summary = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "runs_per_case": args.runs,
        "corpus": str(CORPUS.name),
        "overall_llm_p50_ms": round(overall_p50, 1),
        "cases": results,
    }
    args.summary.parent.mkdir(parents=True, exist_ok=True)
    args.summary.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"\n[phase1] summary written: {args.summary}")

    print("\n=== Phase 1 baseline table ===")
    print(f"{'case':<20} {'llm p50':>8} {'gate':>6} {'pe(ms)':>7} {'ev(ms)':>7} {'reused':>7} {'correct':>10}")
    for c in results:
        ok = c["correctness_ok"]
        cstr = "OK" if ok is True else ("~" if isinstance(ok, str) else "MISMATCH")
        print(
            f"{c['case']:<20} "
            f"{c['llm_latency_ms']['p50']:>8.1f} "
            f"{(c.get('gate_llm_p50_ms') or 0):>6} "
            f"{(c['llama_perf_p50'].get('prompt_eval_ms') or 0):>7.1f} "
            f"{(c['llama_perf_p50'].get('eval_ms') or 0):>7.1f} "
            f"{(c['llama_perf_p50'].get('reused_tokens') or 0):>7.1f} "
            f"{cstr:>10}"
        )
    print(f"{'overall':<20} {overall_p50:>8.1f}")

    if gate_failures:
        print("\n[phase1] LATENCY GATE FAILURES:", file=sys.stderr)
        for failure in gate_failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("\n[phase1] all latency gates passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

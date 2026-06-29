#!/usr/bin/env python3
"""Smoke-test NeuralWatt OpenAI-compatible models on the polish dataset."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import statistics
import time
import urllib.error
import urllib.request
from pathlib import Path

from bench_two_pass import dynamic_rewrite_tokens, validate_candidate

HERE = Path(__file__).resolve().parent
OUT_DIR = HERE / "out" / "neuralwatt-smoke"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)

PROMPT = (
    "Clean voice-dictation disfluencies only. Remove fillers, repeated words, "
    "false starts, and correction cues with replacements. Preserve the final "
    "meaning and every fact-like token. Do not convert spoken digits/dot/at "
    "to symbols. If unsure, return unchanged. Output only cleaned text."
)

FEW_SHOT = [
    (
        "Please open settings, no wait, open the dashboard.",
        "Please open the dashboard.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "Her email is janet dot smith at example dot com.",
    ),
]

DEFAULT_IDS = [
    "restart-01",
    "restart-03",
    "preserve-01",
    "preserve-03",
    "preserve-05",
    "preserve-07",
    "compound-02",
    "compound-05",
    "tricky-03",
    "tricky-05",
    "long-01",
    "long-03",
]


def messages(text: str) -> list[dict[str, str]]:
    out = [{"role": "system", "content": PROMPT}]
    for input_text, output_text in FEW_SHOT:
        out.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        out.append({"role": "assistant", "content": output_text})
    out.append({"role": "user", "content": f"Clean this transcript:\n{text}"})
    return out


def post_json(url: str, key: str, payload: dict) -> dict:
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace")
        raise RuntimeError(f"HTTP {e.code}: {body}") from e


def load_rows(path: Path, ids: list[str] | None) -> list[dict]:
    rows = bench.load_dataset(str(path))
    if not ids:
        return rows
    wanted = set(ids)
    selected = [row for row in rows if row["id"] in wanted]
    missing = wanted - {row["id"] for row in selected}
    if missing:
        raise SystemExit(f"unknown dataset ids: {', '.join(sorted(missing))}")
    return selected


def summarize(results: list[dict]) -> dict:
    latencies = [row["latency_ms"] for row in results]
    exact_n = sum(1 for row in results if row["exact"])
    return {
        "n": len(results),
        "exact_matches": exact_n,
        "exact_pct": round(100 * exact_n / len(results), 1),
        "mean_similarity": round(statistics.mean(row["similarity"] for row in results), 4),
        "mean_wer": round(statistics.mean(row["wer"] for row in results), 4),
        "mean_latency_ms": round(statistics.mean(latencies)),
        "p50_latency_ms": round(statistics.median(latencies)),
        "p95_latency_ms": round(sorted(latencies)[int(0.95 * (len(latencies) - 1))]),
        "unsafe_cases": sum(1 for row in results if row["validation_reasons"]),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="https://api.neuralwatt.com/v1")
    parser.add_argument("--model", default="kimi-k2.6")
    parser.add_argument("--dataset", default=str(HERE / "dataset.jsonl"))
    parser.add_argument("--ids", nargs="*", default=DEFAULT_IDS)
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--temperature", type=float, default=0.1)
    parser.add_argument("--top-p", type=float, default=0.95)
    parser.add_argument("--out-dir", default=str(OUT_DIR))
    args = parser.parse_args()

    key = os.environ.get("NEURALWATT_API_KEY")
    if not key:
        raise SystemExit("NEURALWATT_API_KEY is required")

    ids = None if args.all else args.ids
    rows = load_rows(Path(args.dataset), ids)
    endpoint = args.base_url.rstrip("/") + "/chat/completions"

    results = []
    for row in rows:
        payload = {
            "model": args.model,
            "messages": messages(row["input"]),
            "max_tokens": dynamic_rewrite_tokens(row["input"]),
            "temperature": args.temperature,
            "top_p": args.top_p,
        }
        start = time.time()
        response = post_json(endpoint, key, payload)
        latency_ms = round((time.time() - start) * 1000)
        raw = response["choices"][0]["message"].get("content") or ""
        output = bench.clean_output(raw, PROMPT)
        score = bench.score(output, row["expected"])
        validation_reasons = validate_candidate(row["input"], output)
        result = {
            "id": row["id"],
            "category": row["category"],
            "input": row["input"],
            "expected": row["expected"],
            "output": output,
            "raw_output": raw.strip(),
            "exact": score["exact"],
            "similarity": score["similarity"],
            "wer": score["wer"],
            "latency_ms": latency_ms,
            "validation_reasons": validation_reasons,
        }
        results.append(result)
        marker = "OK " if result["exact"] else ("~ " if result["similarity"] >= 0.9 else "X  ")
        unsafe = f" unsafe={','.join(validation_reasons)}" if validation_reasons else ""
        print(f"{marker}{row['id']:14s} sim={result['similarity']:.2f} {latency_ms:5d}ms{unsafe}")

    summary = summarize(results)
    artifact = {
        "model": args.model,
        "base_url": args.base_url,
        "system_prompt": PROMPT,
        "few_shot": FEW_SHOT,
        "summary": summary,
        "results": results,
    }
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{args.model.replace('/', '_')}.json"
    out_path.write_text(json.dumps(artifact, indent=2))
    print(
        f"SUMMARY exact={summary['exact_pct']} sim={summary['mean_similarity']} "
        f"p50={summary['p50_latency_ms']}ms p95={summary['p95_latency_ms']}ms "
        f"unsafe={summary['unsafe_cases']}"
    )
    print(f"wrote {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

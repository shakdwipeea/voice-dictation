#!/usr/bin/env python3
"""Benchmark Hugging Face MLX models on the polish dataset."""

from __future__ import annotations

import argparse
import importlib.util
import json
import statistics
import time
from pathlib import Path

import mlx.core as mx
from mlx_lm import generate, load
from mlx_lm.sample_utils import make_repetition_penalty, make_sampler

HERE = Path(__file__).resolve().parent
OUT_DIR = HERE / "out" / "macos-mlx"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


MODELS = {
    "lfm2-2.6b-mlx4": {
        "repo": "mlx-community/LFM2-2.6B-4bit",
        "label": "LFM2-2.6B (MLX 4bit)",
        "family": "Liquid",
        "size_mib": 1500,
    },
    "qwen3.5-4b-mlx4": {
        "repo": "mlx-community/Qwen3.5-4B-MLX-4bit",
        "label": "Qwen3.5-4B (MLX 4bit)",
        "family": "Qwen",
        "size_mib": 2900,
    },
    "gemma-4-e2b-it-mlx4": {
        "repo": "mlx-community/gemma-4-e2b-it-4bit",
        "label": "Gemma 4 E2B-it (MLX 4bit)",
        "family": "Gemma",
        "size_mib": 4300,
    },
}


def render_prompt(tokenizer, messages: list[dict]) -> str:
    prompt = tokenizer.apply_chat_template(
        messages,
        add_generation_prompt=True,
        tokenize=False,
    )
    return prompt


def run_model(model_key: str, rows: list[dict], args: argparse.Namespace) -> dict:
    info = MODELS[model_key]
    print(f"\n=== {info['label']} ===", flush=True)
    t0 = time.time()
    model, tokenizer = load(info["repo"])
    mx.eval(model.parameters())
    load_ms = round((time.time() - t0) * 1000)
    print(f"  loaded in {load_ms} ms", flush=True)

    sampler = make_sampler(
        temp=args.temperature,
        top_p=args.top_p,
        top_k=args.top_k,
        min_p=args.min_p,
    )
    logits_processors = []
    if args.repeat_penalty != 1.0:
        logits_processors.append(make_repetition_penalty(args.repeat_penalty))

    results = []
    exact_n = 0
    sims = []
    wers = []
    lat_ms = []
    chat_fails = 0

    for row in rows:
        messages = bench.build_messages(model_key, row["input"])
        prompt = render_prompt(tokenizer, messages)
        t1 = time.time()
        try:
            raw = generate(
                model,
                tokenizer,
                prompt=prompt,
                max_tokens=args.max_tokens,
                sampler=sampler,
                logits_processors=logits_processors,
                verbose=False,
            )
        except Exception as error:
            raw = ""
            print(f"    [{row['id']}] ERROR {error}", flush=True)
        ms = round((time.time() - t1) * 1000)
        output = bench.clean_output(raw, bench.SYSTEM_PROMPT)
        score = bench.score(output, row["expected"])
        if bench.is_refusal_or_chatty(output):
            chat_fails += 1
        if score["exact"]:
            exact_n += 1
        sims.append(score["similarity"])
        wers.append(score["wer"])
        lat_ms.append(ms)
        results.append(
            {
                "id": row["id"],
                "category": row["category"],
                "input": row["input"],
                "expected": row["expected"],
                "output": output,
                "raw_output": raw.strip(),
                "exact": score["exact"],
                "similarity": score["similarity"],
                "wer": score["wer"],
                "latency_ms": ms,
            }
        )
        marker = "OK " if score["exact"] else ("~ " if score["similarity"] >= 0.9 else "X  ")
        print(f"    {marker}{row['id']:14s} sim={score['similarity']:.2f} {ms:4d}ms", flush=True)

    del model
    mx.clear_cache()
    return {
        "model": model_key,
        "label": info["label"],
        "family": info["family"],
        "repo": info["repo"],
        "size_mib": info["size_mib"],
        "load_ms": load_ms,
        "n": len(rows),
        "exact_matches": exact_n,
        "exact_pct": round(100 * exact_n / len(rows), 1),
        "mean_similarity": round(statistics.mean(sims), 4),
        "mean_wer": round(statistics.mean(wers), 4),
        "mean_latency_ms": round(statistics.mean(lat_ms)),
        "p50_latency_ms": round(statistics.median(lat_ms)),
        "p95_latency_ms": round(sorted(lat_ms)[int(0.95 * (len(lat_ms) - 1))]),
        "chat_fails": chat_fails,
        "results": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", nargs="*", default=list(MODELS))
    parser.add_argument("--max-tokens", type=int, default=128)
    parser.add_argument("--temperature", type=float, default=bench.INFER["temperature"])
    parser.add_argument("--top-k", type=int, default=bench.INFER["top_k"])
    parser.add_argument("--top-p", type=float, default=bench.INFER["top_p"])
    parser.add_argument("--min-p", type=float, default=0.0)
    parser.add_argument("--repeat-penalty", type=float, default=bench.INFER["repeat_penalty"])
    parser.add_argument("--out-dir", default=str(OUT_DIR))
    args = parser.parse_args()

    rows = bench.load_dataset(str(HERE / "dataset.jsonl"))
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"loaded {len(rows)} examples")

    all_results = []
    for model_key in args.models:
        if model_key not in MODELS:
            print(f"skipping unknown model {model_key}", flush=True)
            continue
        result = run_model(model_key, rows, args)
        all_results.append(result)
        with (out_dir / f"{model_key}.json").open("w") as handle:
            json.dump(result, handle, indent=2)
        print(
            f"  -> {model_key}: exact {result['exact_pct']}%, "
            f"sim {result['mean_similarity']}, p50 {result['p50_latency_ms']}ms",
            flush=True,
        )

    with (out_dir / "all.json").open("w") as handle:
        json.dump(all_results, handle, indent=2)
    print(f"\nDONE -> {out_dir / 'all.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

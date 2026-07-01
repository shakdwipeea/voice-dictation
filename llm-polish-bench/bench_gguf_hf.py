#!/usr/bin/env python3
"""Benchmark Hugging Face GGUF models on the polish dataset.

This is a small companion to bench.py for model bake-offs where the GGUFs live
on Hugging Face. It reuses the current prompt and scoring logic from bench.py,
downloads the requested model files into the gitignored models/ tree, and emits
one JSON result per model plus a combined all.json.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import statistics
import time
from pathlib import Path

from huggingface_hub import hf_hub_download
from llama_cpp import Llama

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
MODEL_DIR = ROOT / "models" / "llm-polish-hf"
OUT_DIR = HERE / "out" / "macos-gguf"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


MODELS = {
    "qwen3.5-4b-q4": {
        "repo": "unsloth/Qwen3.5-4B-GGUF",
        "filename": "Qwen3.5-4B-Q4_K_M.gguf",
        "label": "Qwen3.5-4B (Q4_K_M, Unsloth GGUF)",
        "family": "Qwen",
        "size_mib": 2614,
    },
    "gemma-4-e2b-it-q4": {
        "repo": "bartowski/google_gemma-4-E2B-it-GGUF",
        "filename": "google_gemma-4-E2B-it-Q4_K_M.gguf",
        "label": "Gemma 4 E2B-it (Q4_K_M, bartowski GGUF)",
        "family": "Gemma",
        "size_mib": 3302,
    },
    "lfm2-2.6b-q4": {
        "repo": "LiquidAI/LFM2-2.6B-GGUF",
        "filename": "LFM2-2.6B-Q4_K_M.gguf",
        "label": "LFM2-2.6B (Q4_K_M, LiquidAI GGUF)",
        "family": "Liquid",
        "size_mib": 1491,
    },
}


def download_model(model_key: str) -> str:
    info = MODELS[model_key]
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    return hf_hub_download(
        repo_id=info["repo"],
        filename=info["filename"],
        local_dir=MODEL_DIR / model_key,
        local_dir_use_symlinks=False,
    )


def run_model(model_key: str, rows: list[dict], args: argparse.Namespace) -> dict:
    info = MODELS[model_key]
    print(f"\n=== {info['label']} ===", flush=True)
    path = download_model(model_key)
    print(f"  model file: {path}", flush=True)

    t0 = time.time()
    llm = Llama(
        model_path=path,
        n_gpu_layers=args.gpu_layers,
        n_ctx=args.ctx,
        n_batch=args.batch,
        n_ubatch=args.ubatch,
        n_threads=args.threads,
        flash_attn=args.flash_attn,
        verbose=args.verbose,
        logits_all=False,
        seed=args.seed,
    )
    load_ms = round((time.time() - t0) * 1000)
    print(f"  loaded in {load_ms} ms", flush=True)

    results = []
    exact_n = 0
    sims = []
    wers = []
    lat_ms = []
    chat_fails = 0

    for row in rows:
        messages = bench.build_messages(model_key, row["input"])
        t1 = time.time()
        try:
            response = llm.create_chat_completion(
                messages=messages,
                max_tokens=args.max_tokens,
                temperature=args.temperature,
                top_k=args.top_k,
                top_p=args.top_p,
                repeat_penalty=args.repeat_penalty,
            )
            raw = response["choices"][0]["message"]["content"] or ""
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

    del llm
    return {
        "model": model_key,
        "label": info["label"],
        "family": info["family"],
        "repo": info["repo"],
        "filename": info["filename"],
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
    parser.add_argument("--ctx", type=int, default=2048)
    parser.add_argument("--max-tokens", type=int, default=128)
    parser.add_argument("--gpu-layers", type=int, default=-1)
    parser.add_argument("--batch", type=int, default=512)
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--flash-attn", action="store_true")
    parser.add_argument("--temperature", type=float, default=bench.INFER["temperature"])
    parser.add_argument("--top-k", type=int, default=bench.INFER["top_k"])
    parser.add_argument("--top-p", type=float, default=bench.INFER["top_p"])
    parser.add_argument("--repeat-penalty", type=float, default=bench.INFER["repeat_penalty"])
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--out-dir", default=str(OUT_DIR))
    parser.add_argument("--verbose", action="store_true")
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

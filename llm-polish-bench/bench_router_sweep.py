#!/usr/bin/env python3
"""Sweep model-based CLEAN/POLISH router prompts."""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import statistics
import time
from pathlib import Path

from llama_cpp import Llama

from bench_gguf_hf import MODELS, download_model

HERE = Path(__file__).resolve().parent
OUT_DIR = HERE / "out" / "macos-gemma-router-sweep"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


ROUTER_PROMPTS = {
    "current": (
        "You classify voice-dictation transcripts. "
        "Answer exactly CLEAN if the text should be left unchanged. "
        "Answer exactly POLISH if it contains speech disfluency, false starts, "
        "self-correction, or filler that should be cleaned. "
        "Do not explain."
    ),
    "change_only": (
        "Decide whether cleanup should change this voice transcript. "
        "Answer exactly POLISH only when removing speech artifacts should clearly "
        "change the text. Answer exactly CLEAN when it is already acceptable, "
        "when words like no/wait/actually/sorry are meaningful, or when unsure. "
        "No explanation."
    ),
    "strict_polish": (
        "Route this transcript. Output POLISH only for obvious removable filler, "
        "stutter, repeated words, false starts, or explicit self-corrections. "
        "Output CLEAN for meaningful wording, facts, numbers, code, emails, URLs, "
        "names, commands, or any uncertain case. Output exactly CLEAN or POLISH."
    ),
    "safe_skip": (
        "Should a second LLM rewrite be run? Output POLISH only if a rewrite is "
        "clearly needed to remove dictation disfluency. Output CLEAN if the text "
        "can be inserted as-is, is fact-heavy, or the decision is ambiguous. "
        "Output exactly CLEAN or POLISH."
    ),
    "aggressive_skip": (
        "You are an expensive-rewrite gate. Prefer CLEAN. Output POLISH only if "
        "the transcript has clear removable speech noise such as um/uh filler, "
        "duplicated words, an abandoned start, or a correction cue with replacement. "
        "Otherwise output CLEAN. Output exactly CLEAN or POLISH."
    ),
}


def classifier_truth(row: dict) -> str:
    return "POLISH" if bench.norm(row["input"]) != bench.norm(row["expected"]) else "CLEAN"


def parse_classifier_output(raw: str) -> tuple[str, bool]:
    cleaned = bench.clean_output(raw, "").strip().upper()
    cleaned = re.sub(r"[^A-Z]", " ", cleaned)
    tokens = cleaned.split()
    if tokens and tokens[0] == "CLEAN":
        return "CLEAN", True
    if tokens and tokens[0] == "POLISH":
        return "POLISH", True
    return "POLISH", False


def build_messages(prompt_key: str, text: str) -> list[dict]:
    return [
        {"role": "system", "content": ROUTER_PROMPTS[prompt_key]},
        {"role": "user", "content": f"Transcript:\n{text}"},
    ]


def run_router(llm: Llama, rows: list[dict], prompt_key: str, args: argparse.Namespace) -> dict:
    print(f"\n=== router: {prompt_key} ===", flush=True)
    results = []
    for row in rows:
        start = time.time()
        response = llm.create_chat_completion(
            messages=build_messages(prompt_key, row["input"]),
            max_tokens=args.max_tokens,
            temperature=0.0,
            top_k=0,
            top_p=1.0,
            repeat_penalty=1.0,
        )
        latency_ms = round((time.time() - start) * 1000)
        raw = response["choices"][0]["message"]["content"] or ""
        decision, valid = parse_classifier_output(raw)
        truth = classifier_truth(row)
        results.append(
            {
                "id": row["id"],
                "category": row["category"],
                "input": row["input"],
                "expected": row["expected"],
                "truth": truth,
                "raw_output": raw.strip(),
                "decision": decision,
                "valid": valid,
                "latency_ms": latency_ms,
            }
        )
        marker = "OK " if decision == truth else "X  "
        print(f"    {marker}{row['id']:14s} truth={truth:<6} decision={decision:<6} {latency_ms:4d}ms", flush=True)
    summary = summarize(results)
    print(
        f"  -> {prompt_key}: rewrite {summary['rewrite_call_rate']:.0%}, "
        f"precision {summary['precision']}, recall {summary['recall']}, "
        f"false_clean {summary['false_clean']}, p50 {summary['p50_latency_ms']}ms",
        flush=True,
    )
    return {
        "router": prompt_key,
        "prompt": ROUTER_PROMPTS[prompt_key],
        "summary": summary,
        "results": results,
    }


def summarize(results: list[dict]) -> dict:
    latencies = [row["latency_ms"] for row in results]
    tp = sum(1 for row in results if row["truth"] == "POLISH" and row["decision"] == "POLISH")
    fp = sum(1 for row in results if row["truth"] == "CLEAN" and row["decision"] == "POLISH")
    fn = sum(1 for row in results if row["truth"] == "POLISH" and row["decision"] == "CLEAN")
    tn = sum(1 for row in results if row["truth"] == "CLEAN" and row["decision"] == "CLEAN")
    predicted_polish = tp + fp
    truth_polish = tp + fn
    precision = tp / predicted_polish if predicted_polish else 0.0
    recall = tp / truth_polish if truth_polish else 0.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {
        "n": len(results),
        "rewrite_calls": predicted_polish,
        "rewrite_call_rate": round(predicted_polish / len(results), 4),
        "skipped": tn + fn,
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "f1": round(f1, 4),
        "false_clean": fn,
        "false_polish": fp,
        "true_polish": tp,
        "true_clean": tn,
        "invalid": sum(1 for row in results if not row["valid"]),
        "mean_latency_ms": round(statistics.mean(latencies)),
        "p50_latency_ms": round(statistics.median(latencies)),
        "p95_latency_ms": round(sorted(latencies)[int(0.95 * (len(latencies) - 1))]),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="gemma-4-e2b-it-q4", choices=sorted(MODELS))
    parser.add_argument("--routers", nargs="*", default=list(ROUTER_PROMPTS))
    parser.add_argument("--ctx", type=int, default=2048)
    parser.add_argument("--gpu-layers", type=int, default=-1)
    parser.add_argument("--batch", type=int, default=512)
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--flash-attn", action="store_true")
    parser.add_argument("--max-tokens", type=int, default=4)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--out-dir", default=str(OUT_DIR))
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    rows = bench.load_dataset(str(HERE / "dataset.jsonl"))
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    info = MODELS[args.model]
    path = download_model(args.model)
    print(f"loaded {len(rows)} examples")
    print(f"model file: {path}")

    load_start = time.time()
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
    load_ms = round((time.time() - load_start) * 1000)
    print(f"loaded model in {load_ms} ms", flush=True)

    routers = []
    for router in args.routers:
        if router not in ROUTER_PROMPTS:
            print(f"skipping unknown router {router}", flush=True)
            continue
        result = run_router(llm, rows, router, args)
        routers.append(result)
        with (out_dir / f"{router}.json").open("w") as handle:
            json.dump(result, handle, indent=2)

    payload = {
        "model": args.model,
        "label": info["label"],
        "family": info["family"],
        "repo": info["repo"],
        "filename": info["filename"],
        "runtime": "llama-cpp-python GGUF/Metal",
        "flash_attn": args.flash_attn,
        "load_ms": load_ms,
        "routers": routers,
    }
    with (out_dir / "all.json").open("w") as handle:
        json.dump(payload, handle, indent=2)
    print(f"\nDONE -> {out_dir / 'all.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

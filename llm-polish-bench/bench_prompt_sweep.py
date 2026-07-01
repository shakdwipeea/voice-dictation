#!/usr/bin/env python3
"""Sweep compact rewrite prompts for the Gemma polish candidate."""

from __future__ import annotations

import argparse
import importlib.util
import json
import statistics
import time
from pathlib import Path

from llama_cpp import Llama

from bench_gguf_hf import MODELS, download_model
from bench_two_pass import dynamic_rewrite_tokens, validate_candidate

HERE = Path(__file__).resolve().parent
OUT_DIR = HERE / "out" / "macos-gemma-prompt-sweep"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


PROMPTS = {
    "v2_with_fewshot": {
        "label": "Current v2 prompt + four few-shot examples",
        "system": bench.SYSTEM_PROMPT,
        "few_shot": bench.FEW_SHOT,
    },
    "v2_no_fewshot": {
        "label": "Current v2 prompt, no few-shot examples",
        "system": bench.SYSTEM_PROMPT,
        "few_shot": [],
    },
    "compact_guarded": {
        "label": "Compact guarded rewrite prompt",
        "system": (
            "Return a cleaned voice-dictation transcript. Remove only speech artifacts: "
            "fillers, stutters, repeated words, abandoned starts, and correction cues "
            "when the speaker gives a replacement. Keep the final intended wording. "
            "Preserve names, facts, numbers, spoken digit words, code, emails, URLs, "
            "serials, and literal words like no, wait, actually, and sorry when they "
            "are the message. Do not turn spoken digits, dot, or at into symbols. "
            "If unsure, copy the transcript unchanged. Output only the transcript."
        ),
        "few_shot": [],
    },
    "compact_marker_rules": {
        "label": "Compact prompt with explicit correction marker rules",
        "system": (
            "Clean only disfluencies from voice dictation. If a cue such as no wait, "
            "wait no, actually, I mean, scratch that, strike that, or let me start "
            "over introduces a correction, drop the discarded wording and keep the "
            "replacement. Remove um, uh, er, hmm, repeated words, and false starts. "
            "Never add meaning or reformat facts. Keep numbers as spoken words; keep "
            "emails, URLs, IPs, code, serials, names, and negation words unchanged "
            "unless they are part of a discarded correction. Output only cleaned text."
        ),
        "few_shot": [],
    },
    "compact_conservative": {
        "label": "Compact conservative prompt",
        "system": (
            "You are a conservative transcript cleaner. Delete only obvious filler, "
            "stutter, repetition, false-start, or correction-marker text. Do not "
            "summarize, rewrite style, answer, translate, format, punctuate into new "
            "forms, or convert spoken words to symbols. Preserve every fact token, "
            "number word, name, email, URL, IP, code token, serial, and negation. "
            "If a cleanup is not obvious, return the input unchanged. Output only text."
        ),
        "few_shot": [],
    },
    "compact_one_shot": {
        "label": "Compact prompt with one preservation example",
        "system": (
            "Clean voice-dictation disfluencies only. Remove fillers, repeated words, "
            "false starts, and correction cues with replacements. Preserve the final "
            "meaning and every fact-like token. Do not convert spoken digits/dot/at "
            "to symbols. If unsure, return unchanged. Output only cleaned text."
        ),
        "few_shot": [
            (
                "Her email is jane, no, janet dot smith at example dot com.",
                "Her email is janet dot smith at example dot com.",
            )
        ],
    },
    "compact_marker_one_shot": {
        "label": "Marker-rule prompt with one preservation example",
        "system": (
            "Clean only disfluencies from voice dictation. If a cue such as no wait, "
            "wait no, actually, I mean, scratch that, strike that, or let me start "
            "over introduces a correction, drop the discarded wording and keep the "
            "replacement. Remove um, uh, er, hmm, repeated words, and false starts. "
            "Never add meaning or reformat facts. Keep numbers as spoken words; keep "
            "emails, URLs, IPs, code, serials, names, and negation words unchanged "
            "unless they are part of a discarded correction. Output only cleaned text."
        ),
        "few_shot": [
            (
                "Her email is jane, no, janet dot smith at example dot com.",
                "Her email is janet dot smith at example dot com.",
            )
        ],
    },
    "compact_balanced_two_shot": {
        "label": "Compact prompt with one correction and one preservation example",
        "system": (
            "Clean voice-dictation disfluencies only. Remove fillers, repeated words, "
            "false starts, and correction cues with replacements. Preserve the final "
            "meaning and every fact-like token. Do not convert spoken digits/dot/at "
            "to symbols. If unsure, return unchanged. Output only cleaned text."
        ),
        "few_shot": [
            (
                "Please open settings, no wait, open the dashboard.",
                "Please open the dashboard.",
            ),
            (
                "Her email is jane, no, janet dot smith at example dot com.",
                "Her email is janet dot smith at example dot com.",
            ),
        ],
    },
    "v2_preserve_one_shot": {
        "label": "Current v2 system prompt with one preservation example",
        "system": bench.SYSTEM_PROMPT,
        "few_shot": [
            (
                "Her email is jane, no, janet dot smith at example dot com.",
                "Her email is janet dot smith at example dot com.",
            )
        ],
    },
    "v2_balanced_two_shot": {
        "label": "Current v2 system prompt with one correction and one preservation example",
        "system": bench.SYSTEM_PROMPT,
        "few_shot": [
            (
                "Please open settings, no wait, open the dashboard.",
                "Please open the dashboard.",
            ),
            (
                "Her email is jane, no, janet dot smith at example dot com.",
                "Her email is janet dot smith at example dot com.",
            ),
        ],
    },
}


def build_messages(prompt_key: str, text: str) -> list[dict]:
    prompt = PROMPTS[prompt_key]
    messages = [{"role": "system", "content": prompt["system"]}]
    for input_text, output_text in prompt["few_shot"]:
        messages.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{text}"})
    return messages


def run_prompt(llm: Llama, rows: list[dict], prompt_key: str, args: argparse.Namespace) -> dict:
    print(f"\n=== prompt: {prompt_key} ===", flush=True)
    results = []
    for row in rows:
        start = time.time()
        response = llm.create_chat_completion(
            messages=build_messages(prompt_key, row["input"]),
            max_tokens=dynamic_rewrite_tokens(row["input"]),
            temperature=args.temperature,
            top_k=args.top_k,
            top_p=args.top_p,
            repeat_penalty=args.repeat_penalty,
        )
        latency_ms = round((time.time() - start) * 1000)
        raw = response["choices"][0]["message"]["content"] or ""
        output = bench.clean_output(raw, PROMPTS[prompt_key]["system"])
        score = bench.score(output, row["expected"])
        validation_reasons = validate_candidate(row["input"], output)
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
                "latency_ms": latency_ms,
                "validation_reasons": validation_reasons,
            }
        )
        marker = "OK " if score["exact"] else ("~ " if score["similarity"] >= 0.9 else "X  ")
        unsafe = f" unsafe={','.join(validation_reasons)}" if validation_reasons else ""
        print(
            f"    {marker}{row['id']:14s} sim={score['similarity']:.2f} "
            f"{latency_ms:4d}ms{unsafe}",
            flush=True,
        )
    summary = summarize(results)
    print(
        f"  -> {prompt_key}: exact {summary['exact_pct']}%, sim {summary['mean_similarity']}, "
        f"p50 {summary['p50_latency_ms']}ms, unsafe {summary['unsafe_cases']}",
        flush=True,
    )
    return {
        "prompt": prompt_key,
        "label": PROMPTS[prompt_key]["label"],
        "system_prompt": PROMPTS[prompt_key]["system"],
        "few_shot_count": len(PROMPTS[prompt_key]["few_shot"]),
        "summary": summary,
        "results": results,
    }


def summarize(results: list[dict]) -> dict:
    latencies = [row["latency_ms"] for row in results]
    reason_counts: dict[str, int] = {}
    for row in results:
        for reason in row["validation_reasons"]:
            reason_counts[reason] = reason_counts.get(reason, 0) + 1
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
        "validation_reasons": reason_counts,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="gemma-4-e2b-it-q4", choices=sorted(MODELS))
    parser.add_argument("--prompts", nargs="*", default=list(PROMPTS))
    parser.add_argument("--ctx", type=int, default=2048)
    parser.add_argument("--gpu-layers", type=int, default=-1)
    parser.add_argument("--batch", type=int, default=512)
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--flash-attn", action="store_true")
    parser.add_argument("--temperature", type=float, default=0.1)
    parser.add_argument("--top-k", type=int, default=50)
    parser.add_argument("--top-p", type=float, default=0.95)
    parser.add_argument("--repeat-penalty", type=float, default=1.05)
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

    prompt_results = []
    for prompt_key in args.prompts:
        if prompt_key not in PROMPTS:
            print(f"skipping unknown prompt {prompt_key}", flush=True)
            continue
        result = run_prompt(llm, rows, prompt_key, args)
        prompt_results.append(result)
        with (out_dir / f"{prompt_key}.json").open("w") as handle:
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
        "prompts": prompt_results,
    }
    with (out_dir / "all.json").open("w") as handle:
        json.dump(payload, handle, indent=2)
    print(f"\nDONE -> {out_dir / 'all.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

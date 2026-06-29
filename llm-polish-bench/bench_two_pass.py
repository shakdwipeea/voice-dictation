#!/usr/bin/env python3
"""Benchmark a two-pass LLM polish flow.

The experiment keeps routing model-based: a tiny classifier pass decides
whether cleanup is needed, and a rewrite pass runs only for POLISH cases.
Safety validation is post-rewrite only; it decides whether to accept the
candidate or fall back to the input.
"""

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
OUT_DIR = HERE / "out" / "macos-two-pass-gemma-balanced"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)

CLASSIFIER_SYSTEM_PROMPT = (
    "You classify voice-dictation transcripts. "
    "Answer exactly CLEAN if the text should be left unchanged. "
    "Answer exactly POLISH if it contains speech disfluency, false starts, "
    "self-correction, or filler that should be cleaned. "
    "Do not explain."
)

REWRITE_SYSTEM_PROMPT = (
    "Clean voice-dictation disfluencies only. Remove fillers, repeated words, "
    "false starts, and correction cues with replacements. Preserve the final "
    "meaning and every fact-like token. Do not convert spoken digits/dot/at "
    "to symbols. If unsure, return unchanged. Output only cleaned text."
)

REWRITE_FEW_SHOT = (
    (
        "Please open settings, no wait, open the dashboard.",
        "Please open the dashboard.",
    ),
    (
        "Her email is jane, no, janet dot smith at example dot com.",
        "Her email is janet dot smith at example dot com.",
    ),
)

EXPLICIT_CORRECTION_MARKERS = (
    "actually",
    "i mean",
    "no wait",
    "wait no",
    "scratch that",
    "strike that",
)

NEGATIONS = ("no", "not", "never", "without")
DIGIT_WORDS = {
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
}


def word_tokens(text: str) -> list[str]:
    return re.findall(r"[A-Za-z0-9]+(?:[-'][A-Za-z0-9]+)?", text.lower())


def contains_explicit_correction(text: str) -> bool:
    haystack = " ".join(word_tokens(text))
    return any(marker in haystack for marker in EXPLICIT_CORRECTION_MARKERS)


def contains_plain_no_correction(text: str) -> bool:
    return bool(re.search(r"\b[A-Za-z0-9][^.!?]*,\s*no,\s*(?!that\b|this\b|what\b)", text, re.I))


def contains_no_correction(text: str) -> bool:
    haystack = " ".join(word_tokens(text))
    return "no wait" in haystack or "wait no" in haystack or contains_plain_no_correction(text)


def has_spoken_digits(text: str) -> bool:
    return any(token in DIGIT_WORDS for token in word_tokens(text))


def introduces_ascii_digit_compaction(raw: str, output: str) -> bool:
    if not has_spoken_digits(raw):
        return False
    raw_has_digits = bool(re.search(r"\d", raw))
    output_has_digits = bool(re.search(r"\d", output))
    return output_has_digits and not raw_has_digits


def introduces_formatted_target(raw: str, output: str) -> bool:
    checks = (
        r"\b\S+@\S+\.\S+\b",
        r"https?://\S+|www\.\S+",
        r"\b\d{1,3}(?:\.\d{1,3}){3}\b",
    )
    return any(re.search(pattern, output) and not re.search(pattern, raw) for pattern in checks)


def introduces_spoken_marker_formatting(raw: str, output: str) -> bool:
    raw_tokens = set(word_tokens(raw))
    if "at" in raw_tokens and "@" in output and "@" not in raw:
        return True
    if "dot" in raw_tokens:
        raw_dotted = set(re.findall(r"\b[\w-]+\.[\w.-]+\b", raw))
        output_dotted = set(re.findall(r"\b[\w-]+\.[\w.-]+\b", output))
        if output_dotted - raw_dotted:
            return True
    return False


def introduces_code_formatting(raw: str, output: str) -> bool:
    if "`" in output and "`" not in raw:
        return True
    if '"' in output and '"' not in raw and re.search(r"\bprint\b|\bdef\b|\bclass\b", raw, re.I):
        return True
    return False


def word_count(text: str, word: str) -> int:
    return sum(1 for token in word_tokens(text) if token == word)


def drops_negation_unsafely(raw: str, output: str) -> bool:
    for word in ("not", "never", "without"):
        if word_count(output, word) < word_count(raw, word):
            return True
    if word_count(output, "no") >= word_count(raw, "no"):
        return False
    if contains_no_correction(raw):
        return False
    return True


def drops_correction_no(raw: str, output: str) -> bool:
    return (
        word_count(output, "no") < word_count(raw, "no")
        and not drops_negation_unsafely(raw, output)
        and contains_no_correction(raw)
    )


def validate_candidate_detailed(raw: str, output: str) -> dict[str, list[str]]:
    hard_unsafe = []
    review_flags = []
    if not output.strip():
        hard_unsafe.append("empty_output")
    if len(output) > int(len(raw) * 1.25) + 20:
        review_flags.append("too_long")
    if introduces_ascii_digit_compaction(raw, output):
        hard_unsafe.append("digit_compaction")
    if introduces_formatted_target(raw, output):
        hard_unsafe.append("formatted_target")
    if introduces_spoken_marker_formatting(raw, output):
        hard_unsafe.append("spoken_marker_formatting")
    if introduces_code_formatting(raw, output):
        hard_unsafe.append("code_formatting")
    if drops_negation_unsafely(raw, output):
        hard_unsafe.append("negation_dropped")
    elif drops_correction_no(raw, output):
        review_flags.append("correction_no_dropped")
    return {
        "hard_unsafe": hard_unsafe,
        "review_flags": review_flags,
    }


def validate_candidate(raw: str, output: str) -> list[str]:
    return validate_candidate_detailed(raw, output)["hard_unsafe"]


def classifier_truth(row: dict) -> str:
    return "POLISH" if bench.norm(row["input"]) != bench.norm(row["expected"]) else "CLEAN"


def build_classifier_messages(text: str) -> list[dict]:
    return [
        {"role": "system", "content": CLASSIFIER_SYSTEM_PROMPT},
        {"role": "user", "content": f"Transcript:\n{text}"},
    ]


def build_rewrite_messages(text: str) -> list[dict]:
    messages = [{"role": "system", "content": REWRITE_SYSTEM_PROMPT}]
    for input_text, output_text in REWRITE_FEW_SHOT:
        messages.append({"role": "user", "content": f"Clean this transcript:\n{input_text}"})
        messages.append({"role": "assistant", "content": output_text})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{text}"})
    return messages


def parse_classifier_output(raw: str) -> tuple[str, bool]:
    cleaned = bench.clean_output(raw, CLASSIFIER_SYSTEM_PROMPT).strip().upper()
    cleaned = re.sub(r"[^A-Z]", " ", cleaned)
    tokens = cleaned.split()
    if tokens and tokens[0] == "CLEAN":
        return "CLEAN", True
    if tokens and tokens[0] == "POLISH":
        return "POLISH", True
    return "POLISH", False


def dynamic_rewrite_tokens(text: str) -> int:
    return min(96, max(24, 2 * len(word_tokens(text)) + 16))


def chat(
    llm: Llama,
    messages: list[dict],
    max_tokens: int,
    temperature: float,
    top_k: int,
    top_p: float,
    repeat_penalty: float,
) -> tuple[str, int]:
    start = time.time()
    response = llm.create_chat_completion(
        messages=messages,
        max_tokens=max_tokens,
        temperature=temperature,
        top_k=top_k,
        top_p=top_p,
        repeat_penalty=repeat_penalty,
    )
    elapsed_ms = round((time.time() - start) * 1000)
    raw = response["choices"][0]["message"]["content"] or ""
    return raw, elapsed_ms


def score_case(row: dict, accepted_output: str) -> dict:
    score = bench.score(accepted_output, row["expected"])
    return {
        "exact": score["exact"],
        "similarity": score["similarity"],
        "wer": score["wer"],
    }


def run_short_single(llm: Llama, row: dict, args: argparse.Namespace) -> dict:
    raw, rewrite_ms = chat(
        llm,
        build_rewrite_messages(row["input"]),
        dynamic_rewrite_tokens(row["input"]),
        args.rewrite_temperature,
        args.rewrite_top_k,
        args.rewrite_top_p,
        args.rewrite_repeat_penalty,
    )
    output = bench.clean_output(raw, REWRITE_SYSTEM_PROMPT)
    scored = score_case(row, output)
    validation = validate_candidate_detailed(row["input"], output)
    return {
        **scored,
        "id": row["id"],
        "category": row["category"],
        "input": row["input"],
        "expected": row["expected"],
        "truth": classifier_truth(row),
        "classifier_raw": "",
        "classifier_decision": "POLISH",
        "classifier_valid": True,
        "classifier_latency_ms": 0,
        "rewrite_called": True,
        "rewrite_raw": raw.strip(),
        "rewrite_output": output,
        "rewrite_latency_ms": rewrite_ms,
        "accepted_output": output,
        "validation_reasons": validation["hard_unsafe"],
        "hard_unsafe": validation["hard_unsafe"],
        "review_flags": validation["review_flags"],
        "fallback_used": False,
        "latency_ms": rewrite_ms,
    }


def run_two_pass(llm: Llama, row: dict, args: argparse.Namespace, safe: bool) -> dict:
    classifier_raw, classifier_ms = chat(
        llm,
        build_classifier_messages(row["input"]),
        args.classifier_max_tokens,
        0.0,
        0,
        1.0,
        1.0,
    )
    decision, valid_classifier = parse_classifier_output(classifier_raw)
    rewrite_raw = ""
    rewrite_output = ""
    rewrite_ms = 0
    reasons: list[str] = []
    review_flags: list[str] = []
    fallback_used = False
    accepted = row["input"]

    if decision == "POLISH":
        rewrite_raw, rewrite_ms = chat(
            llm,
            build_rewrite_messages(row["input"]),
            dynamic_rewrite_tokens(row["input"]),
            args.rewrite_temperature,
            args.rewrite_top_k,
            args.rewrite_top_p,
            args.rewrite_repeat_penalty,
        )
        rewrite_output = bench.clean_output(rewrite_raw, REWRITE_SYSTEM_PROMPT)
        validation = validate_candidate_detailed(row["input"], rewrite_output) if safe else {"hard_unsafe": [], "review_flags": []}
        reasons = validation["hard_unsafe"]
        review_flags = validation["review_flags"]
        if safe and reasons:
            fallback_used = True
            accepted = row["input"]
        else:
            accepted = rewrite_output

    scored = score_case(row, accepted)
    return {
        **scored,
        "id": row["id"],
        "category": row["category"],
        "input": row["input"],
        "expected": row["expected"],
        "truth": classifier_truth(row),
        "classifier_raw": classifier_raw.strip(),
        "classifier_decision": decision,
        "classifier_valid": valid_classifier,
        "classifier_latency_ms": classifier_ms,
        "rewrite_called": decision == "POLISH",
        "rewrite_raw": rewrite_raw.strip(),
        "rewrite_output": rewrite_output,
        "rewrite_latency_ms": rewrite_ms,
        "accepted_output": accepted,
        "validation_reasons": reasons,
        "hard_unsafe": reasons,
        "review_flags": review_flags,
        "fallback_used": fallback_used,
        "latency_ms": classifier_ms + rewrite_ms,
    }


def summarize(results: list[dict]) -> dict:
    latencies = [row["latency_ms"] for row in results]
    rewrite_latencies = [row["rewrite_latency_ms"] for row in results if row["rewrite_called"]]
    exact_n = sum(1 for row in results if row["exact"])
    truth_polish = sum(1 for row in results if row["truth"] == "POLISH")
    truth_clean = len(results) - truth_polish
    predicted_polish = sum(1 for row in results if row["classifier_decision"] == "POLISH")
    predicted_clean = len(results) - predicted_polish
    tp = sum(1 for row in results if row["truth"] == "POLISH" and row["classifier_decision"] == "POLISH")
    fp = sum(1 for row in results if row["truth"] == "CLEAN" and row["classifier_decision"] == "POLISH")
    fn = sum(1 for row in results if row["truth"] == "POLISH" and row["classifier_decision"] == "CLEAN")
    precision = tp / predicted_polish if predicted_polish else 0.0
    recall = tp / truth_polish if truth_polish else 0.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    invalid_classifier = sum(1 for row in results if not row["classifier_valid"])
    fallback_n = sum(1 for row in results if row["fallback_used"])
    reasons: dict[str, int] = {}
    review_flags: dict[str, int] = {}
    for row in results:
        for reason in row.get("hard_unsafe", row["validation_reasons"]):
            reasons[reason] = reasons.get(reason, 0) + 1
        for flag in row.get("review_flags", []):
            review_flags[flag] = review_flags.get(flag, 0) + 1
    hard_unsafe_cases = sum(1 for row in results if row.get("hard_unsafe", row["validation_reasons"]))
    review_flag_cases = sum(1 for row in results if row.get("review_flags", []))
    return {
        "n": len(results),
        "exact_matches": exact_n,
        "exact_pct": round(100 * exact_n / len(results), 1),
        "mean_similarity": round(statistics.mean(row["similarity"] for row in results), 4),
        "mean_wer": round(statistics.mean(row["wer"] for row in results), 4),
        "mean_latency_ms": round(statistics.mean(latencies)),
        "p50_latency_ms": round(statistics.median(latencies)),
        "p95_latency_ms": round(sorted(latencies)[int(0.95 * (len(latencies) - 1))]),
        "rewrite_calls": predicted_polish,
        "rewrite_call_rate": round(predicted_polish / len(results), 4),
        "rewrite_p50_latency_ms": round(statistics.median(rewrite_latencies)) if rewrite_latencies else 0,
        "rewrite_p95_latency_ms": round(sorted(rewrite_latencies)[int(0.95 * (len(rewrite_latencies) - 1))]) if len(rewrite_latencies) > 1 else (round(rewrite_latencies[0]) if rewrite_latencies else 0),
        "truth_polish": truth_polish,
        "truth_clean": truth_clean,
        "predicted_polish": predicted_polish,
        "predicted_clean": predicted_clean,
        "classifier_precision": round(precision, 4),
        "classifier_recall": round(recall, 4),
        "classifier_f1": round(f1, 4),
        "classifier_false_clean": fn,
        "classifier_false_polish": fp,
        "classifier_invalid": invalid_classifier,
        "fallbacks": fallback_n,
        "unsafe_cases": hard_unsafe_cases,
        "hard_unsafe_cases": hard_unsafe_cases,
        "review_flag_cases": review_flag_cases,
        "validation_reasons": reasons,
        "hard_unsafe": reasons,
        "review_flags": review_flags,
    }


def run_mode(llm: Llama, rows: list[dict], mode: str, args: argparse.Namespace) -> dict:
    print(f"\n=== mode: {mode} ===", flush=True)
    results = []
    for row in rows:
        if mode == "short_single":
            result = run_short_single(llm, row, args)
        elif mode == "two_pass":
            result = run_two_pass(llm, row, args, safe=False)
        elif mode == "two_pass_safe":
            result = run_two_pass(llm, row, args, safe=True)
        else:
            raise ValueError(f"unknown mode {mode}")
        results.append(result)
        marker = "OK " if result["exact"] else ("~ " if result["similarity"] >= 0.9 else "X  ")
        route = "rewrite" if result["rewrite_called"] else "clean"
        safe = f" fallback={','.join(result['validation_reasons'])}" if result["fallback_used"] else ""
        review = f" review={','.join(result['review_flags'])}" if result.get("review_flags") else ""
        print(
            f"    {marker}{row['id']:14s} sim={result['similarity']:.2f} "
            f"{result['latency_ms']:4d}ms {route}{safe}{review}",
            flush=True,
        )
    summary = summarize(results)
    print(
        f"  -> {mode}: exact {summary['exact_pct']}%, sim {summary['mean_similarity']}, "
        f"p50 {summary['p50_latency_ms']}ms, rewrite {summary['rewrite_call_rate']:.0%}",
        flush=True,
    )
    return {
        "mode": mode,
        "summary": summary,
        "results": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="gemma-4-e2b-it-q4", choices=sorted(MODELS))
    parser.add_argument("--modes", nargs="*", default=["short_single", "two_pass", "two_pass_safe"])
    parser.add_argument("--ctx", type=int, default=2048)
    parser.add_argument("--gpu-layers", type=int, default=-1)
    parser.add_argument("--batch", type=int, default=512)
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--flash-attn", action="store_true")
    parser.add_argument("--classifier-max-tokens", type=int, default=4)
    parser.add_argument("--rewrite-temperature", type=float, default=0.1)
    parser.add_argument("--rewrite-top-k", type=int, default=50)
    parser.add_argument("--rewrite-top-p", type=float, default=0.95)
    parser.add_argument("--rewrite-repeat-penalty", type=float, default=1.05)
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

    modes = [run_mode(llm, rows, mode, args) for mode in args.modes]
    payload = {
        "model": args.model,
        "label": info["label"],
        "family": info["family"],
        "repo": info["repo"],
        "filename": info["filename"],
        "runtime": "llama-cpp-python GGUF/Metal",
        "flash_attn": args.flash_attn,
        "load_ms": load_ms,
        "classifier_prompt": CLASSIFIER_SYSTEM_PROMPT,
        "rewrite_prompt": REWRITE_SYSTEM_PROMPT,
        "rewrite_few_shot": list(REWRITE_FEW_SHOT),
        "modes": modes,
    }
    with (out_dir / "all.json").open("w") as handle:
        json.dump(payload, handle, indent=2)
    for mode in modes:
        with (out_dir / f"{mode['mode']}.json").open("w") as handle:
            json.dump(mode, handle, indent=2)
    print(f"\nDONE -> {out_dir / 'all.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

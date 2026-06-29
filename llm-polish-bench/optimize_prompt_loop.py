#!/usr/bin/env python3
"""Optimize the Gemma polish prompt with an offline model-in-the-loop search.

The optimizer model proposes short prompt candidates. The local target model
executes each candidate against the dataset, and this script renders a static
HTML dashboard for comparing quality, latency, safety, and per-example output.
"""

from __future__ import annotations

import argparse
import html
import importlib.util
import json
import os
import random
import re
import statistics
import time
import urllib.error
import urllib.request
from collections import defaultdict
from pathlib import Path
from typing import Any

from llama_cpp import Llama

from bench_gguf_hf import MODELS, download_model
from bench_two_pass import dynamic_rewrite_tokens, validate_candidate_detailed

HERE = Path(__file__).resolve().parent
OUT_DIR = HERE / "out" / "prompt-optimizer"

_bench_spec = importlib.util.spec_from_file_location("bench_prompts", HERE / "bench.py")
bench = importlib.util.module_from_spec(_bench_spec)
assert _bench_spec.loader is not None
_bench_spec.loader.exec_module(bench)


BASELINE_SYSTEM_PROMPT = (
    "Clean voice-dictation disfluencies only. Remove fillers, repeated words, "
    "false starts, and correction cues with replacements. Preserve the final "
    "meaning and every fact-like token. Do not convert spoken digits/dot/at "
    "to symbols. If unsure, return unchanged. Output only cleaned text."
)

BASELINE_FEW_SHOT = [
    {
        "input": "Please open settings, no wait, open the dashboard.",
        "output": "Please open the dashboard.",
    },
    {
        "input": "Her email is jane, no, janet dot smith at example dot com.",
        "output": "Her email is janet dot smith at example dot com.",
    },
]

REPAIR_FEW_SHOT = [
    {
        "input": "My account number is four seven two, um, four seven two nine three one.",
        "output": "My account number is four seven two nine three one.",
    },
    {
        "input": "Her email is jane, no, janet dot smith at example dot com.",
        "output": "Her email is janet dot smith at example dot com.",
    },
]


def word_count(text: str) -> int:
    return len(re.findall(r"\S+", text.strip()))


def slugify(text: str) -> str:
    slug = re.sub(r"[^a-zA-Z0-9]+", "-", text.lower()).strip("-")
    return slug or "prompt"


def percentile(values: list[int], pct: float) -> int:
    if not values:
        return 0
    idx = int(pct * (len(values) - 1))
    return round(sorted(values)[idx])


def split_rows(rows: list[dict], seed: int) -> list[dict]:
    """Assign a stable train/dev/test split, stratified by category."""
    rng = random.Random(seed)
    by_category: dict[str, list[dict]] = defaultdict(list)
    for row in rows:
        by_category[row["category"]].append(dict(row))

    split_rows_out: list[dict] = []
    for category_rows in by_category.values():
        shuffled = list(category_rows)
        rng.shuffle(shuffled)
        n = len(shuffled)
        train_end = max(1, round(n * 0.70))
        dev_end = train_end + max(1, round(n * 0.15)) if n >= 4 else train_end
        for index, row in enumerate(shuffled):
            if index < train_end:
                split = "train"
            elif index < dev_end:
                split = "dev"
            else:
                split = "test"
            row["split"] = split
            split_rows_out.append(row)
    return sorted(split_rows_out, key=lambda row: row["id"])


def make_baseline_candidate() -> dict:
    return {
        "id": "baseline-compact-balanced-two-shot",
        "name": "baseline_compact_balanced_two_shot",
        "round": 0,
        "source": "baseline",
        "system_prompt": BASELINE_SYSTEM_PROMPT,
        "few_shot": list(BASELINE_FEW_SHOT),
        "rationale": "Current best manual prompt from the Gemma prompt sweep.",
    }


def load_repair_candidate(prompt_id: str, out_dir: Path) -> dict:
    search_dirs = []
    for candidate_dir in (out_dir, OUT_DIR):
        if candidate_dir not in search_dirs:
            search_dirs.append(candidate_dir)

    for directory in search_dirs:
        path = directory / f"{slugify(prompt_id)}.json"
        if path.exists():
            return normalize_existing_candidate(json.loads(path.read_text()), prompt_id, "repair_base")

    for directory in search_dirs:
        path = directory / "all.json"
        if not path.exists():
            continue
        payload = json.loads(path.read_text())
        for item in payload.get("prompt_results", []):
            if item.get("id") == prompt_id or slugify(str(item.get("id", ""))) == slugify(prompt_id):
                return normalize_existing_candidate(item, prompt_id, "repair_base")

    searched = ", ".join(str(directory) for directory in search_dirs)
    raise SystemExit(f"could not find repair prompt {prompt_id!r} in {searched}")


def normalize_existing_candidate(raw: dict, prompt_id: str, source: str) -> dict:
    system_prompt = str(raw.get("system_prompt", "")).strip()
    if not system_prompt:
        raise SystemExit(f"repair prompt {prompt_id!r} has no system_prompt")
    return {
        "id": str(raw.get("id") or prompt_id),
        "name": slugify(str(raw.get("name") or prompt_id)).replace("-", "_"),
        "round": int(raw.get("round", 0) or 0),
        "source": source,
        "system_prompt": system_prompt,
        "few_shot": normalize_few_shot(raw.get("few_shot", [])),
        "rationale": str(raw.get("rationale", "")).strip() or "Prompt selected as the repair base.",
    }


def normalize_few_shot(value: Any) -> list[dict[str, str]]:
    few_shot: list[dict[str, str]] = []
    if not isinstance(value, list):
        return few_shot
    for item in value:
        if isinstance(item, dict):
            input_text = str(item.get("input", "")).strip()
            output_text = str(item.get("output", "")).strip()
        elif isinstance(item, (list, tuple)) and len(item) >= 2:
            input_text = str(item[0]).strip()
            output_text = str(item[1]).strip()
        else:
            continue
        if input_text and output_text:
            few_shot.append({"input": input_text, "output": output_text})
    return few_shot[:2]


def build_messages(candidate: dict, text: str) -> list[dict[str, str]]:
    messages = [{"role": "system", "content": candidate["system_prompt"]}]
    for shot in candidate.get("few_shot", []):
        messages.append({"role": "user", "content": f"Clean this transcript:\n{shot['input']}"})
        messages.append({"role": "assistant", "content": shot["output"]})
    messages.append({"role": "user", "content": f"Clean this transcript:\n{text}"})
    return messages


def metrics_for(results: list[dict]) -> dict:
    if not results:
        return {
            "n": 0,
            "exact_matches": 0,
            "exact_pct": 0.0,
            "mean_similarity": 0.0,
            "mean_wer": 1.0,
            "mean_latency_ms": 0,
            "p50_latency_ms": 0,
            "p95_latency_ms": 0,
            "unsafe_cases": 0,
            "hard_unsafe_cases": 0,
            "review_flag_cases": 0,
            "chatty_cases": 0,
        }
    latencies = [row["latency_ms"] for row in results]
    exact_n = sum(1 for row in results if row["exact"])
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
        "p95_latency_ms": percentile(latencies, 0.95),
        "unsafe_cases": hard_unsafe_cases,
        "hard_unsafe_cases": hard_unsafe_cases,
        "review_flag_cases": review_flag_cases,
        "chatty_cases": sum(1 for row in results if row["chatty"]),
    }


def category_metrics_for(results: list[dict]) -> dict[str, dict]:
    by_category: dict[str, list[dict]] = defaultdict(list)
    for row in results:
        by_category[row["category"]].append(row)
    return {category: metrics_for(rows) for category, rows in sorted(by_category.items())}


def objective_for(metrics: dict, prompt_words: int, few_shot_count: int) -> float:
    """Single ranking score: quality first, then safety, brevity, and latency."""
    quality = metrics["mean_similarity"] * 100.0 + metrics["exact_pct"] * 0.15
    hard_unsafe_cases = metrics.get("hard_unsafe_cases", metrics["unsafe_cases"])
    review_flag_cases = metrics.get("review_flag_cases", 0)
    risk_penalty = hard_unsafe_cases * 1.75 + review_flag_cases * 0.25 + metrics["chatty_cases"] * 2.5
    prompt_penalty = prompt_words * 0.025 + few_shot_count * 0.50
    latency_penalty = metrics["p95_latency_ms"] / 1000.0
    return round(quality - risk_penalty - prompt_penalty - latency_penalty, 4)


def prompt_rank_key(item: dict) -> tuple:
    gates = item.get("gate_summary", {})
    return (
        1 if gates.get("all_passed") else 0,
        gates.get("passed", 0),
        item.get("objective", 0),
    )


def gate_result(name: str, actual: Any, target: Any, passed: bool, higher_is_better: bool) -> dict:
    return {
        "name": name,
        "actual": actual,
        "target": target,
        "passed": bool(passed),
        "higher_is_better": higher_is_better,
    }


def evaluate_gates(item: dict, baseline: dict | None, args: argparse.Namespace) -> dict:
    summary = item["summary"]
    gates = [
        gate_result("exact_pct", summary["exact_pct"], args.target_exact_pct, summary["exact_pct"] >= args.target_exact_pct, True),
        gate_result("mean_similarity", summary["mean_similarity"], args.target_mean_similarity, summary["mean_similarity"] >= args.target_mean_similarity, True),
        gate_result(
            "hard_unsafe_cases",
            summary.get("hard_unsafe_cases", summary["unsafe_cases"]),
            args.target_max_hard_unsafe_cases,
            summary.get("hard_unsafe_cases", summary["unsafe_cases"]) <= args.target_max_hard_unsafe_cases,
            False,
        ),
        gate_result("p50_latency_ms", summary["p50_latency_ms"], args.target_max_p50_ms, summary["p50_latency_ms"] <= args.target_max_p50_ms, False),
        gate_result("p95_latency_ms", summary["p95_latency_ms"], args.target_max_p95_ms, summary["p95_latency_ms"] <= args.target_max_p95_ms, False),
        gate_result("prompt_words", item["prompt_words"], args.max_prompt_words, item["prompt_words"] <= args.max_prompt_words, False),
        gate_result("few_shot_count", item["few_shot_count"], args.target_max_few_shot, item["few_shot_count"] <= args.target_max_few_shot, False),
    ]
    if baseline is not None:
        gates.append(
            gate_result(
                "test_similarity_vs_baseline",
                item["split_summary"]["test"]["mean_similarity"],
                baseline["split_summary"]["test"]["mean_similarity"],
                item["split_summary"]["test"]["mean_similarity"] >= baseline["split_summary"]["test"]["mean_similarity"],
                True,
            )
        )
        for category in args.no_regress_categories:
            actual = item["category_summary"].get(category, {}).get("mean_similarity", 0.0)
            target = baseline["category_summary"].get(category, {}).get("mean_similarity", 0.0)
            gates.append(
                gate_result(
                    f"{category}_similarity_vs_baseline",
                    actual,
                    target,
                    actual >= target,
                    True,
                )
            )
    passed = sum(1 for gate in gates if gate["passed"])
    return {
        "passed": passed,
        "total": len(gates),
        "all_passed": passed == len(gates),
        "gates": gates,
    }


def attach_gates(prompt_results: list[dict], args: argparse.Namespace) -> None:
    baseline = next((item for item in prompt_results if item.get("source") == "baseline"), None)
    for item in prompt_results:
        item["gate_summary"] = evaluate_gates(item, baseline, args)


def evaluate_candidate(llm: Llama, rows: list[dict], candidate: dict, args: argparse.Namespace) -> dict:
    print(f"\n=== prompt {candidate['id']} ===", flush=True)
    results = []
    system_prompt = candidate["system_prompt"]
    for row in rows:
        start = time.time()
        try:
            response = llm.create_chat_completion(
                messages=build_messages(candidate, row["input"]),
                max_tokens=dynamic_rewrite_tokens(row["input"]),
                temperature=args.rewrite_temperature,
                top_k=args.rewrite_top_k,
                top_p=args.rewrite_top_p,
                repeat_penalty=args.rewrite_repeat_penalty,
            )
            raw = response["choices"][0]["message"]["content"] or ""
        except Exception as error:
            raw = ""
            print(f"    [{row['id']}] ERROR {error}", flush=True)
        latency_ms = round((time.time() - start) * 1000)
        output = bench.clean_output(raw, system_prompt)
        score = bench.score(output, row["expected"])
        validation = validate_candidate_detailed(row["input"], output)
        validation_reasons = validation["hard_unsafe"]
        review_flags = validation["review_flags"]
        result = {
            "id": row["id"],
            "category": row["category"],
            "split": row["split"],
            "input": row["input"],
            "expected": row["expected"],
            "output": output,
            "raw_output": raw.strip(),
            "exact": score["exact"],
            "similarity": score["similarity"],
            "wer": score["wer"],
            "latency_ms": latency_ms,
            "validation_reasons": validation_reasons,
            "hard_unsafe": validation_reasons,
            "review_flags": review_flags,
            "chatty": bench.is_refusal_or_chatty(output),
        }
        results.append(result)
        marker = "OK " if score["exact"] else ("~ " if score["similarity"] >= 0.9 else "X  ")
        hard = f" hard={','.join(validation_reasons)}" if validation_reasons else ""
        review = f" review={','.join(review_flags)}" if review_flags else ""
        print(f"    {marker}{row['id']:14s} {row['split']:5s} sim={score['similarity']:.2f} {latency_ms:4d}ms{hard}{review}", flush=True)

    split_summary = {
        split: metrics_for([row for row in results if row["split"] == split])
        for split in ("train", "dev", "test")
    }
    all_summary = metrics_for(results)
    category_summary = category_metrics_for(results)
    prompt_words = word_count(system_prompt)
    few_shot_count = len(candidate.get("few_shot", []))

    evaluated = {
        **candidate,
        "prompt_words": prompt_words,
        "few_shot_count": few_shot_count,
        "summary": all_summary,
        "split_summary": split_summary,
        "category_summary": category_summary,
        "selection_split": "all",
        "objective": objective_for(all_summary, prompt_words, few_shot_count),
        "results": results,
    }
    print(
        f"  -> objective {evaluated['objective']}, exact {all_summary['exact_pct']}%, "
        f"sim {all_summary['mean_similarity']}, p50 {all_summary['p50_latency_ms']}ms, "
        f"p95 {all_summary['p95_latency_ms']}ms, hard unsafe {all_summary['hard_unsafe_cases']}, "
        f"review {all_summary['review_flag_cases']}",
        flush=True,
    )
    return evaluated


def summarize_history_for_optimizer(prompt_results: list[dict], limit: int) -> list[dict]:
    ranked = sorted(prompt_results, key=prompt_rank_key, reverse=True)[:limit]
    return [
        {
            "id": item["id"],
            "name": item["name"],
            "round": item["round"],
            "objective": item["objective"],
            "gates": item.get("gate_summary", {}),
            "selection_split": item["selection_split"],
            "all_exact_pct": item["summary"]["exact_pct"],
            "all_mean_similarity": item["summary"]["mean_similarity"],
            "all_hard_unsafe_cases": item["summary"].get("hard_unsafe_cases", item["summary"]["unsafe_cases"]),
            "all_review_flag_cases": item["summary"].get("review_flag_cases", 0),
            "all_p95_latency_ms": item["summary"]["p95_latency_ms"],
            "prompt_words": item["prompt_words"],
            "few_shot_count": item["few_shot_count"],
            "system_prompt": item["system_prompt"],
            "few_shot": item.get("few_shot", []),
            "rationale": item.get("rationale", ""),
        }
        for item in ranked
    ]


def failure_examples(best_result: dict, limit: int) -> list[dict]:
    failures = [
        row for row in best_result["results"]
        if row["split"] == "train"
        and (not row["exact"] or row.get("hard_unsafe", row["validation_reasons"]) or row.get("review_flags", []) or row["chatty"])
    ]
    failures.sort(key=lambda row: (row["exact"], row["similarity"], -len(row.get("hard_unsafe", row["validation_reasons"]))))
    return [
        {
            "id": row["id"],
            "category": row["category"],
            "input": row["input"],
            "expected": row["expected"],
            "output": row["output"],
            "similarity": row["similarity"],
            "hard_unsafe": row.get("hard_unsafe", row["validation_reasons"]),
            "review_flags": row.get("review_flags", []),
            "issue": infer_issue(row),
        }
        for row in failures[:limit]
    ]


def infer_issue(row: dict) -> str:
    hard_unsafe = row.get("hard_unsafe", row["validation_reasons"])
    review_flags = row.get("review_flags", [])
    if hard_unsafe:
        return "hard unsafe: " + ", ".join(hard_unsafe)
    if review_flags:
        return "review: " + ", ".join(review_flags)
    if row["similarity"] < 0.5:
        return "large semantic mismatch"
    if len(bench.norm(row["output"]).split()) < len(bench.norm(row["expected"]).split()):
        return "likely over-deletion"
    if len(bench.norm(row["output"]).split()) > len(bench.norm(row["expected"]).split()):
        return "left extra disfluency or abandoned wording"
    return "wording differs from expected"


def optimizer_payload(prompt_results: list[dict], round_index: int, args: argparse.Namespace) -> dict:
    repair_base = next((item for item in prompt_results if item.get("source") == "repair_base"), None)
    best = repair_base or max(prompt_results, key=prompt_rank_key)
    task = "Improve the system prompt for a small local model that cleans voice-dictation transcripts."
    constraints = [
        f"Return exactly {args.candidates_per_round} candidates.",
        f"Each system_prompt must be <= {args.max_prompt_words} words.",
        "Use 0 to 2 few_shot examples total.",
        "Each rationale must be <= 25 words.",
        "Do not add lots of dataset-specific examples.",
        "Do not broaden into style rewriting, summarization, formatting, punctuation rewriting, or answering.",
        "The runtime model is small and fast; the prompt must be compact and literal.",
        "Preserve names, facts, numbers, spoken digit words, code, emails, URLs, and literal negation.",
        "Output JSON only. No markdown.",
    ]
    if repair_base:
        task = "Minimally repair the selected prompt for a small local voice-dictation cleanup model."
        constraints = [
            f"Return exactly {args.candidates_per_round} candidates.",
            "Repair mode: minimally edit repair_prompt_to_edit instead of inventing fresh prompts.",
            "Preserve the selected prompt's strengths on false starts, explicit cancel, mixed cleanup, and avoiding broad style rewrites.",
            "Focus repairs on numeric-prefix preservation and email correction without formatting.",
            f"Each system_prompt must be <= {args.max_prompt_words} words.",
            "Every candidate must use exactly the two provided repair_examples as few_shot, unchanged.",
            "Each rationale must be <= 25 words.",
            "Do not add lots of dataset-specific examples.",
            "Do not broaden into style rewriting, summarization, formatting, punctuation rewriting, or answering.",
            "The runtime model is small and fast; the prompt must be compact and literal.",
            "Preserve names, facts, numbers, spoken digit words, code, emails, URLs, and literal negation.",
            "Output JSON only. No markdown.",
        ]
    return {
        "task": task,
        "mode": "repair" if repair_base else "broad_search",
        "round": round_index,
        "numeric_targets": {
            "exact_pct_at_least": args.target_exact_pct,
            "mean_similarity_at_least": args.target_mean_similarity,
            "hard_unsafe_cases_at_most": args.target_max_hard_unsafe_cases,
            "p50_latency_ms_at_most": args.target_max_p50_ms,
            "p95_latency_ms_at_most": args.target_max_p95_ms,
            "prompt_words_at_most": args.max_prompt_words,
            "few_shot_count_at_most": args.target_max_few_shot,
            "no_regress_categories": args.no_regress_categories,
        },
        "constraints": constraints,
        "output_schema": {
            "candidates": [
                {
                    "name": "short_snake_case_name",
                    "system_prompt": "prompt text",
                    "few_shot": [{"input": "optional example", "output": "optional cleaned example"}],
                    "rationale": "why this should improve the target model",
                }
            ]
        },
        "scoring_objective": (
            "First pass all numeric targets and no-regression gates. Then maximize "
            "similarity and exact match, minimize hard unsafe rewrites, keep p95 latency "
            "low, and keep the prompt short."
        ),
        "top_prompt_history": summarize_history_for_optimizer(prompt_results, args.history_limit),
        "train_failures_from_current_best": failure_examples(best, args.failure_limit),
        "repair_prompt_to_edit": summarize_history_for_optimizer([repair_base], 1)[0] if repair_base else None,
        "repair_examples": REPAIR_FEW_SHOT if repair_base else None,
    }


def call_optimizer(payload: dict, args: argparse.Namespace) -> tuple[list[dict], dict]:
    key = os.environ.get(args.optimizer_api_key_env)
    if not key:
        raise SystemExit(f"{args.optimizer_api_key_env} is required")

    if args.optimizer_endpoint == "responses":
        return call_responses_optimizer(payload, args, key)
    return call_chat_optimizer(payload, args, key)


def call_responses_optimizer(payload: dict, args: argparse.Namespace, key: str) -> tuple[list[dict], dict]:
    instructions = (
        "You are an offline prompt optimizer. Propose compact prompts for a smaller "
        "target model. Follow the requested JSON schema exactly. Output JSON only."
    )
    if payload.get("mode") == "repair":
        instructions = (
            "You are an offline prompt repair optimizer. Minimally edit the selected "
            "prompt for a smaller target model. Preserve known strengths, use the "
            "provided repair examples exactly, and output JSON only."
        )
    request_body = {
        "model": args.optimizer_model,
        "instructions": instructions,
        "input": json.dumps(payload, indent=2),
        "max_output_tokens": args.optimizer_max_tokens,
    }
    endpoint = args.optimizer_base_url.rstrip("/") + "/responses"
    response, latency_ms = post_optimizer_json(endpoint, key, request_body, args.optimizer_timeout_s)
    content = extract_responses_text(response)
    parsed = parse_optimizer_json(content)
    candidates = parsed.get("candidates", [])
    if not isinstance(candidates, list):
        candidates = []
    return candidates, {
        "provider": args.optimizer_provider,
        "endpoint": args.optimizer_endpoint,
        "model": args.optimizer_model,
        "latency_ms": latency_ms,
        "usage": response.get("usage"),
        "cost": response.get("cost"),
        "raw_content": content,
    }


def call_chat_optimizer(payload: dict, args: argparse.Namespace, key: str) -> tuple[list[dict], dict]:
    system_content = (
        "You are an offline prompt optimizer. Propose compact prompts for a smaller "
        "target model. Follow the requested JSON schema exactly."
    )
    if payload.get("mode") == "repair":
        system_content = (
            "You are an offline prompt repair optimizer. Minimally edit the selected "
            "prompt for a smaller target model. Preserve known strengths and use the "
            "provided repair examples exactly."
        )
    request_body = {
        "model": args.optimizer_model,
        "messages": [
            {
                "role": "system",
                "content": system_content,
            },
            {"role": "user", "content": json.dumps(payload, indent=2)},
        ],
        "max_tokens": args.optimizer_max_tokens,
        "temperature": args.optimizer_temperature,
        "top_p": args.optimizer_top_p,
    }
    endpoint = args.optimizer_base_url.rstrip("/") + "/chat/completions"
    response, latency_ms = post_optimizer_json(endpoint, key, request_body, args.optimizer_timeout_s)
    content = response["choices"][0]["message"].get("content") or ""
    parsed = parse_optimizer_json(content)
    candidates = parsed.get("candidates", [])
    if not isinstance(candidates, list):
        candidates = []
    return candidates, {
        "provider": args.optimizer_provider,
        "endpoint": args.optimizer_endpoint,
        "model": args.optimizer_model,
        "latency_ms": latency_ms,
        "usage": response.get("usage"),
        "cost": response.get("cost"),
        "raw_content": content,
    }


def post_optimizer_json(endpoint: str, key: str, request_body: dict, timeout_s: int) -> tuple[dict, int]:
    req = urllib.request.Request(
        endpoint,
        data=json.dumps(request_body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    start = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            response = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", "replace")
        raise RuntimeError(f"optimizer HTTP {error.code}: {body}") from error
    return response, round((time.time() - start) * 1000)


def extract_responses_text(response: dict) -> str:
    if response.get("output_text"):
        return str(response["output_text"])
    parts: list[str] = []
    for item in response.get("output", []):
        for content in item.get("content", []):
            if content.get("type") == "output_text":
                parts.append(str(content.get("text", "")))
    return "\n".join(part for part in parts if part).strip()


def parse_optimizer_json(content: str) -> dict:
    text = content.strip()
    fence = re.search(r"```(?:json)?\s*(.*?)```", text, flags=re.S | re.I)
    if fence:
        text = fence.group(1).strip()
    if not text.startswith("{"):
        start = text.find("{")
        end = text.rfind("}")
        if start >= 0 and end > start:
            text = text[start:end + 1]
    return json.loads(text)


def normalize_candidates(raw_candidates: list[dict], round_index: int, args: argparse.Namespace, seen_ids: set[str]) -> list[dict]:
    normalized = []
    for index, raw in enumerate(raw_candidates, 1):
        if not isinstance(raw, dict):
            continue
        system_prompt = str(raw.get("system_prompt", "")).strip()
        if not system_prompt:
            continue
        prompt_words = word_count(system_prompt)
        if prompt_words > args.max_prompt_words:
            print(
                f"skipping overlong candidate {raw.get('name', index)}: "
                f"{prompt_words} words > {args.max_prompt_words}",
                flush=True,
            )
            continue
        name = slugify(str(raw.get("name") or f"round_{round_index}_{index}")).replace("-", "_")
        candidate_id = f"r{round_index}-{slugify(name)}"
        suffix = 2
        while candidate_id in seen_ids:
            candidate_id = f"r{round_index}-{slugify(name)}-{suffix}"
            suffix += 1
        seen_ids.add(candidate_id)
        few_shot = [dict(shot) for shot in REPAIR_FEW_SHOT] if args.repair_prompt_id else normalize_few_shot(raw.get("few_shot", []))
        normalized.append(
            {
                "id": candidate_id,
                "name": name,
                "round": round_index,
                "source": "repair_optimizer" if args.repair_prompt_id else "optimizer",
                "system_prompt": system_prompt,
                "few_shot": few_shot,
                "rationale": str(raw.get("rationale", "")).strip(),
            }
        )
    return normalized


def fmt(value: Any) -> str:
    if isinstance(value, float):
        return f"{value:.4f}".rstrip("0").rstrip(".")
    return str(value)


def esc(value: Any) -> str:
    return html.escape(str(value), quote=True)


def status_class(row: dict) -> str:
    if row["exact"]:
        return "ok"
    if row["similarity"] >= 0.9:
        return "warn"
    return "bad"


def generate_report(payload: dict, path: Path) -> None:
    results = payload["prompt_results"]
    ranked = sorted(results, key=prompt_rank_key, reverse=True)
    best = ranked[0]
    baseline = next((item for item in results if item["source"] == "baseline"), results[0])
    categories = sorted({row["category"] for item in results for row in item["results"]})
    splits = ("train", "dev", "test")
    best_gates = best.get("gate_summary", {"passed": 0, "total": 0, "all_passed": False, "gates": []})

    parts: list[str] = []
    parts.append("<!doctype html><html><head><meta charset='utf-8'>")
    parts.append("<meta name='viewport' content='width=device-width, initial-scale=1'>")
    parts.append("<title>LLM Polish Prompt Optimizer</title>")
    parts.append(
        """
<style>
:root { color-scheme: light; --bg:#f7f8fa; --panel:#ffffff; --ink:#17202a; --muted:#667085; --line:#d8dee8; --blue:#2563eb; --green:#16794c; --yellow:#9a6700; --red:#c2410c; }
* { box-sizing: border-box; }
body { margin:0; font:14px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; background:var(--bg); color:var(--ink); }
header { padding:28px 32px 18px; background:#fff; border-bottom:1px solid var(--line); }
h1 { margin:0 0 6px; font-size:26px; letter-spacing:0; }
h2 { margin:28px 0 12px; font-size:18px; }
h3 { margin:18px 0 8px; font-size:15px; }
.wrap { max-width:1400px; margin:0 auto; padding:0 24px 32px; }
.subtle { color:var(--muted); }
.cards { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); gap:12px; margin:18px 0; }
.card { background:var(--panel); border:1px solid var(--line); border-radius:8px; padding:14px; }
.metric { font-size:24px; font-weight:700; margin-top:6px; }
.label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.04em; }
.toolbar { display:flex; flex-wrap:wrap; gap:10px; align-items:center; margin:14px 0; }
input, select { border:1px solid var(--line); border-radius:6px; padding:8px 10px; background:#fff; color:var(--ink); }
table { width:100%; border-collapse:collapse; background:#fff; border:1px solid var(--line); border-radius:8px; overflow:hidden; }
th, td { padding:9px 10px; border-bottom:1px solid var(--line); vertical-align:top; text-align:left; }
th { background:#eef2f7; font-weight:650; color:#344054; position:sticky; top:0; z-index:1; }
tr:last-child td { border-bottom:0; }
.num { text-align:right; font-variant-numeric:tabular-nums; white-space:nowrap; }
.pill { display:inline-block; border-radius:999px; padding:2px 8px; font-size:12px; background:#eef2f7; color:#344054; white-space:nowrap; }
.pill.ok { background:#dcfce7; color:var(--green); }
.pill.warn { background:#fef3c7; color:var(--yellow); }
.pill.bad { background:#ffedd5; color:var(--red); }
.prompt { white-space:pre-wrap; background:#f6f8fb; border:1px solid var(--line); border-radius:6px; padding:10px; }
details { background:#fff; border:1px solid var(--line); border-radius:8px; margin:12px 0; }
summary { cursor:pointer; padding:12px 14px; font-weight:650; }
details > .inside { padding:0 14px 14px; }
.grid2 { display:grid; grid-template-columns:minmax(0,1fr) minmax(0,1fr); gap:12px; }
.raw { white-space:pre-wrap; font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:12px; background:#f6f8fb; border:1px solid var(--line); border-radius:6px; padding:8px; }
.out { white-space:pre-wrap; }
.hide { display:none; }
.best { outline:2px solid rgba(37,99,235,.35); }
.small { font-size:12px; }
@media (max-width: 900px) { .cards, .grid2 { grid-template-columns:1fr; } th, td { padding:8px 7px; } }
</style>
"""
    )
    parts.append("</head><body>")
    parts.append("<header><div class='wrap'>")
    parts.append("<h1>LLM Polish Prompt Optimizer</h1>")
    parts.append(
        f"<div class='subtle'>Target: {esc(payload['target_model'])} via llama.cpp/Metal. "
        f"Optimizer: {esc(payload['optimizer_model'])}. Generated {esc(payload['generated_at'])}.</div>"
    )
    parts.append("</div></header><main class='wrap'>")

    parts.append("<section class='cards'>")
    cards = [
        ("Best Prompt", best["name"]),
        ("Gate Status", "PASS" if best_gates.get("all_passed") else "NEEDS WORK"),
        ("Targets", f"{best_gates.get('passed', 0)}/{best_gates.get('total', 0)}"),
        ("Exact", f"{best['summary']['exact_pct']}%"),
        ("Similarity", fmt(best["summary"]["mean_similarity"])),
    ]
    for label, value in cards:
        parts.append(f"<div class='card'><div class='label'>{esc(label)}</div><div class='metric'>{esc(value)}</div></div>")
    parts.append("</section>")

    parts.append("<h2>Target Gates</h2>")
    parts.append("<table><thead><tr><th>Gate</th><th>Best actual</th><th>Target</th><th>Status</th></tr></thead><tbody>")
    for gate in best_gates.get("gates", []):
        cls = "ok" if gate["passed"] else "bad"
        status = "pass" if gate["passed"] else "fail"
        parts.append("<tr>")
        parts.append(f"<td>{esc(gate['name'])}</td><td>{esc(gate['actual'])}</td><td>{esc(gate['target'])}</td>")
        parts.append(f"<td><span class='pill {cls}'>{status}</span></td>")
        parts.append("</tr>")
    parts.append("</tbody></table>")

    parts.append("<section class='cards'>")
    deltas = {
        "exact": round(best["summary"]["exact_pct"] - baseline["summary"]["exact_pct"], 1),
        "sim": round(best["summary"]["mean_similarity"] - baseline["summary"]["mean_similarity"], 4),
        "p95": best["summary"]["p95_latency_ms"] - baseline["summary"]["p95_latency_ms"],
        "hard_unsafe": best["summary"]["hard_unsafe_cases"] - baseline["summary"]["hard_unsafe_cases"],
    }
    delta_cards = [
        ("Baseline", baseline["name"]),
        ("Exact Delta", f"{deltas['exact']:+.1f} pts"),
        ("Similarity Delta", f"{deltas['sim']:+.4f}"),
        ("Hard Unsafe Delta", f"{deltas['hard_unsafe']:+d}"),
    ]
    for label, value in delta_cards:
        parts.append(f"<div class='card'><div class='label'>{esc(label)}</div><div class='metric'>{esc(value)}</div></div>")
    parts.append("</section>")

    parts.append("<h2>Prompt Leaderboard</h2>")
    parts.append("<div class='toolbar'>")
    parts.append("<input id='promptSearch' placeholder='Filter prompts'>")
    parts.append("<select id='roundFilter'><option value='all'>All rounds</option>")
    for round_id in sorted({item["round"] for item in results}):
        parts.append(f"<option value='{esc(round_id)}'>Round {esc(round_id)}</option>")
    parts.append("</select></div>")
    parts.append("<table id='leaderboard'><thead><tr>")
    for heading in ("Rank", "Prompt", "Round", "Targets", "Objective", "Exact", "Similarity", "Hard Unsafe", "Review", "p50", "p95", "Words", "Few-Shot"):
        parts.append(f"<th>{heading}</th>")
    parts.append("</tr></thead><tbody>")
    for rank, item in enumerate(ranked, 1):
        best_cls = " best" if item["id"] == best["id"] else ""
        parts.append(
            f"<tr class='prompt-row{best_cls}' data-round='{esc(item['round'])}' "
            f"data-name='{esc(item['name'])} {esc(item['id'])}'>"
        )
        parts.append(f"<td class='num'>{rank}</td>")
        parts.append(f"<td><b>{esc(item['name'])}</b><br><span class='subtle small'>{esc(item['id'])}</span></td>")
        parts.append(f"<td class='num'>{esc(item['round'])}</td>")
        gates = item.get("gate_summary", {"passed": 0, "total": 0, "all_passed": False})
        gate_cls = "ok" if gates.get("all_passed") else "warn"
        parts.append(f"<td class='num'><span class='pill {gate_cls}'>{gates.get('passed', 0)}/{gates.get('total', 0)}</span></td>")
        parts.append(f"<td class='num'>{fmt(item['objective'])}</td>")
        parts.append(f"<td class='num'>{item['summary']['exact_pct']}%</td>")
        parts.append(f"<td class='num'>{fmt(item['summary']['mean_similarity'])}</td>")
        parts.append(f"<td class='num'>{item['summary']['hard_unsafe_cases']}</td>")
        parts.append(f"<td class='num'>{item['summary']['review_flag_cases']}</td>")
        parts.append(f"<td class='num'>{item['summary']['p50_latency_ms']} ms</td>")
        parts.append(f"<td class='num'>{item['summary']['p95_latency_ms']} ms</td>")
        parts.append(f"<td class='num'>{item['prompt_words']}</td>")
        parts.append(f"<td class='num'>{item['few_shot_count']}</td>")
        parts.append("</tr>")
    parts.append("</tbody></table>")

    parts.append("<h2>Split Performance</h2>")
    parts.append("<table><thead><tr><th>Prompt</th><th>Split</th><th class='num'>N</th><th class='num'>Exact</th><th class='num'>Similarity</th><th class='num'>Hard Unsafe</th><th class='num'>Review</th><th class='num'>p95</th></tr></thead><tbody>")
    for item in ranked:
        for split in splits:
            summary = item["split_summary"][split]
            parts.append("<tr>")
            parts.append(f"<td>{esc(item['name'])}</td><td>{split}</td><td class='num'>{summary['n']}</td>")
            parts.append(f"<td class='num'>{summary['exact_pct']}%</td><td class='num'>{fmt(summary['mean_similarity'])}</td>")
            parts.append(f"<td class='num'>{summary['hard_unsafe_cases']}</td><td class='num'>{summary['review_flag_cases']}</td><td class='num'>{summary['p95_latency_ms']} ms</td>")
            parts.append("</tr>")
    parts.append("</tbody></table>")

    parts.append("<h2>Prompt Details</h2>")
    parts.append("<div class='toolbar'>")
    parts.append("<select id='categoryFilter'><option value='all'>All categories</option>")
    for category in categories:
        parts.append(f"<option value='{esc(category)}'>{esc(category)}</option>")
    parts.append("</select>")
    parts.append("<select id='splitFilter'><option value='all'>All splits</option><option value='train'>Train</option><option value='dev'>Dev</option><option value='test'>Test</option></select>")
    parts.append("</div>")

    for item in ranked:
        summary = item["summary"]
        parts.append(f"<details class='prompt-detail' data-prompt='{esc(item['name'])}' {'open' if item['id'] == best['id'] else ''}>")
        parts.append(
            f"<summary>{esc(item['name'])} "
            f"<span class='pill {'ok' if item.get('gate_summary', {}).get('all_passed') else 'warn'}'>targets "
            f"{item.get('gate_summary', {}).get('passed', 0)}/{item.get('gate_summary', {}).get('total', 0)}</span> "
            f"<span class='pill'>objective {fmt(item['objective'])}</span> "
            f"<span class='pill {'ok' if summary['hard_unsafe_cases'] == 0 else 'warn'}'>{summary['hard_unsafe_cases']} hard unsafe</span> "
            f"<span class='pill'>{summary['review_flag_cases']} review</span> "
            f"<span class='pill'>exact {summary['exact_pct']}%</span></summary>"
        )
        parts.append("<div class='inside'>")
        parts.append("<div class='grid2'>")
        parts.append("<div><h3>System Prompt</h3>")
        parts.append(f"<div class='prompt'>{esc(item['system_prompt'])}</div></div>")
        parts.append("<div><h3>Rationale</h3>")
        parts.append(f"<div class='prompt'>{esc(item.get('rationale', '') or 'No rationale recorded.')}</div></div>")
        parts.append("</div>")
        if item.get("few_shot"):
            parts.append("<h3>Few-Shot Examples</h3>")
            for shot in item["few_shot"]:
                parts.append("<div class='grid2'>")
                parts.append(f"<div class='raw'><b>Input</b>\n{esc(shot['input'])}</div>")
                parts.append(f"<div class='raw'><b>Output</b>\n{esc(shot['output'])}</div>")
                parts.append("</div>")
        if item.get("gate_summary", {}).get("gates"):
            parts.append("<h3>Target Gates</h3>")
            parts.append("<table><thead><tr><th>Gate</th><th>Actual</th><th>Target</th><th>Status</th></tr></thead><tbody>")
            for gate in item["gate_summary"]["gates"]:
                cls = "ok" if gate["passed"] else "bad"
                status = "pass" if gate["passed"] else "fail"
                parts.append("<tr>")
                parts.append(f"<td>{esc(gate['name'])}</td><td>{esc(gate['actual'])}</td><td>{esc(gate['target'])}</td>")
                parts.append(f"<td><span class='pill {cls}'>{status}</span></td></tr>")
            parts.append("</tbody></table>")
        parts.append("<h3>Per-Example Results</h3>")
        parts.append("<table class='example-table'><thead><tr><th>ID</th><th>Input</th><th>Expected</th><th>Output</th><th class='num'>Sim</th><th class='num'>Latency</th><th>Validation</th></tr></thead><tbody>")
        for row in item["results"]:
            cls = status_class(row)
            hard_unsafe = row.get("hard_unsafe", row["validation_reasons"])
            review_flags = row.get("review_flags", [])
            safety = "hard: " + ", ".join(hard_unsafe) if hard_unsafe else "hard: ok"
            if review_flags:
                safety += "; review: " + ", ".join(review_flags)
            parts.append(f"<tr class='example-row' data-cat='{esc(row['category'])}' data-split='{esc(row['split'])}'>")
            parts.append(
                f"<td><b>{esc(row['id'])}</b><br><span class='pill'>{esc(row['category'])}</span><br>"
                f"<span class='subtle small'>{esc(row['split'])}</span></td>"
            )
            parts.append(f"<td>{esc(row['input'])}</td>")
            parts.append(f"<td>{esc(row['expected'])}</td>")
            parts.append(f"<td><div class='out'>{esc(row['output'])}</div>")
            if row["raw_output"] and row["raw_output"] != row["output"]:
                parts.append(f"<details><summary>raw</summary><div class='raw'>{esc(row['raw_output'])}</div></details>")
            parts.append("</td>")
            parts.append(f"<td class='num'><span class='pill {cls}'>{fmt(row['similarity'])}</span></td>")
            parts.append(f"<td class='num'>{row['latency_ms']} ms</td>")
            parts.append(f"<td>{esc(safety)}</td></tr>")
        parts.append("</tbody></table>")
        parts.append("</div></details>")

    if payload.get("optimizer_calls"):
        parts.append("<h2>Optimizer Calls</h2>")
        parts.append("<table><thead><tr><th>Round</th><th>Model</th><th class='num'>Latency</th><th>Usage</th><th>Cost</th></tr></thead><tbody>")
        for call in payload["optimizer_calls"]:
            parts.append("<tr>")
            parts.append(f"<td>{esc(call['round'])}</td><td>{esc(call['model'])}</td><td class='num'>{call['latency_ms']} ms</td>")
            parts.append(f"<td><div class='raw'>{esc(json.dumps(call.get('usage'), indent=2))}</div></td>")
            parts.append(f"<td><div class='raw'>{esc(json.dumps(call.get('cost'), indent=2))}</div></td>")
            parts.append("</tr>")
        parts.append("</tbody></table>")

    parts.append(
        """
<script>
const promptSearch = document.getElementById('promptSearch');
const roundFilter = document.getElementById('roundFilter');
function applyPromptFilters() {
  const q = (promptSearch.value || '').toLowerCase();
  const r = roundFilter.value;
  document.querySelectorAll('.prompt-row').forEach(row => {
    const okText = row.dataset.name.toLowerCase().includes(q);
    const okRound = r === 'all' || row.dataset.round === r;
    row.classList.toggle('hide', !(okText && okRound));
  });
}
promptSearch.addEventListener('input', applyPromptFilters);
roundFilter.addEventListener('change', applyPromptFilters);

const categoryFilter = document.getElementById('categoryFilter');
const splitFilter = document.getElementById('splitFilter');
function applyExampleFilters() {
  const cat = categoryFilter.value;
  const split = splitFilter.value;
  document.querySelectorAll('.example-row').forEach(row => {
    const okCat = cat === 'all' || row.dataset.cat === cat;
    const okSplit = split === 'all' || row.dataset.split === split;
    row.classList.toggle('hide', !(okCat && okSplit));
  });
}
categoryFilter.addEventListener('change', applyExampleFilters);
splitFilter.addEventListener('change', applyExampleFilters);
</script>
"""
    )
    parts.append("</main></body></html>")
    path.write_text("\n".join(parts))


def write_artifacts(payload: dict, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "all.json").write_text(json.dumps(payload, indent=2))
    for item in payload["prompt_results"]:
        safe_id = slugify(item["id"])
        (out_dir / f"{safe_id}.json").write_text(json.dumps(item, indent=2))
    generate_report(payload, out_dir / "report.html")


def configure_optimizer_args(args: argparse.Namespace) -> None:
    if args.optimizer_provider == "openai":
        args.optimizer_base_url = args.optimizer_base_url or "https://api.openai.com/v1"
        args.optimizer_model = args.optimizer_model or "gpt-5.5"
        args.optimizer_api_key_env = args.optimizer_api_key_env or "OPENAI_API_KEY"
        args.optimizer_endpoint = args.optimizer_endpoint or "responses"
    else:
        args.optimizer_base_url = args.optimizer_base_url or "https://api.neuralwatt.com/v1"
        args.optimizer_model = args.optimizer_model or "kimi-k2.6-fast"
        args.optimizer_api_key_env = args.optimizer_api_key_env or "NEURALWATT_API_KEY"
        args.optimizer_endpoint = args.optimizer_endpoint or "chat"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-model", default="gemma-4-e2b-it-q4", choices=sorted(MODELS))
    parser.add_argument("--optimizer-provider", choices=("openai", "openai-compatible"), default="openai")
    parser.add_argument("--optimizer-endpoint", choices=("responses", "chat"))
    parser.add_argument("--optimizer-base-url")
    parser.add_argument("--optimizer-model")
    parser.add_argument("--optimizer-api-key-env")
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--candidates-per-round", type=int, default=4)
    parser.add_argument("--repair-prompt-id")
    parser.add_argument("--max-prompt-words", type=int, default=85)
    parser.add_argument("--target-exact-pct", type=float, default=83.0)
    parser.add_argument("--target-mean-similarity", type=float, default=0.925)
    parser.add_argument(
        "--target-max-hard-unsafe-cases",
        "--target-max-unsafe-cases",
        dest="target_max_hard_unsafe_cases",
        type=int,
        default=5,
    )
    parser.add_argument("--target-max-p50-ms", type=int, default=220)
    parser.add_argument("--target-max-p95-ms", type=int, default=500)
    parser.add_argument("--target-max-few-shot", type=int, default=2)
    parser.add_argument("--no-regress-categories", nargs="*", default=["preserve_facts", "should_not_overedit"])
    parser.add_argument("--failure-limit", type=int, default=12)
    parser.add_argument("--history-limit", type=int, default=6)
    parser.add_argument("--split-seed", type=int, default=42)
    parser.add_argument("--out-dir", default=str(OUT_DIR))
    parser.add_argument("--ctx", type=int, default=2048)
    parser.add_argument("--gpu-layers", type=int, default=-1)
    parser.add_argument("--batch", type=int, default=512)
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--flash-attn", action="store_true")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--rewrite-temperature", type=float, default=0.1)
    parser.add_argument("--rewrite-top-k", type=int, default=50)
    parser.add_argument("--rewrite-top-p", type=float, default=0.95)
    parser.add_argument("--rewrite-repeat-penalty", type=float, default=1.05)
    parser.add_argument("--optimizer-temperature", type=float, default=0.4)
    parser.add_argument("--optimizer-top-p", type=float, default=0.95)
    parser.add_argument("--optimizer-max-tokens", type=int, default=4000)
    parser.add_argument("--optimizer-timeout-s", type=int, default=120)
    args = parser.parse_args()
    configure_optimizer_args(args)

    rows = split_rows(bench.load_dataset(str(HERE / "dataset.jsonl")), args.split_seed)
    out_dir = Path(args.out_dir)
    baseline_candidate = make_baseline_candidate()
    repair_candidate = load_repair_candidate(args.repair_prompt_id, out_dir) if args.repair_prompt_id else None
    seen_ids = {baseline_candidate["id"]}
    if repair_candidate:
        seen_ids.add(repair_candidate["id"])
    optimizer_calls: list[dict] = []
    prompt_results: list[dict] = []

    info = MODELS[args.target_model]
    path = download_model(args.target_model)
    print(f"loaded {len(rows)} examples")
    print(f"target model: {info['label']}")
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
    print(f"loaded Gemma in {load_ms} ms", flush=True)

    prompt_results.append(evaluate_candidate(llm, rows, baseline_candidate, args))
    if repair_candidate:
        prompt_results.append(evaluate_candidate(llm, rows, repair_candidate, args))
    attach_gates(prompt_results, args)

    stop_early = False
    for round_index in range(1, args.rounds + 1):
        payload = optimizer_payload(prompt_results, round_index, args)
        print(f"\n=== optimizer round {round_index}: {args.optimizer_model} ===", flush=True)
        raw_candidates, call_info = call_optimizer(payload, args)
        call_info["round"] = round_index
        optimizer_calls.append(call_info)
        candidates = normalize_candidates(raw_candidates, round_index, args, seen_ids)
        print(f"optimizer returned {len(candidates)} usable candidates", flush=True)
        if not candidates:
            break
        for candidate in candidates:
            prompt_results.append(evaluate_candidate(llm, rows, candidate, args))
            attach_gates(prompt_results, args)
            if prompt_results[-1]["gate_summary"]["all_passed"]:
                print(f"stopping early: {candidate['id']} passed all gates", flush=True)
                stop_early = True
                break
        if stop_early:
            break

    attach_gates(prompt_results, args)
    ranked = sorted(prompt_results, key=prompt_rank_key, reverse=True)
    payload = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S %Z"),
        "target_model": args.target_model,
        "target_label": info["label"],
        "target_repo": info["repo"],
        "target_filename": info["filename"],
        "target_runtime": "llama-cpp-python GGUF/Metal",
        "target_load_ms": load_ms,
        "optimizer_base_url": args.optimizer_base_url,
        "optimizer_provider": args.optimizer_provider,
        "optimizer_endpoint": args.optimizer_endpoint,
        "optimizer_model": args.optimizer_model,
        "rounds": args.rounds,
        "candidates_per_round": args.candidates_per_round,
        "max_prompt_words": args.max_prompt_words,
        "split_seed": args.split_seed,
        "target_gates": {
            "exact_pct_at_least": args.target_exact_pct,
            "mean_similarity_at_least": args.target_mean_similarity,
            "hard_unsafe_cases_at_most": args.target_max_hard_unsafe_cases,
            "p50_latency_ms_at_most": args.target_max_p50_ms,
            "p95_latency_ms_at_most": args.target_max_p95_ms,
            "prompt_words_at_most": args.max_prompt_words,
            "few_shot_count_at_most": args.target_max_few_shot,
            "no_regress_categories": args.no_regress_categories,
        },
        "objective_formula": (
            "mean_similarity*100 + exact_pct*0.15 - hard_unsafe_cases*1.75 "
            "- review_flag_cases*0.25 - chatty_cases*2.5 - prompt_words*0.025 "
            "- few_shot_count*0.5 - p95_ms/1000"
        ),
        "best_prompt_id": ranked[0]["id"],
        "prompt_results": prompt_results,
        "optimizer_calls": optimizer_calls,
    }
    write_artifacts(payload, out_dir)
    print(f"\nDONE -> {out_dir / 'report.html'}")
    print(f"best -> {ranked[0]['name']} ({ranked[0]['id']}), objective {ranked[0]['objective']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Test a small grammar-correction model for the pill's Improve button.

Runs a T5 grammar model (e.g. Unbabel/gec-t5_small) on a labeled case file,
applies the guard rails from docs/local-history-plan-2026-10-16.md §6.4, and
reports how many mistakes it fixed, how many clean sentences it changed,
speed, and memory.

    .venv-nemotron-mac/bin/python -I tools/improve/grammar_probe.py \\
        --model PATH/TO/gec-t5_small [--cases tools/improve/grammar_cases.jsonl] \\
        [--beams 5] [--json-out report.json]

The model folder holds config.json, tokenizer.json and pytorch_model.bin (or
model.safetensors). Weights are loaded tensors-only (`weights_only=True`), so
a downloaded checkpoint cannot run code.

Case file: one JSON object per line with "id", "group" ("error", "clean" or
"technical") and "input". Error cases also name the "error" substring and the
acceptable "fix_any" alternatives (both compared case-insensitively).
"""

from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import resource
import statistics
import subprocess
import time
from pathlib import Path

DEFAULT_CASES = Path(__file__).with_name("grammar_cases.jsonl")
PREFIX = "gec: "
MAX_CHANGED_FRACTION = 0.4
SENTENCE_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9\"'])")


def load_model(model_dir: Path):
    import torch
    from safetensors.torch import load_file
    from transformers import AutoTokenizer, T5Config, T5ForConditionalGeneration

    tokenizer = AutoTokenizer.from_pretrained(model_dir)
    model = T5ForConditionalGeneration(T5Config.from_pretrained(model_dir))
    safetensors = model_dir / "model.safetensors"
    if safetensors.exists():
        state = load_file(str(safetensors))
    else:
        state = torch.load(model_dir / "pytorch_model.bin", map_location="cpu", weights_only=True)
    missing, unexpected = model.load_state_dict(state, strict=False)
    model.tie_weights()
    tied = {"encoder.embed_tokens.weight", "decoder.embed_tokens.weight", "lm_head.weight"}
    if set(missing) - tied or unexpected:
        raise SystemExit(f"weights do not match the model: missing={missing} unexpected={unexpected}")
    model.eval()
    return tokenizer, model


def correct(tokenizer, model, text: str, beams: int) -> str:
    import torch

    sentences = SENTENCE_END.split(text.strip()) or [text]
    fixed = []
    with torch.inference_mode():
        for sentence in sentences:
            encoded = tokenizer(PREFIX + sentence, max_length=128, truncation=True, return_tensors="pt")
            output = model.generate(**encoded, max_length=128, num_beams=beams, early_stopping=beams > 1)
            fixed.append(tokenizer.decode(output[0], skip_special_tokens=True, clean_up_tokenization_spaces=True))
    return " ".join(fixed)


def protected_tokens(text: str) -> list[str]:
    """Words a grammar model must not touch: code, file names, links, emails,
    versions and brand-style names (anything with digits, symbols, a leading
    dash, or a capital after the first letter: agents.md, --workspace, macOS)."""
    tokens = []
    for raw in text.split():
        token = raw.strip(".,!?;:\"'()")
        if token and (
            re.search(r"[\d./@:_\\#$%&*=+<>\[\]{}|~]", token)
            or token.startswith("-")
            or any(char.isupper() for char in token[1:])
        ):
            tokens.append(token)
    return tokens


def guard_rail(before: str, after: str) -> str | None:
    """Why the result must not replace the text, or None when it may."""
    if re.findall(r"\d+", before) != re.findall(r"\d+", after):
        return "numbers changed"
    if protected_tokens(before) != protected_tokens(after):
        return "code, name or link changed"
    words_before, words_after = before.lower().split(), after.lower().split()
    matcher = difflib.SequenceMatcher(a=words_before, b=words_after)
    kept = sum(block.size for block in matcher.get_matching_blocks())
    if words_before and 1 - kept / len(words_before) > MAX_CHANGED_FRACTION:
        return "too much rewritten"
    return None


def footprint_mb(pid: int) -> float | None:
    """macOS phys_footprint, the number Activity Monitor shows as Memory."""
    try:
        out = subprocess.run(["footprint", str(pid)], capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    match = re.search(r"Footprint:\s*([\d.]+)\s*(KB|MB|GB)", out)
    if not match:
        return None
    value, unit = float(match.group(1)), match.group(2)
    return value * {"KB": 1 / 1024, "MB": 1, "GB": 1024}[unit]


def grade(case: dict, output: str, rejected: str | None) -> str:
    unchanged = output.strip() == case["input"].strip()
    if case["group"] == "error":
        lowered = output.lower()
        fixed = case["error"].lower() not in lowered and any(fix.lower() in lowered for fix in case["fix_any"])
        if fixed and rejected:
            return "fixed but blocked"
        return "fixed" if fixed else ("unchanged" if unchanged else "changed, not fixed")
    if unchanged:
        return "unchanged"
    return "blocked" if rejected else "changed"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--cases", type=Path, default=DEFAULT_CASES)
    parser.add_argument("--beams", type=int, default=5)
    parser.add_argument("--json-out", type=Path)
    args = parser.parse_args()

    cases = [json.loads(line) for line in args.cases.read_text().splitlines() if line.strip()]
    started = time.perf_counter()
    import torch  # noqa: F401  (import cost is part of a cold start)
    import transformers  # noqa: F401

    imported = time.perf_counter()
    tokenizer, model = load_model(args.model)
    loaded = time.perf_counter()
    memory_loaded = footprint_mb(os.getpid())

    results = []
    for case in cases:
        t0 = time.perf_counter()
        output = correct(tokenizer, model, case["input"], args.beams)
        ms = (time.perf_counter() - t0) * 1000
        rejected = None if output.strip() == case["input"].strip() else guard_rail(case["input"], output)
        verdict = grade(case, output, rejected)
        results.append({**case, "output": output, "ms": round(ms, 1), "guard_rail": rejected, "verdict": verdict})
        print(f"[{case['group']:9}] {verdict:18} {ms:6.0f} ms  {case['input']!r}\n{'':40}-> {output!r}"
              + (f"  ({rejected})" if rejected else ""), flush=True)

    memory_end = footprint_mb(os.getpid())
    peak_rss_mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (1024 * 1024)
    latencies = [r["ms"] for r in results]
    warm = latencies[1:] or latencies
    summary = {
        "model": str(args.model),
        "beams": args.beams,
        "cases": len(results),
        "import_s": round(imported - started, 2),
        "load_s": round(loaded - imported, 2),
        "first_ms": latencies[0] if latencies else None,
        "warm_p50_ms": round(statistics.median(warm), 1) if warm else None,
        "warm_p95_ms": round(sorted(warm)[max(0, int(len(warm) * 0.95) - 1)], 1) if warm else None,
        "footprint_loaded_mb": memory_loaded,
        "footprint_end_mb": memory_end,
        "peak_rss_mb": round(peak_rss_mb, 1),
        "by_group": {},
    }
    for group in ("error", "clean", "technical"):
        verdicts = [r["verdict"] for r in results if r["group"] == group]
        summary["by_group"][group] = {v: verdicts.count(v) for v in sorted(set(verdicts))} | {"total": len(verdicts)}
    print("\n" + json.dumps(summary, indent=2))
    if args.json_out:
        args.json_out.write_text(json.dumps({"summary": summary, "results": results}, indent=2) + "\n")


if __name__ == "__main__":
    main()

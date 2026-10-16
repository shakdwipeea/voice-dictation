"""Check the MLX grammar helper against the PyTorch reference.

Runs services/improve/grammar_sidecar.py's Corrector (MLX, no PyTorch) over
the cases of a grammar_probe.py report made with --beams 1, and reports
output mismatches, speed, and memory.

    .venv-nemotron-mac/bin/python -I tools/improve/grammar_mlx_check.py \\
        --model PATH/TO/gec-t5_small-safetensors --reference report-beam1.json \\
        [--dtype bfloat16|float32]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "services" / "improve"))
import grammar_sidecar  # noqa: E402


def footprint_mb() -> float | None:
    out = subprocess.run(["footprint", str(os.getpid())], capture_output=True, text=True).stdout
    match = re.search(r"Footprint:\s*([\d.]+)\s*(KB|MB|GB)", out)
    if not match:
        return None
    return float(match.group(1)) * {"KB": 1 / 1024, "MB": 1, "GB": 1024}[match.group(2)]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--dtype", choices=("bfloat16", "float32"), default="bfloat16")
    args = parser.parse_args()

    reference = json.loads(args.reference.read_text())["results"]
    before = footprint_mb()
    started = time.perf_counter()
    corrector = grammar_sidecar.Corrector(args.model, args.dtype)
    load_s = time.perf_counter() - started
    loaded = footprint_mb()

    mismatches, latencies = [], []
    for case in reference:
        t0 = time.perf_counter()
        output = corrector.correct(case["input"])
        latencies.append((time.perf_counter() - t0) * 1000)
        if output != case["output"]:
            mismatches.append((case["id"], case["input"], case["output"], output))
    end = footprint_mb()

    for case_id, text, expected, got in mismatches:
        print(f"{case_id}: {text!r}\n   torch: {expected!r}\n   mlx:   {got!r}")
    warm = latencies[1:] or latencies
    print(json.dumps({
        "dtype": args.dtype,
        "cases": len(reference),
        "identical_to_torch": len(reference) - len(mismatches),
        "load_s": round(load_s, 2),
        "first_ms": round(latencies[0], 1),
        "warm_p50_ms": round(statistics.median(warm), 1),
        "warm_p95_ms": round(sorted(warm)[max(0, int(len(warm) * 0.95) - 1)], 1),
        "footprint_python_mb": before,
        "footprint_loaded_mb": loaded,
        "footprint_end_mb": end,
    }, indent=2))


if __name__ == "__main__":
    main()

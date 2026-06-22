#!/usr/bin/env python3
"""Phase 0 benchmark for parakeet-mlx on macOS/Apple Silicon.

This is the go/no-go spike for replacing the current macOS NeMo CPU backend with
an MLX-backed Parakeet backend. It measures:

  - import time,
  - from_pretrained() load/download time,
  - 1s warm-up transcription time,
  - per-clip release-to-final latency, RTF, and WER on the same tiny
    LibriSpeech sample set used by services/asr/phase0_macos_measure.py.

Important runtime rule: run this while the daemon/Nemotron sidecar is stopped.
Do not load a second ASR model in parallel.

Usage:
  .venv-nemotron-mac/bin/python tools/phase0/parakeet_mlx_measure.py \
      --limit 5 --output /tmp/sunoto-parakeet-bench.json

Default model is the benchmark-selected MLX-converted v3 checkpoint. parakeet-mlx
does not load raw NeMo checkpoints such as `nvidia/parakeet-tdt-0.6b-v2`
directly.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import sys
import time
from pathlib import Path

# Reuse the existing macOS phase0 data prep and WER helpers so Nemotron and
# Parakeet benchmarks are comparable.
_REPO_ROOT = Path(__file__).resolve().parents[2]
_SERVICE_ASR = _REPO_ROOT / "services" / "asr"
if str(_SERVICE_ASR) not in sys.path:
    sys.path.insert(0, str(_SERVICE_ASR))

from phase0_macos_measure import (  # noqa: E402
    DEFAULT_CACHE_DIR,
    DEFAULT_DATASET,
    DEFAULT_DATASET_CONFIG,
    DEFAULT_SPLIT,
    prepare_hf_librispeech_samples,
    prepare_tone_samples,
    percentile,
    synth_tone_wav,
    word_error_rate,
)

DEFAULT_MODEL = "mlx-community/parakeet-tdt-0.6b-v3"
DEFAULT_CACHE_DIR = Path("build/phase0/parakeet-mlx-real-speech")


def log(message: str) -> None:
    print(f"[phase0-parakeet-mlx] {message}", file=sys.stderr, flush=True)


def peak_rss_mib() -> float:
    """Return peak RSS in MiB. On macOS ru_maxrss is bytes; on Linux it is KiB."""
    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return round(value / (1024 * 1024), 1)
    return round(value / 1024, 1)


def load_engine(model_name: str, *, fp32: bool, cache_dir: str | None):
    timings: dict = {}

    t0 = time.perf_counter()
    log("importing parakeet_mlx ...")
    from parakeet_mlx import from_pretrained  # noqa: delayed heavy import

    timings["import_s"] = round(time.perf_counter() - t0, 3)

    kwargs = {}
    if cache_dir:
        kwargs["cache_dir"] = cache_dir
    # parakeet-mlx defaults to bf16; it exposes fp32 primarily through the CLI.
    # Some versions accept fp32 in from_pretrained, some do not. Try the explicit
    # kwarg only when requested and fall back cleanly if the API does not accept it.
    if fp32:
        kwargs["fp32"] = True

    log(f"loading {model_name} via MLX ...")
    t = time.perf_counter()
    try:
        model = from_pretrained(model_name, **kwargs)
    except TypeError:
        if "fp32" in kwargs:
            log("from_pretrained() did not accept fp32=; retrying with library default precision")
            kwargs.pop("fp32", None)
            model = from_pretrained(model_name, **kwargs)
        else:
            raise
    timings["load_s"] = round(time.perf_counter() - t, 3)
    timings["precision_requested"] = "fp32" if fp32 else "bf16_default"

    t = time.perf_counter()
    warm_path = synth_tone_wav(1.0)
    try:
        log("warming up with 1s tone ...")
        model.transcribe(warm_path)
    finally:
        os.unlink(warm_path)
    timings["warmup_s"] = round(time.perf_counter() - t, 3)
    timings["peak_rss_mib_after_warmup"] = peak_rss_mib()
    return model, timings


def transcribe_one(model, path: str, *, chunk_duration: float | None, overlap_duration: float):
    kwargs = {}
    if chunk_duration is not None:
        kwargs["chunk_duration"] = chunk_duration
        kwargs["overlap_duration"] = overlap_duration
    result = model.transcribe(path, **kwargs)
    text = getattr(result, "text", result)
    return text if isinstance(text, str) else ""


def measure(model, samples, *, chunk_duration: float | None, overlap_duration: float):
    results = []
    for sample in samples:
        path = sample["path"]
        try:
            t = time.perf_counter()
            text = transcribe_one(
                model,
                path,
                chunk_duration=chunk_duration,
                overlap_duration=overlap_duration,
            )
            latency = time.perf_counter() - t
        finally:
            if sample.get("delete_after"):
                os.unlink(path)

        seconds = float(sample["duration_seconds"])
        rtf = latency / seconds if seconds > 0 else float("inf")
        speed_x = seconds / latency if latency > 0 else float("inf")
        wer = word_error_rate(sample.get("reference", ""), text or "")
        record = {
            **sample,
            "latency_s": round(latency, 3),
            "rtf": round(rtf, 3),
            "speed_x_realtime": round(speed_x, 2),
            "text": text or "",
            "text_preview": (text or "")[:120],
            "peak_rss_mib": peak_rss_mib(),
        }
        if wer is not None:
            record["wer"] = round(wer, 4)
        results.append(record)
        log(
            f"{sample['id']}: {seconds:.2f}s audio latency={latency:.3f}s "
            f"rtf={rtf:.3f} speed={speed_x:.1f}x text={record['text_preview']!r}"
        )
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument(
        "--source",
        choices=["hf-librispeech", "tone"],
        default="hf-librispeech",
        help="real speech from Hugging Face (default) or synthetic tone smoke test",
    )
    parser.add_argument("--dataset", default=DEFAULT_DATASET)
    parser.add_argument("--dataset-config", default=DEFAULT_DATASET_CONFIG)
    parser.add_argument("--split", default=DEFAULT_SPLIT)
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--cache-dir", type=Path, default=DEFAULT_CACHE_DIR)
    parser.add_argument(
        "--hf-cache-dir",
        default=None,
        help="optional Hugging Face model cache directory for from_pretrained()",
    )
    parser.add_argument("--seconds", type=float, nargs="+", default=[1.0, 3.0, 8.0])
    parser.add_argument("--chunk-duration", type=float, default=None)
    parser.add_argument("--overlap-duration", type=float, default=15.0)
    parser.add_argument("--fp32", action="store_true", help="request fp32 if supported")
    parser.add_argument("--output", help="write JSON summary to this path")
    args = parser.parse_args(argv)

    if args.limit < 1:
        raise ValueError("--limit must be at least 1")

    if args.source == "tone":
        samples = prepare_tone_samples(args.seconds)
    else:
        samples = prepare_hf_librispeech_samples(
            args.dataset,
            args.dataset_config,
            args.split,
            args.limit,
            args.cache_dir,
        )

    model, timings = load_engine(args.model, fp32=args.fp32, cache_dir=args.hf_cache_dir)
    results = measure(
        model,
        samples,
        chunk_duration=args.chunk_duration,
        overlap_duration=args.overlap_duration,
    )
    latencies = [item["latency_s"] for item in results]
    rtfs = [item["rtf"] for item in results]
    speeds = [item["speed_x_realtime"] for item in results]
    wers = [item["wer"] for item in results if "wer" in item]

    summary = {
        "model": args.model,
        "backend": "parakeet_mlx",
        "platform": {
            "system": platform.system(),
            "machine": platform.machine(),
            "platform": platform.platform(),
            "python": sys.version.split()[0],
        },
        "source": args.source,
        "dataset": args.dataset if args.source == "hf-librispeech" else None,
        "dataset_config": args.dataset_config if args.source == "hf-librispeech" else None,
        "split": args.split if args.source == "hf-librispeech" else None,
        "chunk_duration": args.chunk_duration,
        "overlap_duration": args.overlap_duration if args.chunk_duration else None,
        "timings": timings,
        "clips": results,
        "latency_summary": {
            "count": len(latencies),
            "p50_s": percentile(latencies, 0.50),
            "p95_s": percentile(latencies, 0.95),
            "max_s": round(max(latencies), 3) if latencies else None,
            "rtf_p50": percentile(rtfs, 0.50),
            "speed_x_p50": percentile(speeds, 0.50),
            "wer_mean": round(sum(wers) / len(wers), 4) if wers else None,
            "peak_rss_mib_final": peak_rss_mib(),
        },
        "notes": [
            "Run with the daemon and Nemotron sidecar stopped; do not load two ASR models in parallel.",
            "Default model is benchmark-selected MLX v3; raw nvidia/parakeet-tdt checkpoints are not the parakeet-mlx target.",
            "RTF below 1.0 means faster than real time; speed_x is audio seconds per wall-clock second.",
        ],
    }
    text = json.dumps(summary, indent=2)
    if args.output:
        with open(args.output, "w") as fh:
            fh.write(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""macOS ASR feasibility and real-speech latency measurement.

Loads nvidia/nemotron-speech-streaming-en-0.6b onto the requested device
(cpu by default, mps optional) and measures:

  - warm-load time (model import + from_pretrained + warm-up transcribe),
  - per-clip release-to-final latency and real-time factor (RTF),
  - which device actually ended up running (requested vs CPU fallback).

This is the macOS ASR gate for the port (see docs/macos-port-plan.md). No
GPU/CUDA is involved; it is CPU/MPS only. The default benchmark downloads a
tiny real-speech LibriSpeech sample from Hugging Face and exports cached
16 kHz mono WAVs. A synthetic tone mode remains available only as a runtime
smoke test; it is not representative of real ASR latency because it emits no
words.

Usage:
  .venv-nemotron-mac/bin/python services/asr/phase0_macos_measure.py \
      [--device cpu|mps] [--model MODEL] [--limit 5]

Writes a JSON summary to stdout (and a path with --output).
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import sys
import tempfile
import time
import wave
from array import array
from pathlib import Path


SAMPLE_RATE = 16_000
DEFAULT_DATASET = "hf-internal-testing/librispeech_asr_dummy"
DEFAULT_DATASET_CONFIG = "clean"
DEFAULT_SPLIT = "validation"
DEFAULT_CACHE_DIR = Path("build/phase0/macos-real-speech")


def log(message: str) -> None:
    print(f"[phase0-macos] {message}", file=sys.stderr, flush=True)


def synth_tone_wav(seconds: float, freq: float = 220.0) -> str:
    """Synthesize `seconds` of a soft sine tone as 16 kHz mono s16le WAV."""
    import math

    n = int(SAMPLE_RATE * seconds)
    samples = array("h")
    for i in range(n):
        env = min(1.0, i / 800, (n - i) / 800)
        value = int(0.2 * env * 32767 * math.sin(2 * math.pi * freq * i / SAMPLE_RATE))
        samples.append(value)
    fd, path = tempfile.mkstemp(suffix=".wav", prefix="sunoto-phase0-")
    with os.fdopen(fd, "wb") as fh:
        with wave.open(fh, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(SAMPLE_RATE)
            wav.writeframes(samples.tobytes())
    return path


def load_engine(model_name: str, device: str, use_lhotse: bool, batch_size: int):
    """Import torch + NeMo lazily, load the model, warm it up."""
    timings: dict = {}

    t0 = time.perf_counter()
    log("importing torch + NeMo ...")
    import torch  # noqa: delayed heavy import
    import nemo.collections.asr as nemo_asr

    timings["import_s"] = round(time.perf_counter() - t0, 2)
    torch.set_grad_enabled(False)
    try:
        torch.set_float32_matmul_precision("high")
    except Exception:
        pass

    def _load(dev_name: str):
        log(f"loading {model_name} onto {dev_name} ...")
        t = time.perf_counter()
        dev = torch.device(dev_name)
        model = nemo_asr.models.ASRModel.from_pretrained(model_name, map_location=dev)
        model.eval()
        load_s = time.perf_counter() - t
        log(f"loaded onto {dev_name} in {load_s:.1f}s")
        return model, dev, load_s

    actual = device
    try:
        model, _dev, load_s = _load(device)
        timings["load_s"] = round(load_s, 2)
    except Exception as error:
        if device != "cpu":
            log(f"load on {device} failed ({error!r}); falling back to CPU")
            actual = "cpu"
            model, _dev, load_s = _load("cpu")
            timings["load_s"] = round(load_s, 2)
            timings["mps_load_error"] = repr(error)[:500]
        else:
            raise

    t = time.perf_counter()
    warm_path = synth_tone_wav(1.0)
    try:
        try:
            model.transcribe(
                [warm_path],
                return_hypotheses=False,
                use_lhotse=use_lhotse,
                batch_size=batch_size,
                num_workers=0,
                verbose=False,
            )
        except Exception as error:
            if actual != "cpu":
                log(f"warm transcribe on {actual} failed ({error!r}); reloading on CPU")
                timings["mps_run_error"] = repr(error)[:500]
                model, _dev, load_s = _load("cpu")
                timings["load_s"] = round(load_s, 2)
                actual = "cpu"
                model.transcribe(
                    [warm_path],
                    return_hypotheses=False,
                    use_lhotse=use_lhotse,
                    batch_size=batch_size,
                    num_workers=0,
                    verbose=False,
                )
            else:
                raise
    finally:
        os.unlink(warm_path)
    timings["warmup_s"] = round(time.perf_counter() - t, 2)
    timings["requested_device"] = device
    timings["actual_device"] = actual
    return model, actual, timings


def _resample_linear(samples, source_rate: int, target_rate: int = SAMPLE_RATE):
    import numpy as np

    samples = np.asarray(samples, dtype=np.float32)
    if source_rate == target_rate or samples.size == 0:
        return samples
    duration = samples.size / float(source_rate)
    target_len = max(1, int(round(duration * target_rate)))
    old_t = np.arange(samples.size, dtype=np.float64) / float(source_rate)
    new_t = np.arange(target_len, dtype=np.float64) / float(target_rate)
    return np.interp(new_t, old_t, samples).astype(np.float32)


def _write_float_wav(path: Path, samples) -> float:
    import numpy as np

    path.parent.mkdir(parents=True, exist_ok=True)
    samples = np.asarray(samples, dtype=np.float32)
    samples = np.clip(samples, -1.0, 1.0)
    pcm = (samples * 32767.0).astype("<i2")
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(pcm.tobytes())
    return len(samples) / SAMPLE_RATE


def _read_audio_payload(audio):
    """Return (float32 mono samples, sample_rate) from a datasets Audio value."""
    import numpy as np
    import soundfile as sf

    if isinstance(audio, dict):
        if audio.get("array") is not None:
            samples = np.asarray(audio["array"], dtype=np.float32)
            sample_rate = int(audio["sampling_rate"])
        elif audio.get("bytes") is not None:
            samples, sample_rate = sf.read(io.BytesIO(audio["bytes"]), dtype="float32")
        elif audio.get("path"):
            samples, sample_rate = sf.read(audio["path"], dtype="float32")
        else:
            raise ValueError("audio payload has no array, bytes, or path")
    elif isinstance(audio, (str, os.PathLike)):
        samples, sample_rate = sf.read(audio, dtype="float32")
    else:
        raise TypeError(f"unsupported audio payload: {type(audio).__name__}")

    samples = np.asarray(samples, dtype=np.float32)
    if samples.ndim == 2:
        samples = samples.mean(axis=1)
    return _resample_linear(samples, int(sample_rate)), SAMPLE_RATE


def _reference_text(row: dict) -> str:
    for key in ("text", "sentence", "transcript", "transcription"):
        value = row.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


def prepare_hf_librispeech_samples(
    dataset_name: str,
    dataset_config: str,
    split: str,
    limit: int,
    cache_dir: Path,
) -> list[dict]:
    """Download/export a tiny real-speech benchmark set from Hugging Face."""
    from datasets import Audio, load_dataset

    log(
        f"loading Hugging Face dataset {dataset_name} "
        f"config={dataset_config!r} split={split!r}"
    )
    dataset = load_dataset(dataset_name, dataset_config, split=split)
    dataset = dataset.cast_column("audio", Audio(decode=False))
    samples = []
    for index, row in enumerate(dataset):
        if index >= limit:
            break
        audio, sample_rate = _read_audio_payload(row["audio"])
        path = cache_dir / f"hf-librispeech-{index:03d}.wav"
        duration = _write_float_wav(path, audio)
        samples.append(
            {
                "id": f"hf-librispeech-{index:03d}",
                "path": str(path),
                "duration_seconds": round(duration, 4),
                "sample_rate_hz": sample_rate,
                "reference": _reference_text(row),
                "source": dataset_name,
            }
        )
    if not samples:
        raise RuntimeError("dataset produced no audio samples")
    log(f"prepared {len(samples)} real-speech WAVs in {cache_dir}")
    return samples


def prepare_tone_samples(seconds_list) -> list[dict]:
    samples = []
    for index, seconds in enumerate(seconds_list):
        samples.append(
            {
                "id": f"tone-{index:03d}",
                "path": synth_tone_wav(seconds),
                "duration_seconds": float(seconds),
                "sample_rate_hz": SAMPLE_RATE,
                "reference": "",
                "source": "synthetic_tone",
                "delete_after": True,
            }
        )
    return samples


def _normalize_words(text: str) -> list[str]:
    text = text.lower()
    text = re.sub(r"[^a-z0-9' ]+", " ", text)
    return [word for word in text.split() if word]


def _edit_distance(a: list[str], b: list[str]) -> int:
    previous = list(range(len(b) + 1))
    for i, left in enumerate(a, start=1):
        current = [i]
        for j, right in enumerate(b, start=1):
            current.append(
                min(
                    previous[j] + 1,
                    current[j - 1] + 1,
                    previous[j - 1] + (left != right),
                )
            )
        previous = current
    return previous[-1]


def word_error_rate(reference: str, hypothesis: str) -> float | None:
    ref_words = _normalize_words(reference)
    if not ref_words:
        return None
    hyp_words = _normalize_words(hypothesis)
    return _edit_distance(ref_words, hyp_words) / len(ref_words)


def percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * p)))
    return round(ordered[index], 3)


def measure(model, samples, use_lhotse: bool, batch_size: int):
    results = []
    for sample in samples:
        path = sample["path"]
        try:
            t = time.perf_counter()
            out = model.transcribe(
                [path],
                return_hypotheses=False,
                use_lhotse=use_lhotse,
                batch_size=batch_size,
                num_workers=0,
                verbose=False,
            )
            latency = time.perf_counter() - t
            text = out[0] if out else ""
            if not isinstance(text, str):
                text = getattr(text, "text", "")
        finally:
            if sample.get("delete_after"):
                os.unlink(path)
        seconds = float(sample["duration_seconds"])
        rtf = latency / seconds if seconds > 0 else float("inf")
        wer = word_error_rate(sample.get("reference", ""), text or "")
        record = {
            **sample,
            "latency_s": round(latency, 3),
            "rtf": round(rtf, 3),
            "text": text or "",
            "text_preview": (text or "")[:80],
        }
        if wer is not None:
            record["wer"] = round(wer, 4)
        results.append(record)
        log(
            f"{sample['id']}: {seconds:.2f}s audio latency={latency:.3f}s "
            f"rtf={rtf:.3f} text={record['text_preview']!r}"
        )
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="nvidia/nemotron-speech-streaming-en-0.6b")
    parser.add_argument("--device", default="cpu", help="cpu (default) or mps")
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
    parser.add_argument("--batch-size", type=int, default=1)
    parser.add_argument("--use-lhotse", action="store_true")
    parser.add_argument("--seconds", type=float, nargs="+", default=[1.0, 3.0, 8.0])
    parser.add_argument("--output", help="write JSON summary to this path")
    args = parser.parse_args(argv)

    if args.limit < 1:
        raise ValueError("--limit must be at least 1")
    if args.batch_size < 1:
        raise ValueError("--batch-size must be at least 1")

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

    model, actual, timings = load_engine(
        args.model, args.device, args.use_lhotse, args.batch_size
    )
    results = measure(model, samples, args.use_lhotse, args.batch_size)
    latencies = [item["latency_s"] for item in results]
    rtfs = [item["rtf"] for item in results]
    wers = [item["wer"] for item in results if "wer" in item]

    summary = {
        "model": args.model,
        "requested_device": args.device,
        "actual_device": actual,
        "source": args.source,
        "dataset": args.dataset if args.source == "hf-librispeech" else None,
        "dataset_config": args.dataset_config if args.source == "hf-librispeech" else None,
        "split": args.split if args.source == "hf-librispeech" else None,
        "use_lhotse": args.use_lhotse,
        "batch_size": args.batch_size,
        "timings": timings,
        "clips": results,
        "latency_summary": {
            "count": len(latencies),
            "p50_s": percentile(latencies, 0.50),
            "p95_s": percentile(latencies, 0.95),
            "max_s": round(max(latencies), 3) if latencies else None,
            "rtf_p50": percentile(rtfs, 0.50),
            "wer_mean": round(sum(wers) / len(wers), 4) if wers else None,
        },
        "notes": [
            "Real-speech measurements are the macOS go/no-go signal.",
            "Tone WAVs are only a runtime smoke test; empty transcripts make them misleading for latency.",
            "warm-load includes import + from_pretrained + a 1s warm-up transcribe.",
            "release-to-final in the offline sidecar ~= warm-load(skip) + per-clip latency.",
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

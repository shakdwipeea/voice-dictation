#!/usr/bin/env python3
"""Capture and validate a short 16 kHz mono microphone sample."""

from __future__ import annotations

import argparse
import array
import json
import math
import signal
import subprocess
import time
import wave
from pathlib import Path


def resolve_device(device: str) -> str:
    if device != "auto":
        return device
    default = subprocess.run(
        ["pactl", "get-default-source"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if default and not default.endswith(".monitor"):
        return default

    sources = subprocess.run(
        ["pactl", "list", "short", "sources"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    for source in sources:
        fields = source.split("\t")
        if len(fields) >= 2 and not fields[1].endswith(".monitor"):
            return fields[1]
    raise RuntimeError("No physical microphone source is available")


def capture(output: Path, duration_seconds: float, device: str) -> dict[str, object]:
    device = resolve_device(device)
    output.parent.mkdir(parents=True, exist_ok=True)
    command = [
        "parec",
        "--record",
        f"--device={device}",
        "--rate=16000",
        "--format=s16le",
        "--channels=1",
        "--latency-msec=20",
        "--process-time-msec=20",
        "--file-format=wav",
        str(output),
    ]
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    time.sleep(duration_seconds)
    process.send_signal(signal.SIGINT)
    _, stderr = process.communicate(timeout=5)
    if process.returncode not in (0, -signal.SIGINT):
        raise RuntimeError(
            f"parec failed with exit code {process.returncode}: "
            f"{stderr.decode(errors='replace').strip()}"
        )

    with wave.open(str(output), "rb") as wav:
        frames = wav.readframes(wav.getnframes())
        metadata = {
            "sample_rate_hz": wav.getframerate(),
            "channels": wav.getnchannels(),
            "sample_width_bytes": wav.getsampwidth(),
            "frames": wav.getnframes(),
            "duration_seconds": round(wav.getnframes() / wav.getframerate(), 4),
        }
    samples = array.array("h")
    samples.frombytes(frames)
    mean_square = sum(sample * sample for sample in samples) / max(len(samples), 1)
    metadata["rms"] = round(math.sqrt(mean_square), 2)
    metadata["non_silent"] = metadata["rms"] > 10
    metadata["path"] = str(output.resolve())
    metadata["device"] = device
    metadata["valid_for_nemotron"] = (
        metadata["sample_rate_hz"] == 16_000
        and metadata["channels"] == 1
        and metadata["sample_width_bytes"] == 2
        and metadata["frames"] > 0
    )
    return metadata


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--duration", type=float, default=1.0)
    parser.add_argument(
        "--device",
        default="auto",
        help="PulseAudio source name, or 'auto' to prefer a physical microphone",
    )
    args = parser.parse_args()

    try:
        result = capture(args.output, args.duration, args.device)
    except (FileNotFoundError, RuntimeError, subprocess.TimeoutExpired, wave.Error) as error:
        print(json.dumps({"ready": False, "error": str(error)}, indent=2))
        return 1

    result["ready"] = bool(result["valid_for_nemotron"])
    payload = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(payload, encoding="utf-8")
    print(payload, end="")
    return 0 if result["ready"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

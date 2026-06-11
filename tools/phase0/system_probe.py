#!/usr/bin/env python3
"""Collect Phase 0 system, audio, GPU, and X11 feasibility diagnostics."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import time
from pathlib import Path
from typing import Sequence


def run(command: Sequence[str], timeout: int = 10) -> dict[str, object]:
    started = time.perf_counter()
    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return {
            "command": list(command),
            "returncode": result.returncode,
            "stdout": result.stdout.strip(),
            "stderr": result.stderr.strip(),
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 2),
        }
    except (FileNotFoundError, subprocess.TimeoutExpired) as error:
        return {
            "command": list(command),
            "returncode": None,
            "error": str(error),
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 2),
        }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--x11-probe", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    commands = {
        name: shutil.which(name)
        for name in (
            "arecord",
            "ffmpeg",
            "nvidia-smi",
            "pactl",
            "parec",
            "pw-record",
        )
    }
    probes: dict[str, object] = {
        "nvidia_smi": run(
            [
                "nvidia-smi",
                "--query-gpu=name,memory.total,driver_version",
                "--format=csv,noheader",
            ]
        ),
        "audio_sources": run(["pactl", "list", "short", "sources"]),
    }
    if args.x11_probe:
        probes["x11"] = run([str(args.x11_probe), "check"])

    result = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "environment": {
            name: os.environ.get(name)
            for name in ("DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP")
        },
        "commands": commands,
        "probes": probes,
    }
    payload = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(payload, encoding="utf-8")
    print(payload, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Interactively record the Phase 2 evaluation corpus.

Each case from the scripted corpus is read aloud by a human (speak the `raw`
text naturally, fillers and corrections included). Recordings land next to a
manifest that `tools/phase2/transcribe_corpus.py` completes by running the
audio through the ASR sidecar; the result feeds `sunoto-daemon eval`.

Push-to-talk is simulated with the Enter key: press Enter to start recording,
press Enter again to stop.
"""

from __future__ import annotations

import argparse
import array
import json
import math
import signal
import subprocess
import sys
import time
import wave
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "phase0"))
from audio_probe import resolve_device  # noqa: E402  (shared monitor-rejecting lookup)

MIN_RMS = 10.0


def record_wav(output: Path, device: str) -> None:
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
    # parec needs a moment to connect before frames flow.
    time.sleep(0.25)
    try:
        input("  recording... press Enter to stop ")
    finally:
        process.send_signal(signal.SIGINT)
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise


def wav_stats(path: Path) -> tuple[float, float]:
    with wave.open(str(path), "rb") as wav:
        if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) != (16_000, 1, 2):
            raise RuntimeError(f"{path} is not 16 kHz mono s16le")
        frames = wav.readframes(wav.getnframes())
        duration = wav.getnframes() / wav.getframerate()
    samples = array.array("h")
    samples.frombytes(frames)
    mean_square = sum(s * s for s in samples) / max(len(samples), 1)
    return duration, math.sqrt(mean_square)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--corpus",
        type=Path,
        default=Path("tests/corpus/phase2-text-cases.json"),
        help="scripted corpus whose cases provide the speaking prompts",
    )
    parser.add_argument(
        "--outdir",
        type=Path,
        default=Path("tests/corpus/phase2-recorded"),
        help="directory for recordings and the manifest",
    )
    parser.add_argument("--device", default="auto")
    parser.add_argument(
        "--force", action="store_true", help="re-record cases that already have audio"
    )
    parser.add_argument("--only", help="record just this case id")
    args = parser.parse_args()

    source = json.loads(args.corpus.read_text(encoding="utf-8"))
    device = resolve_device(args.device)
    print(f"microphone: {device}")
    args.outdir.mkdir(parents=True, exist_ok=True)

    manifest_path = args.outdir / "manifest.json"
    manifest = {
        "description": (
            "Recorded Phase 2 evaluation corpus. `raw` fields are filled by "
            "tools/phase2/transcribe_corpus.py from the audio files."
        ),
        "kind": "recorded",
        "dictionary": source.get("dictionary", []),
        "snippets": source.get("snippets", []),
        "cases": [],
    }
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    def manifest_case(case_id: str) -> dict:
        for case in manifest["cases"]:
            if case["id"] == case_id:
                return case
        case = {"id": case_id, "audio": f"{case_id}.wav", "raw": "", "expected": "", "tags": []}
        manifest["cases"].append(case)
        return case

    recorded = 0
    for case in source["cases"]:
        case_id = case["id"]
        if args.only and case_id != args.only:
            continue
        wav_path = args.outdir / f"{case_id}.wav"
        if wav_path.exists() and not args.force:
            print(f"[skip] {case_id}: {wav_path} exists (use --force to re-record)")
            entry = manifest_case(case_id)
            entry["expected"] = case["expected"]
            entry["tags"] = case.get("tags", [])
            continue
        print(f"\n=== {case_id} ===")
        print(f'  say: "{case["raw"]}"')
        while True:
            input("  press Enter to start recording ")
            record_wav(wav_path, device)
            try:
                duration, rms = wav_stats(wav_path)
            except (RuntimeError, wave.Error) as error:
                print(f"  recording invalid ({error}); try again")
                continue
            quality = "ok" if rms >= MIN_RMS else "SILENT?"
            print(f"  captured {duration:.1f}s, rms {rms:.0f} [{quality}]")
            answer = input("  (a)ccept, (r)etry, (s)kip, (q)uit: ").strip().lower()
            if answer in ("", "a"):
                entry = manifest_case(case_id)
                entry["expected"] = case["expected"]
                entry["tags"] = case.get("tags", [])
                recorded += 1
                break
            if answer == "s":
                wav_path.unlink(missing_ok=True)
                break
            if answer == "q":
                wav_path.unlink(missing_ok=True)
                manifest_path.write_text(
                    json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
                )
                print(f"manifest written to {manifest_path}")
                return 0
            # anything else retries

    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"\n{recorded} new recordings; manifest written to {manifest_path}")
    print("next: python3 tools/phase2/transcribe_corpus.py")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

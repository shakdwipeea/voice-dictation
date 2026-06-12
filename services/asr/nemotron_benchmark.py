#!/usr/bin/env python3
"""Preflight and benchmark NVIDIA Nemotron cache-aware streaming ASR."""

from __future__ import annotations

import argparse
import ast
import importlib.util
import json
import platform
import subprocess
import sys
import time
import wave
from dataclasses import asdict, dataclass
from importlib import metadata
from pathlib import Path
from typing import Sequence

MODEL_NAME = "nvidia/nemotron-speech-streaming-en-0.6b"
PROFILE_CONTEXTS = {
    "80": [70, 0],
    "160": [70, 1],
    "560": [70, 6],
    "1120": [70, 13],
}
STREAMING_SCRIPT = Path(
    "examples/asr/asr_cache_aware_streaming/"
    "speech_to_text_cache_aware_streaming_infer.py"
)


@dataclass(frozen=True)
class AudioMetadata:
    path: str
    duration_seconds: float
    sample_rate_hz: int
    channels: int
    sample_width_bytes: int
    frames: int


def command_output(command: Sequence[str]) -> dict[str, object]:
    started = time.perf_counter()
    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=20,
        )
        return {
            "available": True,
            "returncode": result.returncode,
            "stdout": result.stdout.strip(),
            "stderr": result.stderr.strip(),
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 2),
        }
    except (FileNotFoundError, subprocess.TimeoutExpired) as error:
        return {
            "available": False,
            "error": str(error),
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 2),
        }


def module_available(name: str) -> bool:
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ModuleNotFoundError):
        return False


def package_version(name: str) -> str | None:
    try:
        return metadata.version(name)
    except metadata.PackageNotFoundError:
        return None


def build_preflight() -> dict[str, object]:
    modules = {
        name: module_available(name)
        for name in ("torch", "nemo", "nemo.collections.asr", "omegaconf")
    }
    nvidia_smi = command_output(
        [
            "nvidia-smi",
            "--query-gpu=name,memory.total,driver_version",
            "--format=csv,noheader",
        ]
    )
    cuda = {
        "checked": False,
        "available": False,
        "allocation_test": False,
        "device_count": 0,
        "devices": [],
        "error": None,
        "torch_version": None,
        "torch_cuda_version": None,
        "cuda_bindings_version": package_version("cuda-bindings"),
    }
    if modules["torch"]:
        cuda["checked"] = True
        try:
            import torch

            cuda["torch_version"] = torch.__version__
            cuda["torch_cuda_version"] = torch.version.cuda
            cuda["available"] = torch.cuda.is_available()
            cuda["device_count"] = torch.cuda.device_count()
            cuda["devices"] = [
                torch.cuda.get_device_name(index)
                for index in range(torch.cuda.device_count())
            ]
            if cuda["available"]:
                tensor = torch.zeros(1, device="cuda")
                torch.cuda.synchronize()
                cuda["allocation_test"] = tensor.item() == 0
        except Exception as error:  # Runtime-specific diagnostics.
            cuda["error"] = repr(error)

    blockers: list[str] = []
    if nvidia_smi.get("returncode") != 0:
        blockers.append("nvidia-smi cannot communicate with the NVIDIA driver")
    for module in ("torch", "nemo.collections.asr", "omegaconf"):
        if not modules[module]:
            blockers.append(f"Python module is missing: {module}")
    if cuda["checked"] and not cuda["available"]:
        blockers.append("PyTorch cannot access CUDA")
    elif cuda["checked"] and not cuda["allocation_test"]:
        blockers.append("PyTorch cannot allocate and synchronize a CUDA tensor")

    warnings: list[str] = []
    torch_cuda_version = cuda["torch_cuda_version"]
    cuda_bindings_version = cuda["cuda_bindings_version"]
    if isinstance(torch_cuda_version, str) and isinstance(cuda_bindings_version, str):
        if torch_cuda_version.split(".", 1)[0] != cuda_bindings_version.split(".", 1)[0]:
            warnings.append(
                f"cuda-bindings {cuda_bindings_version} does not match PyTorch "
                f"CUDA {torch_cuda_version}; optimized CUDA graphs may fall back"
            )

    return {
        "model": MODEL_NAME,
        "python": sys.version,
        "platform": platform.platform(),
        "modules": modules,
        "nvidia_smi": nvidia_smi,
        "cuda": cuda,
        "ready": not blockers,
        "blockers": blockers,
        "warnings": warnings,
    }


def inspect_wav(path: Path) -> AudioMetadata:
    try:
        with wave.open(str(path), "rb") as wav:
            metadata = AudioMetadata(
                path=str(path.resolve()),
                duration_seconds=round(wav.getnframes() / wav.getframerate(), 4),
                sample_rate_hz=wav.getframerate(),
                channels=wav.getnchannels(),
                sample_width_bytes=wav.getsampwidth(),
                frames=wav.getnframes(),
            )
    except (FileNotFoundError, wave.Error) as error:
        raise ValueError(f"Cannot read WAV file {path}: {error}") from error

    problems = []
    if metadata.sample_rate_hz != 16_000:
        problems.append(f"sample rate is {metadata.sample_rate_hz} Hz, expected 16000 Hz")
    if metadata.channels != 1:
        problems.append(f"audio has {metadata.channels} channels, expected mono")
    if metadata.sample_width_bytes != 2:
        problems.append(
            f"sample width is {metadata.sample_width_bytes} bytes, expected 2"
        )
    if problems:
        raise ValueError(f"{path}: " + "; ".join(problems))
    return metadata


def parse_profiles(values: Sequence[str]) -> list[str]:
    profiles: list[str] = []
    for value in values:
        if value not in PROFILE_CONTEXTS:
            options = ", ".join(PROFILE_CONTEXTS)
            raise ValueError(f"Unsupported profile {value!r}; choose from {options}")
        if value not in profiles:
            profiles.append(value)
    return profiles


def find_streaming_script(nemo_root: Path) -> Path:
    script = nemo_root / STREAMING_SCRIPT
    if not script.is_file():
        raise ValueError(f"NeMo streaming script not found: {script}")
    return script


def create_manifest(path: Path, audios: Sequence[AudioMetadata]) -> None:
    create_manifest_with_references(path, audios, None)


def create_manifest_with_references(
    path: Path,
    audios: Sequence[AudioMetadata],
    references: Sequence[str] | None,
) -> None:
    with path.open("w", encoding="utf-8") as manifest:
        for index, audio in enumerate(audios):
            record: dict[str, object] = {
                "audio_filepath": audio.path,
                "duration": audio.duration_seconds,
            }
            if references is not None:
                record["text"] = references[index]
            manifest.write(json.dumps(record) + "\n")


def parse_streaming_metrics(stdout: str) -> dict[str, object]:
    metrics: dict[str, object] = {}
    transcription_marker = "Final streaming transcriptions:"
    timing_marker = "The whole streaming process took:"
    for line in stdout.splitlines():
        if transcription_marker in line:
            payload = line.split(transcription_marker, 1)[1].strip()
            # NeMo logs the Python repr of a list, which uses single quotes
            # unless the text happens to contain an apostrophe; JSON parsing
            # alone would silently drop most transcriptions.
            transcriptions: object
            try:
                transcriptions = ast.literal_eval(payload)
            except (ValueError, SyntaxError):
                try:
                    transcriptions = json.loads(payload)
                except json.JSONDecodeError:
                    continue
            if isinstance(transcriptions, list):
                metrics["transcriptions"] = transcriptions
        elif timing_marker in line:
            payload = line.split(timing_marker, 1)[1].strip()
            if payload.endswith("s"):
                try:
                    metrics["streaming_process_seconds"] = float(payload[:-1])
                except ValueError:
                    continue
    return metrics


def run_profile(
    script: Path,
    manifest: Path,
    output_dir: Path,
    profile: str,
    python: str,
    model: str,
) -> dict[str, object]:
    profile_dir = output_dir / f"{profile}ms"
    profile_dir.mkdir(parents=True, exist_ok=True)
    context = PROFILE_CONTEXTS[profile]
    command = [
        python,
        str(script),
        f"dataset_manifest={manifest}",
        "batch_size=1",
        f"att_context_size=[{context[0]},{context[1]}]",
        f"output_path={profile_dir}",
    ]
    model_argument = (
        f"model_path={model}"
        if model.endswith(".nemo") or Path(model).is_file()
        else f"pretrained_name={model}"
    )
    command.insert(2, model_argument)
    started = time.perf_counter()
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    elapsed = time.perf_counter() - started
    return {
        "profile_ms": int(profile),
        "att_context_size": context,
        "command": command,
        "returncode": result.returncode,
        "elapsed_seconds": round(elapsed, 4),
        "stdout": result.stdout,
        "stderr": result.stderr,
        **parse_streaming_metrics(result.stdout),
    }


def write_json(data: dict[str, object], output: Path | None) -> None:
    payload = json.dumps(data, indent=2, sort_keys=True) + "\n"
    if output:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(payload, encoding="utf-8")
    print(payload, end="")


def preflight_command(args: argparse.Namespace) -> int:
    result = build_preflight()
    write_json(result, args.output)
    return 0 if result["ready"] else 2


def benchmark_command(args: argparse.Namespace) -> int:
    preflight = build_preflight()
    if not preflight["ready"] and not args.ignore_preflight:
        write_json(preflight, args.output_dir / "preflight.json")
        print(
            "Nemotron runtime preflight failed; use --ignore-preflight only for "
            "diagnosing the benchmark command.",
            file=sys.stderr,
        )
        return 2

    try:
        profiles = parse_profiles(args.profiles)
        script = find_streaming_script(args.nemo_root)
        audios = [inspect_wav(path) for path in args.audio]
        if args.reference and len(args.reference) != len(audios):
            raise ValueError("--reference must be supplied once per --audio file")
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2

    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest = args.output_dir / "manifest.jsonl"
    references = args.reference
    create_manifest_with_references(manifest, audios, references)

    runs = [
        run_profile(
            script=script,
            manifest=manifest,
            output_dir=args.output_dir,
            profile=profile,
            python=args.python,
            model=args.model,
        )
        for profile in profiles
    ]
    total_audio_seconds = sum(audio.duration_seconds for audio in audios)
    for run in runs:
        streaming_seconds = run.get("streaming_process_seconds")
        if isinstance(streaming_seconds, float) and total_audio_seconds:
            run["real_time_factor"] = round(streaming_seconds / total_audio_seconds, 4)

    result = {
        "model": args.model,
        "preflight": preflight,
        "audio": [asdict(audio) for audio in audios],
        "references": references,
        "references_provided": references is not None,
        "runs": runs,
        "timing_scope": (
            "Process-level elapsed time includes Python, model loading, and official "
            "NeMo script startup; it is not warm-stream application latency."
        ),
    }
    write_json(result, args.output_dir / "benchmark.json")
    return 0 if all(run["returncode"] == 0 for run in runs) else 1


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    preflight = subparsers.add_parser("preflight", help="check local runtime")
    preflight.add_argument("--output", type=Path)
    preflight.set_defaults(func=preflight_command)

    benchmark = subparsers.add_parser(
        "benchmark", help="run NeMo's official cache-aware streaming example"
    )
    benchmark.add_argument(
        "--nemo-root", type=Path, default=Path("build/phase0/nemotron/nemo-source")
    )
    benchmark.add_argument("--audio", type=Path, nargs="+", required=True)
    benchmark.add_argument(
        "--reference",
        nargs="+",
        help="reference transcript for each audio file; omitted references skip WER",
    )
    benchmark.add_argument(
        "--profiles", nargs="+", default=["80", "160", "560"]
    )
    benchmark.add_argument("--model", default=MODEL_NAME)
    benchmark.add_argument("--python", default=sys.executable)
    benchmark.add_argument("--output-dir", type=Path, required=True)
    benchmark.add_argument("--ignore-preflight", action="store_true")
    benchmark.set_defaults(func=benchmark_command)
    return parser


def main() -> int:
    args = create_parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())

"""M1 smoke test — record from default mic, transcribe, print.

Usage:
    uv run vd-smoke                      # CPU + tiny.en, 5s
    uv run vd-smoke --device cuda --model small.en --seconds 8
    uv run vd-smoke --wav path/to/x.wav  # skip recording; use existing wav

The script writes a wav to ~/.local/state/voice-dictation/last-recording.wav
so a failed transcription does not lose the audio.
"""
from __future__ import annotations

import argparse
import os
import sys
import time
import wave
from pathlib import Path

import numpy as np
import sounddevice as sd
import soundfile as sf

STATE_DIR = Path(
    os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state"
) / "voice-dictation"
STATE_DIR.mkdir(parents=True, exist_ok=True)

SAMPLE_RATE = 16_000  # Whisper's native rate


def record(seconds: float) -> np.ndarray:
    print(f"  recording {seconds:.1f}s from default input … ", end="", flush=True)
    t0 = time.perf_counter()
    audio = sd.rec(
        int(seconds * SAMPLE_RATE),
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="float32",
        blocking=True,
    )
    elapsed = time.perf_counter() - t0
    print(f"done ({elapsed:.2f}s wall)")
    return audio.squeeze()


def save_wav(audio: np.ndarray, path: Path) -> None:
    sf.write(str(path), audio, SAMPLE_RATE, subtype="PCM_16")


def load_wav(path: Path) -> np.ndarray:
    data, sr = sf.read(str(path), dtype="float32")
    if sr != SAMPLE_RATE:
        raise SystemExit(f"wav sample rate {sr} != {SAMPLE_RATE}; resample first")
    if data.ndim > 1:
        data = data.mean(axis=1)
    return data


def transcribe(audio: np.ndarray, model_name: str, device: str, compute_type: str) -> dict:
    if device == "cuda":
        from voice_dictation._cuda_preload import preload
        preload()
    # Import here so an import failure shows up at the moment we ask for the model,
    # not at script-startup time. Useful for separating "code broken" from "GPU broken".
    from faster_whisper import WhisperModel

    print(f"  loading model={model_name!r} device={device!r} compute_type={compute_type!r} …")
    t_load_start = time.perf_counter()
    model = WhisperModel(model_name, device=device, compute_type=compute_type)
    t_load = time.perf_counter() - t_load_start
    print(f"  model loaded in {t_load:.2f}s")

    print("  transcribing …")
    t_inf_start = time.perf_counter()
    segments, info = model.transcribe(
        audio,
        language="en",
        beam_size=5,
        vad_filter=False,
    )
    segments = list(segments)  # force generator
    t_inf = time.perf_counter() - t_inf_start

    text = " ".join(s.text.strip() for s in segments).strip()
    return {
        "text": text,
        "load_time": t_load,
        "inference_time": t_inf,
        "audio_duration": len(audio) / SAMPLE_RATE,
        "language": info.language,
        "language_prob": info.language_probability,
        "segments": [(s.start, s.end, s.text) for s in segments],
    }


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--device", default="cpu", choices=["cpu", "cuda"])
    p.add_argument("--model", default="tiny.en")
    p.add_argument(
        "--compute-type",
        default=None,
        help="default: int8 for CPU, float16 for CUDA",
    )
    p.add_argument("--seconds", type=float, default=5.0)
    p.add_argument(
        "--wav",
        type=Path,
        default=None,
        help="transcribe an existing wav instead of recording (must be 16kHz mono)",
    )
    p.add_argument(
        "--save",
        type=Path,
        default=STATE_DIR / "last-recording.wav",
        help="where to save the captured audio",
    )
    args = p.parse_args()

    if args.compute_type is None:
        args.compute_type = "float16" if args.device == "cuda" else "int8"

    print(f"== voice-dictation smoke test ==")
    print(f"  device={args.device}  model={args.model}  compute_type={args.compute_type}")

    if args.wav is not None:
        print(f"  loading wav {args.wav}")
        audio = load_wav(args.wav)
        print(f"  audio: {len(audio)/SAMPLE_RATE:.2f}s")
    else:
        # Show input device for transparency
        try:
            dev = sd.query_devices(kind="input")
            print(f"  input device: {dev['name']}  (sr={int(dev['default_samplerate'])})")
        except Exception as e:
            print(f"  (could not query input device: {e})")
        audio = record(args.seconds)
        save_wav(audio, args.save)
        print(f"  saved capture to {args.save}")

    result = transcribe(audio, args.model, args.device, args.compute_type)

    rtf = result["inference_time"] / max(result["audio_duration"], 1e-6)
    print()
    print(f"  detected language: {result['language']} (p={result['language_prob']:.2f})")
    print(f"  load:      {result['load_time']:.2f}s")
    print(f"  inference: {result['inference_time']:.2f}s  (audio {result['audio_duration']:.2f}s, RTF={rtf:.2f}x)")
    print()
    print(f"== transcription ==")
    print(result["text"] or "(empty — no speech detected?)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

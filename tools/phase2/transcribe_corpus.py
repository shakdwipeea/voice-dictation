#!/usr/bin/env python3
"""Fill the recorded corpus manifest's `raw` fields via the ASR sidecar.

Streams each recording through the configured sidecar exactly as the daemon
would (start_session, audio chunks, finish_session) and writes a completed
corpus file that `sunoto-daemon eval --corpus` consumes directly.
"""

from __future__ import annotations

import argparse
import json
import queue
import subprocess
import threading
import wave
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CHUNK_SAMPLES = 1600  # 100 ms at 16 kHz; the sidecar accepts any chunking
READY_TIMEOUT_S = 600.0  # model load can be slow on a cold page cache
FINAL_TIMEOUT_S = 120.0


def sidecar_command(backend: str, profile_ms: int, python: str | None) -> list[str]:
    if backend == "mock":
        script = REPO_ROOT / "services/asr/mock_sidecar.py"
        return [python or "python3", str(script)]
    if backend == "nemotron":
        script = REPO_ROOT / "services/asr/nemotron_sidecar.py"
        if python is None:
            venv = REPO_ROOT / ".venv-nemotron/bin/python"
            python = str(venv) if venv.is_file() else "python3"
        return [python, str(script), "--profile-ms", str(profile_ms)]
    raise ValueError(f"unknown backend: {backend}")


def read_samples(path: Path) -> list[int]:
    with wave.open(str(path), "rb") as wav:
        if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) != (16_000, 1, 2):
            raise RuntimeError(f"{path} is not 16 kHz mono s16le")
        frames = wav.readframes(wav.getnframes())
    import array

    samples = array.array("h")
    samples.frombytes(frames)
    return samples.tolist()


class Sidecar:
    """Line-oriented protocol client with a reader thread, so partial events
    streaming back never deadlock the writing side."""

    def __init__(self, command: list[str]) -> None:
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.events: queue.Queue[dict | None] = queue.Queue()
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()

    def _pump(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self.events.put(json.loads(line))
            except json.JSONDecodeError:
                print(f"[transcribe] ignored non-protocol output: {line!r}")
        self.events.put(None)

    def send(self, request: dict) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def wait_for(self, predicate, timeout_s: float) -> dict:
        while True:
            event = self.events.get(timeout=timeout_s)
            if event is None:
                raise RuntimeError("sidecar exited unexpectedly")
            if event.get("type") == "error":
                raise RuntimeError(f"sidecar error: {event.get('message')}")
            if predicate(event):
                return event

    def close(self) -> None:
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def transcribe_all(manifest: dict, manifest_dir: Path, sidecar: Sidecar) -> None:
    sidecar.send({"type": "health"})
    ready = sidecar.wait_for(lambda e: e.get("type") == "ready", READY_TIMEOUT_S)
    print(f"sidecar ready: {ready.get('backend')}")

    for index, case in enumerate(manifest["cases"], start=1):
        audio = manifest_dir / case["audio"]
        if not audio.exists():
            print(f"[skip] {case['id']}: no recording at {audio}")
            continue
        samples = read_samples(audio)
        session_id = index
        sidecar.send({"type": "start_session", "session_id": session_id})
        for offset in range(0, len(samples), CHUNK_SAMPLES):
            sidecar.send(
                {
                    "type": "audio_chunk",
                    "session_id": session_id,
                    "samples": samples[offset : offset + CHUNK_SAMPLES],
                }
            )
        sidecar.send({"type": "finish_session", "session_id": session_id})
        final = sidecar.wait_for(
            lambda e: e.get("type") == "final" and e.get("session_id") == session_id,
            FINAL_TIMEOUT_S,
        )
        case["raw"] = final.get("text", "")
        print(f"{case['id']}: {case['raw']!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path("tests/corpus/phase2-recorded/manifest.json"),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="completed corpus path (default: <manifest dir>/corpus-recorded.json)",
    )
    parser.add_argument("--backend", choices=["mock", "nemotron"], default="nemotron")
    parser.add_argument("--profile-ms", type=int, default=160)
    parser.add_argument("--python", default=None, help="sidecar interpreter override")
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    manifest_dir = args.manifest.resolve().parent
    output = args.output or manifest_dir / "corpus-recorded.json"

    sidecar = Sidecar(sidecar_command(args.backend, args.profile_ms, args.python))
    try:
        transcribe_all(manifest, manifest_dir, sidecar)
    finally:
        sidecar.close()

    transcribed = [case for case in manifest["cases"] if case.get("raw")]
    output.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"\n{len(transcribed)}/{len(manifest['cases'])} cases transcribed -> {output}")
    print(f"next: cargo run -q -p sunoto-daemon -- eval --corpus {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

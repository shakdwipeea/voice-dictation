"""Interactive sample manager — record voice clips with ground-truth text,
then transcribe them against any model/device combination to validate
accent + tech-term accuracy.

Run:
    uv run vd-samples

Samples live in tests/samples/<id>.wav + <id>.json (ground truth + metadata).
"""
from __future__ import annotations

import json
import re
import shlex
import shutil
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, asdict
from datetime import datetime
from pathlib import Path

import numpy as np
import sounddevice as sd
import soundfile as sf
from rich.console import Console
from rich.prompt import Prompt, Confirm
from rich.table import Table
from rich.text import Text

SAMPLE_RATE = 16_000
SAMPLES_DIR = Path(__file__).resolve().parents[2] / "tests" / "samples"
SAMPLES_DIR.mkdir(parents=True, exist_ok=True)
DEFAULT_MAX_SECONDS = 30.0

console = Console()


@dataclass
class Sample:
    id: str           # e.g. "2026-05-23_21-32-30"
    slug: str         # filesystem-safe truncation of ground_truth
    ground_truth: str
    duration_s: float
    created_at: str
    wav_path: str
    json_path: str

    @property
    def short_truth(self) -> str:
        t = self.ground_truth
        return t if len(t) <= 70 else t[:67] + "…"


def _slugify(text: str, max_len: int = 50) -> str:
    s = re.sub(r"[^a-zA-Z0-9]+", "-", text.strip().lower()).strip("-")
    return (s[:max_len] or "untitled")


def list_samples() -> list[Sample]:
    out: list[Sample] = []
    for jpath in sorted(SAMPLES_DIR.glob("*.json")):
        try:
            data = json.loads(jpath.read_text())
            out.append(Sample(**data))
        except Exception as e:
            console.print(f"[yellow]warn:[/yellow] could not read {jpath.name}: {e}")
    return out


def print_samples_table(samples: list[Sample]) -> None:
    if not samples:
        console.print("[dim](no samples yet)[/dim]")
        return
    t = Table(show_header=True, header_style="bold")
    t.add_column("#", style="dim", width=3, justify="right")
    t.add_column("ID")
    t.add_column("dur", justify="right")
    t.add_column("ground truth")
    for i, s in enumerate(samples, 1):
        t.add_row(str(i), s.id, f"{s.duration_s:.1f}s", s.short_truth)
    console.print(t)


def record_sample() -> Sample | None:
    console.print()
    truth = Prompt.ask("[bold]What will you speak?[/bold]  (the exact ground-truth text)").strip()
    if not truth:
        console.print("[yellow]aborted: empty ground truth[/yellow]")
        return None

    console.print(f"\nGround truth: [italic cyan]{truth}[/italic cyan]")
    max_s = float(Prompt.ask("Max seconds", default=str(DEFAULT_MAX_SECONDS)))

    try:
        dev = sd.query_devices(kind="input")
        console.print(f"[dim]input: {dev['name']} @ {int(dev['default_samplerate'])} Hz[/dim]")
    except Exception:
        pass

    if not Confirm.ask("Ready to record?", default=True):
        return None

    # 3-2-1 countdown
    for n in (3, 2, 1):
        console.print(f"  [bold yellow]{n}…[/bold yellow]", end="\r")
        time.sleep(0.7)
    console.print("  [bold green]🎙  recording — press ENTER to stop[/bold green]              ")

    audio, actual_seconds = _record_until_enter_or_max(max_s)

    if actual_seconds < 0.3:
        console.print("[yellow]too short, discarding[/yellow]")
        return None

    console.print(f"  [dim]captured {actual_seconds:.2f}s[/dim]")

    if not Confirm.ask("Save this sample?", default=True):
        return None

    ts = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
    slug = _slugify(truth)
    sample_id = f"{ts}_{slug}"
    wav_path = SAMPLES_DIR / f"{sample_id}.wav"
    json_path = SAMPLES_DIR / f"{sample_id}.json"

    sf.write(str(wav_path), audio, SAMPLE_RATE, subtype="PCM_16")
    sample = Sample(
        id=sample_id,
        slug=slug,
        ground_truth=truth,
        duration_s=actual_seconds,
        created_at=ts,
        wav_path=str(wav_path.relative_to(SAMPLES_DIR.parents[1])),
        json_path=str(json_path.relative_to(SAMPLES_DIR.parents[1])),
    )
    json_path.write_text(json.dumps(asdict(sample), indent=2))
    console.print(f"[green]✓ saved[/green] {wav_path.relative_to(SAMPLES_DIR.parents[1])}")
    return sample


def _record_until_enter_or_max(max_seconds: float) -> tuple[np.ndarray, float]:
    """Record up to max_seconds, stop early on Enter keypress.

    Uses sounddevice's InputStream + a threading.Event triggered by stdin read.
    """
    chunks: list[np.ndarray] = []
    stop_evt = threading.Event()
    t_start = time.perf_counter()

    def cb(indata, frames, time_info, status):
        if status:
            pass  # could log overflows
        chunks.append(indata.copy())

    def watch_stdin():
        try:
            sys.stdin.readline()
        except Exception:
            pass
        stop_evt.set()

    stdin_thr = threading.Thread(target=watch_stdin, daemon=True)
    stdin_thr.start()

    with sd.InputStream(samplerate=SAMPLE_RATE, channels=1, dtype="float32", callback=cb):
        while not stop_evt.is_set():
            if (time.perf_counter() - t_start) >= max_seconds:
                break
            time.sleep(0.05)

    elapsed = time.perf_counter() - t_start
    if not chunks:
        return np.zeros(0, dtype="float32"), 0.0
    audio = np.concatenate(chunks, axis=0).squeeze()
    return audio, elapsed


def play_sample(sample: Sample) -> None:
    data, sr = sf.read(sample.wav_path, dtype="float32")
    console.print(f"  playing {sample.id} ({sample.duration_s:.1f}s)…")
    sd.play(data, sr, blocking=True)


def delete_sample(sample: Sample) -> None:
    if not Confirm.ask(f"Delete [red]{sample.id}[/red]?", default=False):
        return
    Path(sample.wav_path).unlink(missing_ok=True)
    Path(sample.json_path).unlink(missing_ok=True)
    console.print(f"[red]✗ deleted[/red] {sample.id}")


def transcribe_sample(sample: Sample, device: str, model_name: str) -> None:
    """Transcribe one sample, print expected vs actual + simple WER."""
    if device == "cuda":
        from voice_dictation._cuda_preload import preload
        preload()
    from faster_whisper import WhisperModel

    # Lazy cache across calls in same session
    cache_key = (device, model_name)
    cache = transcribe_sample._cache  # type: ignore[attr-defined]
    if cache_key not in cache:
        compute_type = "float16" if device == "cuda" else "int8"
        with console.status(f"loading {model_name} on {device}…"):
            cache[cache_key] = WhisperModel(model_name, device=device, compute_type=compute_type)
    model = cache[cache_key]

    data, sr = sf.read(sample.wav_path, dtype="float32")
    if data.ndim > 1:
        data = data.mean(axis=1)

    t0 = time.perf_counter()
    segments, info = model.transcribe(
        data, language="en", beam_size=5, vad_filter=False
    )
    text = " ".join(s.text.strip() for s in segments).strip()
    elapsed = time.perf_counter() - t0

    wer = _wer(sample.ground_truth, text)
    console.print()
    console.print(f"  [bold]{sample.id}[/bold]  ({sample.duration_s:.1f}s audio → {elapsed:.2f}s)")
    console.print(f"  expected:    [cyan]{sample.ground_truth}[/cyan]")
    console.print(f"  transcribed: [green]{text or '(empty)'}[/green]")
    console.print(f"  WER:         {wer*100:.1f}%")


transcribe_sample._cache = {}  # type: ignore[attr-defined]


def _wer(reference: str, hypothesis: str) -> float:
    """Word Error Rate via Levenshtein on tokens. Lowercased, alnum-only."""
    def toks(s: str) -> list[str]:
        return re.findall(r"[a-z0-9]+", s.lower())
    r = toks(reference)
    h = toks(hypothesis)
    if not r:
        return 0.0 if not h else 1.0
    # DP edit distance
    dp = [[0] * (len(h) + 1) for _ in range(len(r) + 1)]
    for i in range(len(r) + 1):
        dp[i][0] = i
    for j in range(len(h) + 1):
        dp[0][j] = j
    for i in range(1, len(r) + 1):
        for j in range(1, len(h) + 1):
            if r[i - 1] == h[j - 1]:
                dp[i][j] = dp[i - 1][j - 1]
            else:
                dp[i][j] = 1 + min(dp[i - 1][j], dp[i][j - 1], dp[i - 1][j - 1])
    return dp[len(r)][len(h)] / len(r)


def help_text() -> str:
    return (
        "  [bold]n[/bold]                    record a new sample\n"
        "  [bold]t[/bold] [<n>|all]          transcribe (default: all)\n"
        "  [bold]t[/bold] <n> --model M --device D    transcribe with specific model\n"
        "  [bold]p[/bold] <n>                play back sample n\n"
        "  [bold]d[/bold] <n>                delete sample n\n"
        "  [bold]l[/bold]                    list samples (refresh)\n"
        "  [bold]h[/bold]                    this help\n"
        "  [bold]q[/bold]                    quit"
    )


def _parse_transcribe_args(rest: list[str]) -> tuple[str | None, str, str]:
    """Returns (target, model, device) where target is 'all' / index / None."""
    target: str | None = None
    model = "large-v3-turbo"
    device = "cuda"
    i = 0
    while i < len(rest):
        tok = rest[i]
        if tok == "--model" and i + 1 < len(rest):
            model = rest[i + 1]; i += 2
        elif tok == "--device" and i + 1 < len(rest):
            device = rest[i + 1]; i += 2
        elif target is None:
            target = tok; i += 1
        else:
            i += 1
    return target, model, device


def main() -> int:
    console.print("[bold magenta]== voice-dictation: sample manager ==[/bold magenta]")
    console.print("[dim]samples dir: tests/samples/[/dim]\n")

    while True:
        samples = list_samples()
        print_samples_table(samples)
        try:
            line = console.input("\n[bold blue]>[/bold blue] ").strip()
        except (EOFError, KeyboardInterrupt):
            console.print()
            break
        if not line:
            continue
        parts = shlex.split(line)
        cmd, *rest = parts

        if cmd in ("q", "quit", "exit"):
            break
        if cmd in ("h", "help", "?"):
            console.print(help_text()); continue
        if cmd in ("l", "ls", "list"):
            continue  # loop will re-print
        if cmd in ("n", "new", "record"):
            record_sample(); continue
        if cmd in ("p", "play"):
            if not rest:
                console.print("[yellow]usage: p <n>[/yellow]"); continue
            try:
                idx = int(rest[0])
                play_sample(samples[idx - 1])
            except (ValueError, IndexError):
                console.print("[yellow]invalid index[/yellow]")
            continue
        if cmd in ("d", "del", "delete"):
            if not rest:
                console.print("[yellow]usage: d <n>[/yellow]"); continue
            try:
                idx = int(rest[0])
                delete_sample(samples[idx - 1])
            except (ValueError, IndexError):
                console.print("[yellow]invalid index[/yellow]")
            continue
        if cmd in ("t", "test", "transcribe"):
            target, model, device = _parse_transcribe_args(rest)
            console.print(f"[dim]model={model}  device={device}[/dim]")
            if target in (None, "all"):
                targets = samples
            else:
                try:
                    targets = [samples[int(target) - 1]]
                except (ValueError, IndexError):
                    console.print("[yellow]invalid index[/yellow]"); continue
            if not targets:
                console.print("[yellow]no samples to transcribe[/yellow]"); continue
            for s in targets:
                try:
                    transcribe_sample(s, device, model)
                except Exception as e:
                    console.print(f"[red]error on {s.id}:[/red] {e}")
            continue

        console.print(f"[yellow]unknown command:[/yellow] {cmd}  (type [bold]h[/bold] for help)")

    console.print("[dim]bye[/dim]")
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Qwen3-ASR (mlx-audio) for the word-list benchmarks, as a drop-in next to
the Parakeet sidecar engine. Qwen3-ASR reads context: names, vocabulary or
background text go in its system prompt, and the model uses them while it
transcribes (no decoding bonus, no fix afterwards).

Needs build/venv-qwen3-asr (mlx-audio); see bench_*.py --engine.
"""

from __future__ import annotations

import time
from array import array

MODELS = {
    "qwen3-bf16": "mlx-community/Qwen3-ASR-0.6B-bf16",
    "qwen3-8bit": "mlx-community/Qwen3-ASR-0.6B-8bit",
}
SCREEN_CONTEXT_CHARS = 2000


def context_prompt(words: dict | None, screen_text: str | None = None, style: str = "list") -> str | None:
    """The system prompt for one dictation: the word list ("list"), or the
    screen's own text followed by the list ("screen")."""
    if not words:
        return None
    listed, seen = [], set()
    for tier in ("screen", "project", "everywhere"):
        for word in words.get(tier) or ():
            if word.lower() not in seen:
                seen.add(word.lower())
                listed.append(word)
    parts = []
    if style == "screen" and screen_text:
        parts.append(screen_text.strip()[:SCREEN_CONTEXT_CHARS])
    if listed:
        parts.append("Vocabulary: " + ", ".join(listed) + ".")
    return "\n".join(parts) or None


class Qwen3Engine:
    def __init__(self, name: str = "qwen3-bf16") -> None:
        import mlx.core as mx
        from mlx_audio.stt import load

        self.mx = mx
        self.name = name
        started = time.perf_counter()
        self.model = load(MODELS[name])
        self.load_s = time.perf_counter() - started
        # Warm up the encoder and decoder kernels, as the sidecar does.
        self.transcribe(array("h", [0] * 16000), None)

    def transcribe(self, pcm: array, context: str | None) -> str:
        import numpy as np

        audio = np.frombuffer(memoryview(pcm), dtype="<i2").astype(np.float32) / 32768.0
        seconds = len(audio) / 16000
        result = self.model.generate(
            audio,
            language="English",
            system_prompt=context,
            # A dictation runs about three tokens a second; the cap only stops
            # a runaway repetition from taking minutes.
            max_tokens=int(12 * seconds) + 40,
        )
        return result.text.strip()

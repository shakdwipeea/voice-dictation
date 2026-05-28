"""faster-whisper wrapper. Loads model once, reuses across requests."""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from typing import Optional

import numpy as np

log = logging.getLogger(__name__)


@dataclass
class TranscribeResult:
    text: str
    audio_duration_s: float
    inference_s: float
    language: str
    language_prob: float


class Transcriber:
    def __init__(
        self,
        model_name: str = "large-v3-turbo",
        device: str = "cuda",
        compute_type: Optional[str] = None,
        language: str = "en",
    ) -> None:
        if compute_type is None:
            compute_type = "float16" if device == "cuda" else "int8"
        if device == "cuda":
            from voice_dictation._cuda_preload import preload
            preload()
        from faster_whisper import WhisperModel
        log.info("loading model=%s device=%s compute_type=%s", model_name, device, compute_type)
        t0 = time.perf_counter()
        self.model = WhisperModel(model_name, device=device, compute_type=compute_type)
        log.info("model loaded in %.2fs", time.perf_counter() - t0)
        self.language = language
        self.model_name = model_name
        self.device = device

    def transcribe(self, audio: np.ndarray, *, vad_filter: bool = True) -> TranscribeResult:
        if audio.size == 0:
            return TranscribeResult("", 0.0, 0.0, self.language, 1.0)
        t0 = time.perf_counter()
        segments, info = self.model.transcribe(
            audio,
            language=self.language,
            beam_size=5,
            vad_filter=vad_filter,  # filters out silence at boundaries
            vad_parameters={"min_silence_duration_ms": 400},
        )
        text = " ".join(s.text.strip() for s in segments).strip()
        return TranscribeResult(
            text=text,
            audio_duration_s=len(audio) / 16_000,
            inference_s=time.perf_counter() - t0,
            language=info.language,
            language_prob=info.language_probability,
        )

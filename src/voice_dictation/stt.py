"""faster-whisper wrapper. Loads model once, reuses across requests."""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import numpy as np

from voice_dictation.technical_vocabulary import (
    apply_technical_corrections,
    build_vocabulary,
    build_whisper_context,
)

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
        vocabulary_file: Path | None = None,
        technical_vocabulary_bias: bool = False,
        technical_corrections: bool = True,
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
        self.technical_corrections = technical_corrections
        self.technical_vocabulary_bias = technical_vocabulary_bias or vocabulary_file is not None
        self.vocabulary = build_vocabulary(vocabulary_file) if self.technical_vocabulary_bias else []
        self.initial_prompt, self.hotwords = build_whisper_context(self.vocabulary)
        log.info(
            "technical vocabulary bias=%s terms=%d corrections=%s",
            "on" if self.technical_vocabulary_bias else "off",
            len(self.vocabulary),
            "on" if self.technical_corrections else "off",
        )

    def postprocess(self, text: str) -> str:
        text = text.strip()
        if self.technical_corrections:
            text = apply_technical_corrections(text)
        return text

    def transcribe(self, audio: np.ndarray, *, vad_filter: bool = True) -> TranscribeResult:
        if audio.size == 0:
            return TranscribeResult("", 0.0, 0.0, self.language, 1.0)
        t0 = time.perf_counter()
        segments, info = self.model.transcribe(
            audio,
            language=self.language,
            beam_size=5,
            initial_prompt=self.initial_prompt,
            hotwords=self.hotwords,
            vad_filter=vad_filter,  # filters out silence at boundaries
            vad_parameters={"min_silence_duration_ms": 400},
        )
        text = self.postprocess(" ".join(s.text.strip() for s in segments))
        return TranscribeResult(
            text=text,
            audio_duration_s=len(audio) / 16_000,
            inference_s=time.perf_counter() - t0,
            language=info.language,
            language_prob=info.language_probability,
        )

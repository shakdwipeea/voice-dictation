"""Live audio segmentation using silero-VAD.

Feed PCM frames in via push(), pop completed segments out via pop_segments().
A segment ends when speech-probability stays below threshold for
min_silence_ms continuous milliseconds, AND the buffer contains at least
min_speech_ms of speech.

silero-VAD operates on fixed-size windows of 512 samples @ 16 kHz (~32 ms).
We accumulate input into 512-sample windows internally.
"""
from __future__ import annotations

import logging
import threading
from collections import deque
from typing import Optional

import numpy as np

log = logging.getLogger(__name__)

SAMPLE_RATE = 16_000
WINDOW_SAMPLES = 512  # silero-vad expected window @ 16 kHz
WINDOW_MS = WINDOW_SAMPLES * 1000 / SAMPLE_RATE  # ~32 ms


class Segmenter:
    def __init__(
        self,
        speech_threshold: float = 0.5,
        min_silence_ms: int = 500,
        min_speech_ms: int = 300,
        max_segment_s: float = 25.0,
        pad_ms: int = 200,  # include this much pre-speech context per segment
    ) -> None:
        from silero_vad import load_silero_vad
        self.vad_model = load_silero_vad()  # torch model, runs on CPU (cheap)
        self.speech_threshold = speech_threshold
        self.min_silence_windows = max(1, int(min_silence_ms / WINDOW_MS))
        self.min_speech_windows = max(1, int(min_speech_ms / WINDOW_MS))
        self.max_segment_samples = int(max_segment_s * SAMPLE_RATE)
        self.pad_windows = max(0, int(pad_ms / WINDOW_MS))

        self._leftover = np.zeros(0, dtype="float32")  # < WINDOW_SAMPLES not yet processed
        self._current_segment: list[np.ndarray] = []
        self._silence_streak = 0
        self._speech_streak = 0
        self._has_speech = False
        # ring buffer of recent windows to provide pre-speech padding
        self._pre_pad: deque[np.ndarray] = deque(maxlen=self.pad_windows)

        self._completed: list[np.ndarray] = []
        self._lock = threading.Lock()

    def reset(self) -> None:
        with self._lock:
            self._leftover = np.zeros(0, dtype="float32")
            self._current_segment = []
            self._silence_streak = 0
            self._speech_streak = 0
            self._has_speech = False
            self._pre_pad.clear()
            self._completed = []
        try:
            # silero exposes reset_states on its underlying RNN
            self.vad_model.reset_states()
        except AttributeError:
            pass

    def push(self, frames: np.ndarray) -> int:
        """Feed mono float32 audio frames. Returns number of segments newly completed."""
        import torch
        if frames.ndim > 1:
            frames = frames.mean(axis=1)
        if frames.dtype != np.float32:
            frames = frames.astype("float32")

        new_completed = 0
        with self._lock:
            buf = np.concatenate([self._leftover, frames]) if self._leftover.size else frames
            n_windows = buf.size // WINDOW_SAMPLES
            offset = 0
            for _ in range(n_windows):
                window = buf[offset:offset + WINDOW_SAMPLES]
                offset += WINDOW_SAMPLES
                # silero expects torch float32 tensor of shape (samples,)
                with torch.no_grad():
                    prob = float(self.vad_model(torch.from_numpy(window), SAMPLE_RATE).item())
                is_speech = prob >= self.speech_threshold

                if is_speech:
                    if not self._has_speech:
                        # transitioning from silence to speech: prepend pad context
                        for ctx in self._pre_pad:
                            self._current_segment.append(ctx)
                        self._pre_pad.clear()
                    self._current_segment.append(window.copy())
                    self._silence_streak = 0
                    self._speech_streak += 1
                    self._has_speech = True
                else:
                    if self._has_speech:
                        # still inside a segment that has started
                        self._current_segment.append(window.copy())
                        self._silence_streak += 1
                        if self._silence_streak >= self.min_silence_windows and \
                                self._speech_streak >= self.min_speech_windows:
                            # finalize segment
                            seg = np.concatenate(self._current_segment)
                            self._completed.append(seg)
                            new_completed += 1
                            self._reset_segment_state()
                    else:
                        # pure silence, keep in pad ring
                        self._pre_pad.append(window.copy())

                # safety: cap segment size
                if self._has_speech and sum(c.size for c in self._current_segment) >= self.max_segment_samples:
                    seg = np.concatenate(self._current_segment)
                    self._completed.append(seg)
                    new_completed += 1
                    self._reset_segment_state()

            self._leftover = buf[offset:].copy()
        return new_completed

    def _reset_segment_state(self) -> None:
        self._current_segment = []
        self._silence_streak = 0
        self._speech_streak = 0
        self._has_speech = False
        self._pre_pad.clear()

    def flush(self) -> int:
        """Force-finalize any in-progress segment. Returns number newly completed."""
        with self._lock:
            if self._has_speech and self._speech_streak >= self.min_speech_windows:
                seg = np.concatenate(self._current_segment)
                self._completed.append(seg)
                self._reset_segment_state()
                return 1
            self._reset_segment_state()
            return 0

    def pop_segments(self) -> list[np.ndarray]:
        with self._lock:
            out = self._completed
            self._completed = []
            return out

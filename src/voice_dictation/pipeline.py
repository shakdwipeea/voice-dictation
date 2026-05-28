"""Streaming pipeline: mic → VAD segmenter → whisper → overlay + accumulator.

Long-lived. Holds the whisper model warm. Toggles a recording on/off; while
recording, audio flows through silero-vad, segments at silence boundaries are
transcribed and pushed to the overlay. On stop, all segments are flushed and
joined for a single paste.
"""
from __future__ import annotations

import logging
import queue
import threading
import time
from typing import Optional

import numpy as np
import sounddevice as sd

from voice_dictation.audio import InputChoice, pick_input, resample_to_16k
from voice_dictation.segmenter import Segmenter, SAMPLE_RATE
from voice_dictation.stt import Transcriber

log = logging.getLogger("vd.pipeline")

_FLUSH_SENTINEL = object()
MIN_FALLBACK_RMS = 0.015


class StreamingPipeline:
    def __init__(
        self,
        transcriber: Transcriber,
        overlay=None,  # Optional[Overlay]; duck-typed to avoid hard GTK dep at import
        input_device=None,
    ) -> None:
        self.transcriber = transcriber
        self.overlay = overlay
        self.segmenter = Segmenter()

        # Probe for a working input device once at startup; remember choice.
        self.input_choice: InputChoice = pick_input(preferred_device=input_device)
        log.info("audio input: %s", self.input_choice)

        self._audio_q: queue.Queue[np.ndarray] = queue.Queue(maxsize=1000)
        self._whisper_q: queue.Queue = queue.Queue()

        self._accumulator: list[str] = []
        self._accum_lock = threading.Lock()
        self._raw_chunks: list[np.ndarray] = []
        self._raw_lock = threading.Lock()

        self._stream: Optional[sd.InputStream] = None
        self._recording = threading.Event()
        self._shutdown = threading.Event()
        self._t_start: float = 0.0
        self._level_peak: float = 0.0
        self._level_rms: float = 0.0

        # Long-lived workers
        threading.Thread(target=self._segmenter_loop, name="segmenter", daemon=True).start()
        threading.Thread(target=self._whisper_loop, name="whisper", daemon=True).start()
        threading.Thread(target=self._tick_loop, name="tick", daemon=True).start()

    # ---- public ----
    def is_recording(self) -> bool:
        return self._recording.is_set()

    def elapsed(self) -> float:
        return (time.perf_counter() - self._t_start) if self._recording.is_set() else 0.0

    def start_recording(self) -> None:
        if self._recording.is_set():
            return
        with self._accum_lock:
            self._accumulator = []
        with self._raw_lock:
            self._raw_chunks = []
        self.segmenter.reset()
        # drain stale audio
        while True:
            try:
                self._audio_q.get_nowait()
            except queue.Empty:
                break
        self._level_peak = 0.0
        self._level_rms = 0.0
        self._t_start = time.perf_counter()
        self._recording.set()
        # blocksize chosen so that, after resample, we get roughly one VAD window per callback.
        # At 48k → 16k that's 1536 in → 512 out. At 16k native that's 512 → 512.
        native_block = max(512, int(512 * self.input_choice.sample_rate / SAMPLE_RATE))
        self._stream = sd.InputStream(
            device=self.input_choice.device,
            samplerate=self.input_choice.sample_rate,
            channels=1,
            dtype="float32",
            callback=self._audio_cb,
            blocksize=native_block,
        )
        self._stream.start()
        if self.overlay is not None:
            self.overlay.clear_segments()
            self.overlay.set_status("listening")
            self.overlay.show()

    def stop_recording(self) -> tuple[str, float]:
        """Returns (joined_text, recording_seconds_wallclock)."""
        if not self._recording.is_set():
            return "", 0.0
        recording_seconds = time.perf_counter() - self._t_start
        self._recording.clear()
        if self._stream is not None:
            try:
                self._stream.stop()
                self._stream.close()
            except Exception:  # noqa: BLE001
                pass
            self._stream = None

        # Let segmenter thread drain whatever's in the audio queue.
        deadline = time.perf_counter() + 1.0
        while not self._audio_q.empty() and time.perf_counter() < deadline:
            time.sleep(0.02)

        # Force-finalize any in-progress segment
        n = self.segmenter.flush()
        if n:
            for seg in self.segmenter.pop_segments():
                self._whisper_q.put(seg)

        if self.overlay is not None:
            self.overlay.set_status("transcribing…")

        # Wait for whisper to drain
        self._whisper_q.join()

        with self._accum_lock:
            joined = " ".join(s for s in self._accumulator if s).strip()
            self._accumulator = []

        # Fallback: if silero-VAD produced no completed speech segments, still
        # run Whisper on the full recording. This keeps quiet mics / unusual
        # input gain from turning a real recording into an empty paste.
        if not joined:
            with self._raw_lock:
                raw_chunks = self._raw_chunks
                self._raw_chunks = []
            if raw_chunks:
                try:
                    raw = np.concatenate(raw_chunks, axis=0).squeeze()
                    if raw.ndim > 1:
                        raw = raw.mean(axis=1)
                    if self.input_choice.sample_rate != SAMPLE_RATE:
                        raw = resample_to_16k(raw, self.input_choice.sample_rate)
                    raw_rms = float(np.sqrt(np.mean(raw * raw))) if raw.size else 0.0
                    if raw_rms < MIN_FALLBACK_RMS:
                        log.info(
                            "no VAD segments; skipping full-recording fallback "
                            "for low signal (%.2fs rms=%.4f)",
                            len(raw) / SAMPLE_RATE,
                            raw_rms,
                        )
                    else:
                        log.info(
                            "no VAD segments; transcribing full %.2fs recording (rms=%.4f)",
                            len(raw) / SAMPLE_RATE,
                            raw_rms,
                        )
                        result = self.transcriber.transcribe(
                            np.asarray(raw, dtype="float32"),
                            vad_filter=False,
                        )
                        joined = (result.text or "").strip()
                        if joined and self.overlay is not None:
                            self.overlay.add_segment(joined)
                except Exception:  # noqa: BLE001
                    log.exception("full-recording fallback transcription failed")
        else:
            with self._raw_lock:
                self._raw_chunks = []

        if self.overlay is not None:
            self.overlay.set_status("done")
            # leave overlay visible briefly so user sees result, then hide
            threading.Timer(1.2, self.overlay.hide).start()
        return joined, recording_seconds

    def transcribe_wav(self, audio: np.ndarray) -> str:
        """Test/simulate path: batch-transcribe a single audio array."""
        result = self.transcriber.transcribe(audio)
        return result.text

    def shutdown(self) -> None:
        self._shutdown.set()
        if self._recording.is_set():
            try:
                self.stop_recording()
            except Exception:  # noqa: BLE001
                pass

    # ---- audio callback (real-time thread, keep it lean) ----
    def _audio_cb(self, indata, frames, time_info, status):  # type: ignore[no-untyped-def]
        if status:
            pass
        # peak across this block (cheap)
        try:
            peak = float(np.max(np.abs(indata)))
            if peak > self._level_peak:
                self._level_peak = peak
            self._level_rms = float(np.sqrt(np.mean(indata * indata)))
        except Exception:  # noqa: BLE001
            pass
        try:
            with self._raw_lock:
                self._raw_chunks.append(indata.copy())
        except Exception:  # noqa: BLE001
            pass
        try:
            self._audio_q.put_nowait(indata.copy())
        except queue.Full:
            # back-pressure: drop oldest
            try:
                self._audio_q.get_nowait()
                self._audio_q.put_nowait(indata.copy())
            except queue.Empty:
                pass

    # ---- worker loops ----
    def _segmenter_loop(self) -> None:
        native_rate = self.input_choice.sample_rate
        while not self._shutdown.is_set():
            try:
                chunk = self._audio_q.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                mono = chunk.squeeze()
                if mono.ndim > 1:
                    mono = mono.mean(axis=1)
                if native_rate != SAMPLE_RATE:
                    mono = resample_to_16k(mono, native_rate)
                n = self.segmenter.push(mono)
                if n > 0:
                    for seg in self.segmenter.pop_segments():
                        self._whisper_q.put(seg)
            except Exception:  # noqa: BLE001
                log.exception("segmenter push failed")

    def _whisper_loop(self) -> None:
        while not self._shutdown.is_set():
            try:
                seg = self._whisper_q.get(timeout=0.2)
            except queue.Empty:
                continue
            try:
                result = self.transcriber.transcribe(seg)
                text = (result.text or "").strip()
                if text:
                    with self._accum_lock:
                        self._accumulator.append(text)
                    if self.overlay is not None:
                        self.overlay.add_segment(text)
                    log.info("segment: %r (audio=%.2fs inf=%.2fs)",
                             text, result.audio_duration_s, result.inference_s)
            except Exception:  # noqa: BLE001
                log.exception("whisper transcribe failed")
            finally:
                self._whisper_q.task_done()

    def _tick_loop(self) -> None:
        # ~12 Hz overlay updates while recording.
        while not self._shutdown.is_set():
            if self._recording.is_set() and self.overlay is not None:
                with self._accum_lock:
                    n = len(self._accumulator)
                self.overlay.set_recording(self.elapsed(), self._level_peak, self._level_rms, n)
                # exponential decay so meter falls when you stop talking
                self._level_peak *= 0.78
                self._level_rms *= 0.78
            time.sleep(0.08)

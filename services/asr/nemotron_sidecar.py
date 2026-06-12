#!/usr/bin/env python3
"""Persistent always-warm Nemotron cache-aware streaming ASR sidecar.

Speaks the Sunoto sidecar protocol (newline-delimited JSON over stdin/stdout,
see crates/sunoto-ipc/src/lib.rs). All logging goes to stderr; stdout carries
protocol events only (the real stdout fd is duplicated at startup and fd 1 is
redirected to stderr so stray library prints can never corrupt the stream).

The streaming engine replicates NeMo's official cache-aware streaming
simulation (examples/asr/asr_cache_aware_streaming/
speech_to_text_cache_aware_streaming_infer.py + CacheAwareStreamingAudioBuffer)
for live, incrementally-arriving audio:

- raw 16 kHz mono float32 samples accumulate in a growable buffer;
- mel features are extracted incrementally with a dither=0/pad_to=0 streaming
  preprocessor; only "stable" frames (whose STFT window cannot change when
  more audio arrives) are committed, recomputing a small margin window so the
  committed frames match whole-utterance extraction (the model uses
  normalize="NA", so there is no utterance-level normalization to break this);
- encoder steps consume streaming_cfg.chunk_size feature frames each (the
  first step uses chunk_size[0] with a zero pre-encode cache and
  drop_extra_pre_encoded=0; later steps use chunk_size[1] with the previous
  pre_encode_cache_size[1] real frames prepended and
  drop_extra_pre_encoded=streaming_cfg.drop_extra_pre_encoded);
- finish_session pads the residual audio with zeros (at least one full
  encoder chunk plus the STFT guard) and flushes the remaining steps, the
  last one with keep_all_outputs=True.
"""

from __future__ import annotations

import argparse
import json
import sys
import time

try:  # numpy lives in the runtime venv; the protocol layer works without it.
    import numpy as _np
except ImportError:  # pragma: no cover - exercised by the stdlib-only tests
    _np = None

BACKEND_NAME = "nemotron"
DEFAULT_MODEL = "nvidia/nemotron-speech-streaming-en-0.6b"

# profile_ms -> encoder attention context [left, right] (multi-lookahead model)
PROFILE_CONTEXTS = {
    80: [70, 0],
    160: [70, 1],
    560: [70, 6],
    1120: [70, 13],
}

SAMPLE_RATE = 16_000
HOP_SAMPLES = 160  # 10 ms feature hop (preprocessor window_stride)
STFT_GUARD_SAMPLES = 256  # n_fft // 2: right context a centered STFT frame needs
# margin (in frames) re-extracted on each incremental featurization so committed
# frames are unaffected by sub-window edge effects (reflect pad + pre-emphasis)
FEATURE_MARGIN_FRAMES = 4


def log(message: str) -> None:
    print(f"[nemotron-sidecar] {message}", file=sys.stderr, flush=True)


def samples_to_float32(samples):
    """Convert i16 PCM sample values to float32 in [-1, 1) (divide by 32768)."""
    if _np is not None:
        return _np.asarray(samples, dtype=_np.float32) / 32768.0
    return [float(value) / 32768.0 for value in samples]


def stable_frame_count(
    total_samples: int,
    hop: int = HOP_SAMPLES,
    guard: int = STFT_GUARD_SAMPLES,
) -> int:
    """Number of leading mel frames that can no longer change as audio grows.

    Frame k is centered at k*hop and looks `guard` samples to the right; it is
    stable once k*hop + guard < total_samples (strict, so reflect padding of
    the current right edge never touches it).
    """
    if total_samples <= guard:
        return 0
    return (total_samples - guard - 1) // hop + 1


def flush_padding_samples(
    total_samples: int,
    chunk_frames: int,
    hop: int = HOP_SAMPLES,
    guard: int = STFT_GUARD_SAMPLES,
) -> int:
    """Zero samples to append on finish_session so everything flushes.

    Guarantees at least one full encoder chunk of silence (chunk_frames * hop)
    plus the STFT guard (so every real frame becomes stable), rounded up so
    the padded total lands on a feature-frame boundary.
    """
    if chunk_frames < 1:
        raise ValueError("chunk_frames must be >= 1")
    if total_samples < 0:
        raise ValueError("total_samples must be >= 0")
    padding = chunk_frames * hop + guard + hop
    padding += (-(total_samples + padding)) % hop
    return padding


def _as_pair(value):
    """streaming_cfg fields may be a scalar or a [first_step, other_steps] pair."""
    if isinstance(value, (list, tuple)):
        return int(value[0]), int(value[1])
    return int(value), int(value)


class _SampleBuffer:
    """Growable float32 sample buffer (amortized O(1) append)."""

    def __init__(self, initial_capacity: int = SAMPLE_RATE):
        self._data = _np.zeros(initial_capacity, dtype=_np.float32)
        self.length = 0

    def append(self, samples) -> None:
        samples = _np.asarray(samples, dtype=_np.float32)
        needed = self.length + samples.size
        if needed > self._data.size:
            capacity = max(self._data.size * 2, needed)
            grown = _np.zeros(capacity, dtype=_np.float32)
            grown[: self.length] = self._data[: self.length]
            self._data = grown
        self._data[self.length : needed] = samples
        self.length = needed

    def append_zeros(self, count: int) -> None:
        self.append(_np.zeros(count, dtype=_np.float32))

    def view(self, start: int, end: int):
        return self._data[start:end]


class NemotronEngine:
    """GPU streaming engine. Loads the model once and stays warm."""

    backend = BACKEND_NAME

    def __init__(
        self,
        model_name: str = DEFAULT_MODEL,
        device: str = "cuda",
        profile_ms: int = 160,
    ):
        if _np is None:
            raise RuntimeError("numpy is required for the Nemotron engine")
        if profile_ms not in PROFILE_CONTEXTS:
            raise ValueError(f"unsupported profile_ms: {profile_ms}")

        started = time.perf_counter()
        log("importing torch + NeMo ...")
        import copy

        import torch
        from omegaconf import OmegaConf

        import nemo.collections.asr as nemo_asr
        from nemo.collections.asr.parts.submodules.rnnt_decoding import (
            RNNTDecodingConfig,
        )

        self._torch = torch
        torch.set_grad_enabled(False)
        torch.set_float32_matmul_precision("high")
        log(f"imports done in {time.perf_counter() - started:.1f}s")

        log(f"loading {model_name} onto {device} ...")
        load_started = time.perf_counter()
        self.model = nemo_asr.models.ASRModel.from_pretrained(
            model_name, map_location=torch.device(device)
        )
        self.model.eval()
        self.device = next(self.model.parameters()).device
        # Mirror the official streaming example's decoding setup.
        decoding_cfg = OmegaConf.structured(RNNTDecodingConfig(fused_batch_size=-1))
        self.model.change_decoding_strategy(decoding_cfg)
        log(f"model loaded in {time.perf_counter() - load_started:.1f}s")

        # Streaming preprocessor, configured the way CacheAwareStreamingAudioBuffer
        # does it (dither off, no length padding; this model uses normalize="NA").
        preprocessor_cfg = copy.deepcopy(self.model._cfg.preprocessor)
        OmegaConf.set_struct(preprocessor_cfg, False)
        preprocessor_cfg.dither = 0.0
        preprocessor_cfg.pad_to = 0
        self.preprocessor = self.model.from_config_dict(preprocessor_cfg)
        self.preprocessor.to(self.device)
        self.preprocessor.eval()
        self._num_features = self.model.encoder._feat_in

        self.profile_ms = None
        self._apply_profile(profile_ms)
        self._session = None

        warm_started = time.perf_counter()
        self._warmup()
        log(
            f"warmup done in {time.perf_counter() - warm_started:.1f}s; "
            f"ready after {time.perf_counter() - started:.1f}s total"
        )

    # ----- profile management -------------------------------------------------

    def _apply_profile(self, profile_ms: int) -> None:
        context = PROFILE_CONTEXTS[profile_ms]
        self.model.encoder.set_default_att_context_size(list(context))
        cfg = self.model.encoder.streaming_cfg
        self._chunk_frames = _as_pair(cfg.chunk_size)
        self._shift_frames = _as_pair(cfg.shift_size)
        self._pre_cache_frames = _as_pair(cfg.pre_encode_cache_size)
        self._drop_extra_pre_encoded = int(cfg.drop_extra_pre_encoded)
        if hasattr(self.model.encoder.pre_encode, "get_sampling_frames"):
            self._sampling_frames = _as_pair(
                self.model.encoder.pre_encode.get_sampling_frames()
            )
        else:
            self._sampling_frames = (1, 1)
        self.profile_ms = profile_ms
        log(
            f"profile {profile_ms}ms: att_context={context} "
            f"chunk={self._chunk_frames} pre_cache={self._pre_cache_frames} "
            f"drop_extra={self._drop_extra_pre_encoded}"
        )

    # ----- session lifecycle ----------------------------------------------------

    def start(self, profile_ms: int) -> None:
        if profile_ms not in PROFILE_CONTEXTS:
            raise ValueError(f"unsupported profile_ms: {profile_ms}")
        if profile_ms != self.profile_ms:
            self._apply_profile(profile_ms)
        cache_channel, cache_time, cache_channel_len = (
            self.model.encoder.get_initial_cache_state(batch_size=1)
        )
        self._session = {
            "samples": _SampleBuffer(),
            "features": None,  # torch [1, n_mels, F] on device
            "committed_frames": 0,
            "consumed_frames": 0,
            "step_num": 0,
            "cache_last_channel": cache_channel,
            "cache_last_time": cache_time,
            "cache_last_channel_len": cache_channel_len,
            "previous_hypotheses": None,
            "pred_out_stream": None,
            "last_text": "",
        }

    def accept_audio(self, samples) -> list:
        """Append audio, run all fully-buffered steps, return changed texts."""
        session = self._require_session()
        session["samples"].append(samples)
        self._extract_stable_features(session)
        return self._drain_steps(session, final=False)

    def finish(self) -> str:
        session = self._require_session()
        try:
            if session["samples"].length == 0 and session["step_num"] == 0:
                return ""
            padding = flush_padding_samples(
                session["samples"].length, self._chunk_frames[1]
            )
            session["samples"].append_zeros(padding)
            self._extract_stable_features(session)
            self._drain_steps(session, final=True)
            return session["last_text"]
        finally:
            self._session = None

    def cancel(self) -> None:
        self._session = None

    def _require_session(self):
        if self._session is None:
            raise RuntimeError("no active session in engine")
        return self._session

    # ----- feature extraction ----------------------------------------------------

    def _extract_stable_features(self, session) -> None:
        torch = self._torch
        total = session["samples"].length
        stable = stable_frame_count(total)
        committed = session["committed_frames"]
        if stable <= committed:
            return
        window_first_frame = max(0, committed - FEATURE_MARGIN_FRAMES)
        window_start = window_first_frame * HOP_SAMPLES
        audio = session["samples"].view(window_start, total)
        with torch.inference_mode():
            signal = torch.from_numpy(_np.ascontiguousarray(audio)).unsqueeze(0)
            signal = signal.to(self.device)
            signal_len = torch.tensor(
                [signal.size(-1)], dtype=torch.int64, device=self.device
            )
            features, _ = self.preprocessor(input_signal=signal, length=signal_len)
        new_frames = features[
            :, :, committed - window_first_frame : stable - window_first_frame
        ]
        if session["features"] is None:
            session["features"] = new_frames
        else:
            session["features"] = torch.cat(
                (session["features"], new_frames), dim=-1
            )
        session["committed_frames"] = stable

    # ----- encoder streaming steps -------------------------------------------------

    def _drain_steps(self, session, final: bool) -> list:
        """Run cache-aware steps over fully-buffered chunks; return changed texts."""
        texts = []
        while True:
            first_step = session["step_num"] == 0
            chunk = self._chunk_frames[0] if first_step else self._chunk_frames[1]
            shift = self._shift_frames[0] if first_step else self._shift_frames[1]
            min_frames = (
                self._sampling_frames[0] if first_step else self._sampling_frames[1]
            )
            available = session["committed_frames"] - session["consumed_frames"]
            if available <= 0:
                break
            if not final and available < chunk:
                break
            if final and available < min_frames:
                # Residual smaller than the subsampling window: with our zero
                # padding this is silence only; the official streaming buffer
                # drops such tails too.
                break
            is_last = final and (available - shift) < min_frames
            text = self._run_step(session, chunk, keep_all_outputs=is_last)
            session["consumed_frames"] += shift
            session["step_num"] += 1
            if text != session["last_text"]:
                session["last_text"] = text
                texts.append(text)
            if is_last:
                break
        return texts

    def _run_step(self, session, chunk_frames: int, keep_all_outputs: bool) -> str:
        torch = self._torch
        features = session["features"]
        start = session["consumed_frames"]
        chunk = features[:, :, start : start + chunk_frames]

        if session["step_num"] == 0:
            pre_cache_frames = self._pre_cache_frames[0]
            drop_extra_pre_encoded = 0
            pre_cache = torch.zeros(
                (1, self._num_features, pre_cache_frames),
                device=chunk.device,
                dtype=chunk.dtype,
            )
        else:
            pre_cache_frames = self._pre_cache_frames[1]
            drop_extra_pre_encoded = self._drop_extra_pre_encoded
            cache_start = max(0, start - pre_cache_frames)
            pre_cache = features[:, :, cache_start:start]
            missing = pre_cache_frames - pre_cache.size(-1)
            if missing > 0:
                zeros = torch.zeros(
                    (1, self._num_features, missing),
                    device=chunk.device,
                    dtype=chunk.dtype,
                )
                pre_cache = torch.cat((zeros, pre_cache), dim=-1)

        step_input = torch.cat((pre_cache, chunk), dim=-1)
        step_length = torch.tensor(
            [step_input.size(-1)], dtype=torch.int64, device=step_input.device
        )

        with torch.inference_mode():
            (
                pred_out_stream,
                transcribed_texts,
                cache_last_channel,
                cache_last_time,
                cache_last_channel_len,
                previous_hypotheses,
            ) = self.model.conformer_stream_step(
                processed_signal=step_input,
                processed_signal_length=step_length,
                cache_last_channel=session["cache_last_channel"],
                cache_last_time=session["cache_last_time"],
                cache_last_channel_len=session["cache_last_channel_len"],
                keep_all_outputs=keep_all_outputs,
                previous_hypotheses=session["previous_hypotheses"],
                previous_pred_out=session["pred_out_stream"],
                drop_extra_pre_encoded=drop_extra_pre_encoded,
                return_transcription=True,
            )

        session["pred_out_stream"] = pred_out_stream
        session["cache_last_channel"] = cache_last_channel
        session["cache_last_time"] = cache_last_time
        session["cache_last_channel_len"] = cache_last_channel_len
        session["previous_hypotheses"] = previous_hypotheses

        hypothesis = transcribed_texts[0]
        text = getattr(hypothesis, "text", hypothesis)
        return text if isinstance(text, str) else ""

    # ----- warmup -----------------------------------------------------------------

    def _warmup(self) -> None:
        """Run tiny streaming steps + a finish flush on zeros so the first real
        session hits no JIT/cudnn cold start."""
        warm_samples = (self._chunk_frames[0] + self._chunk_frames[1]) * HOP_SAMPLES
        warm_samples += STFT_GUARD_SAMPLES + HOP_SAMPLES
        self.start(self.profile_ms)
        self.accept_audio(_np.zeros(warm_samples, dtype=_np.float32))
        self.finish()
        if self.device.type == "cuda":
            self._torch.cuda.synchronize()


class SidecarServer:
    """Protocol layer: parses request lines, drives an engine, yields events.

    The engine is injectable so this layer is unit-testable without GPU/NeMo.
    """

    def __init__(self, engine):
        self.engine = engine
        self.active_session = None

    @staticmethod
    def _error(session_id, message: str) -> dict:
        return {"type": "error", "session_id": session_id, "message": message}

    @staticmethod
    def _valid_session_id(value) -> bool:
        return isinstance(value, int) and not isinstance(value, bool) and value >= 0

    def handle_line(self, line: str) -> list:
        """Process one request line; return the protocol events to emit."""
        line = line.strip()
        if not line:
            return []
        try:
            request = json.loads(line)
        except json.JSONDecodeError as error:
            return [self._error(None, f"invalid request: malformed JSON ({error})")]
        if not isinstance(request, dict):
            return [self._error(None, "invalid request: not a JSON object")]
        request_type = request.get("type")
        raw_session_id = request.get("session_id")
        session_id = raw_session_id if self._valid_session_id(raw_session_id) else None
        try:
            return self._dispatch(request, request_type, session_id)
        except Exception as error:  # never crash; surface and reset the session
            log(f"engine failure on {request_type!r}: {error!r}")
            self.active_session = None
            try:
                self.engine.cancel()
            except Exception:
                pass
            return [
                self._error(
                    session_id, f"engine failure: {type(error).__name__}: {error}"
                )
            ]

    def _dispatch(self, request: dict, request_type, session_id) -> list:
        if request_type == "health":
            return [{"type": "ready", "backend": self.engine.backend}]

        if request_type == "start_session":
            profile_ms = request.get("profile_ms")
            if session_id is None:
                return [self._error(None, "invalid request: start_session needs session_id")]
            if not isinstance(profile_ms, int) or isinstance(profile_ms, bool):
                return [
                    self._error(session_id, "invalid request: start_session needs profile_ms")
                ]
            if profile_ms not in PROFILE_CONTEXTS:
                supported = ", ".join(str(key) for key in sorted(PROFILE_CONTEXTS))
                return [
                    self._error(
                        session_id,
                        f"invalid request: unsupported profile_ms {profile_ms} "
                        f"(supported: {supported})",
                    )
                ]
            events = []
            if self.active_session is not None:
                self.engine.cancel()
                events.append(self._error(self.active_session, "superseded"))
                self.active_session = None
            self.engine.start(profile_ms)
            self.active_session = session_id
            events.append({"type": "session_started", "session_id": session_id})
            return events

        if request_type == "audio_chunk":
            if session_id is None or session_id != self.active_session:
                return [self._error(session_id, "invalid request: audio_chunk")]
            samples = request.get("samples")
            if not isinstance(samples, list):
                return [
                    self._error(session_id, "invalid request: samples must be an array")
                ]
            try:
                converted = samples_to_float32(samples)
            except (TypeError, ValueError):
                return [
                    self._error(
                        session_id, "invalid request: samples must be an array of i16"
                    )
                ]
            return [
                {"type": "partial", "session_id": session_id, "text": text}
                for text in self.engine.accept_audio(converted)
            ]

        if request_type == "finish_session":
            if session_id is None or session_id != self.active_session:
                return [self._error(session_id, "invalid request: finish_session")]
            text = self.engine.finish()
            self.active_session = None
            return [{"type": "final", "session_id": session_id, "text": text}]

        if request_type == "cancel_session":
            if session_id is None or session_id != self.active_session:
                return [self._error(session_id, "invalid request: cancel_session")]
            self.engine.cancel()
            self.active_session = None
            return [self._error(session_id, "session cancelled")]

        return [self._error(session_id, f"invalid request: {request_type}")]


def serve(server: SidecarServer, stdin, protocol_out) -> int:
    for line in stdin:
        for event in server.handle_line(line):
            protocol_out.write(json.dumps(event, separators=(",", ":")) + "\n")
            protocol_out.flush()
    log("stdin closed; exiting")
    return 0


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--device", default="cuda")
    parser.add_argument(
        "--profile-ms",
        type=int,
        default=160,
        choices=sorted(PROFILE_CONTEXTS),
        help="initial warm latency profile",
    )
    return parser


def main(argv=None) -> int:
    import os

    args = create_parser().parse_args(argv)

    # Reserve the real stdout for the protocol, then point fd 1 at stderr so
    # any stray library print (NeMo logs some INFO lines to stdout) is diverted.
    protocol_out = os.fdopen(os.dup(sys.stdout.fileno()), "w", buffering=1)
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr

    engine = NemotronEngine(
        model_name=args.model, device=args.device, profile_ms=args.profile_ms
    )
    server = SidecarServer(engine)
    return serve(server, sys.stdin, protocol_out)


if __name__ == "__main__":
    raise SystemExit(main())

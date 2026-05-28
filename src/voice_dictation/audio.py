"""Audio capture utilities.

Two concerns this module handles:
  1. Picking a working input device. On many Arch + PipeWire setups, PortAudio's
     'default' / 'pipewire' devices fail to open ('No such file or directory'
     during ALSA configure). We probe hw:* devices at 48 kHz / 44.1 kHz and use
     the first one that opens.
  2. Resampling to the model's 16 kHz target if the chosen device uses a
     different native rate.

The streaming pipeline uses pick_input() to choose a device at startup, then
opens an InputStream itself.
"""
from __future__ import annotations

import logging
import threading
from dataclasses import dataclass
from typing import Optional

import numpy as np
import sounddevice as sd

log = logging.getLogger(__name__)

SAMPLE_RATE = 16_000

CANDIDATE_RATES = (48_000, 44_100, 16_000)


@dataclass(frozen=True)
class InputChoice:
    device: object   # int, str, or whatever sounddevice accepts
    sample_rate: int
    name: str
    channels: int  # how many input channels device exposes
    needs_resample: bool

    def __str__(self) -> str:
        rs = " (→16k resample)" if self.needs_resample else ""
        return f"{self.name} @ {self.sample_rate}Hz dev={self.device!r}{rs}"


def _probe(device, sample_rate: int, channels: int = 1) -> bool:
    """Try to open and immediately close an InputStream with these params.
    Returns True on success.
    """
    try:
        def _cb(_indata, _frames, _t, _s):  # noqa: ANN001
            pass
        s = sd.InputStream(
            device=device,
            samplerate=sample_rate,
            channels=channels,
            dtype="float32",
            callback=_cb,
            blocksize=0,
        )
        s.start()
        s.stop()
        s.close()
        return True
    except Exception:  # noqa: BLE001
        return False


def pick_input(preferred_device: Optional[object] = None) -> InputChoice:
    """Find a working input device + sample rate.

    Preference order:
      1. The user's --input-device, if provided.
      2. sounddevice default at 16k (works on healthy PipeWire systems).
      3. sounddevice default at 48k.
      4. Each input-capable hw:X,Y, at 48k → 44.1k → 16k.
    """
    candidates: list[tuple[object, int, str]] = []
    if preferred_device is not None:
        for sr in CANDIDATE_RATES:
            candidates.append((preferred_device, sr, f"user:{preferred_device!r}"))

    # default first
    for sr in (16_000, 48_000, 44_100):
        candidates.append((None, sr, "default"))

    # specific hw devices
    try:
        devices = sd.query_devices()
        for i, d in enumerate(devices):
            if d.get("max_input_channels", 0) <= 0:
                continue
            name = d.get("name", "?")
            # skip 'monitor' devices (these are output loopbacks, not real inputs in our sense)
            if name.startswith("Monitor") or "monitor" in name.lower():
                continue
            # extract hw:X,Y from "HDA NVidia: ... (hw:2,0)" patterns
            if "(hw:" in name and name.endswith(")"):
                hwspec = name.rsplit("(", 1)[1][:-1]  # 'hw:2,0'
                for sr in CANDIDATE_RATES:
                    candidates.append((hwspec, sr, f"{name}"))
            else:
                # use the integer index as fallback
                for sr in CANDIDATE_RATES:
                    candidates.append((i, sr, name))
    except Exception:  # noqa: BLE001
        pass

    tried: set[tuple] = set()
    for dev, sr, name in candidates:
        key = (repr(dev), sr)
        if key in tried:
            continue
        tried.add(key)
        if _probe(dev, sr):
            log.info("input device chosen: %s @ %d Hz (dev=%r)", name, sr, dev)
            return InputChoice(
                device=dev,
                sample_rate=sr,
                name=name,
                channels=1,
                needs_resample=(sr != SAMPLE_RATE),
            )

    raise RuntimeError(
        "Could not open ANY input device. Tried: "
        + ", ".join(f"{n}@{sr}" for _, sr, n in candidates[:20])
    )


# ---- resampling (torchaudio) ----
# IMPORTANT: torchaudio's first call after CTranslate2 has claimed the CUDA
# context can crash the process (CUDA-context conflict on import-time lazy init).
# We force a tiny warmup call at module import — long BEFORE the daemon
# constructs the Transcriber — so torchaudio's lazy globals are all resolved
# while CUDA is still unclaimed.
_RESAMPLER_WARMED = False


def _warmup_torchaudio() -> None:
    global _RESAMPLER_WARMED
    if _RESAMPLER_WARMED:
        return
    try:
        import torch
        import torchaudio.functional as F
        t = torch.zeros(48, dtype=torch.float32)
        _ = F.resample(t, 48_000, 16_000)
        _RESAMPLER_WARMED = True
    except Exception:  # noqa: BLE001
        log.exception("torchaudio warmup failed; resample will fail at runtime")


_warmup_torchaudio()


def resample_to_16k(audio: np.ndarray, src_rate: int) -> np.ndarray:
    """Return audio resampled to 16 kHz (mono float32). No-op if already 16k."""
    if src_rate == SAMPLE_RATE:
        return audio
    if audio.size == 0:
        return audio
    import torch
    import torchaudio.functional as F
    t = torch.from_numpy(audio.astype("float32", copy=False)).contiguous()
    out = F.resample(t, src_rate, SAMPLE_RATE)
    return out.numpy()

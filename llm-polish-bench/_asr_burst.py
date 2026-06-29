#!/usr/bin/env python3
"""ASR GPU burst subprocess for the contention probe.

Modes:
  burst   : one model.generate() on 2.5s tone audio (simulates finish direct-final)
  stream N: N x 560ms transcribe_stream chunks (simulates sustained recording)

Prints a single "ASRBURST done elapsed=<ms>" line on stdout when the GPU job
finishes, so the parent can time the contention window precisely.
"""
from __future__ import annotations
import math, sys, time

def fake_audio(seconds: float, sr: int = 16000):
    n = int(seconds * sr)
    return [0.3 * math.sin(2 * math.pi * 220 * i / sr) for i in range(n)]

def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "burst"
    import mlx.core as mx
    from parakeet_mlx import from_pretrained
    from parakeet_mlx.audio import get_logmel
    model = from_pretrained("mlx-community/parakeet-tdt-0.6b-v3")
    audio = fake_audio(2.5)
    audio_mx = mx.array(audio, dtype=mx.float32)
    t0 = time.time()
    if mode == "burst":
        mel = get_logmel(audio_mx, model.preprocessor_config)
        res = model.generate(mel)
        txt = res[0].text if res else ""
        print(f"[asr] burst generate in {round((time.time()-t0)*1000)}ms", file=sys.stderr, flush=True)
    elif mode.startswith("stream"):
        n = int(mode.split()[1]) if " " in mode else 5
        cm = model.transcribe_stream()
        sp = cm.__enter__()
        for _ in range(n):
            sp.add_audio(mx.array(fake_audio(0.56), dtype=mx.float32))
        # finalize text access (triggers final decode)
        _ = sp.finalized_tokens if hasattr(sp, "finalized_tokens") else None
        try:
            cm.__exit__(None, None, None)
        except Exception:
            pass
        print(f"[asr] stream {n} chunks in {round((time.time()-t0)*1000)}ms", file=sys.stderr, flush=True)
    # signal parent
    print(f"ASRBURST done elapsed={round((time.time()-t0)*1000)}", flush=True)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())

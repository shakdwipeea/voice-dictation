# Plan: Switch macOS ASR to parakeet-mlx (parakeet-tdt-0.6b-v3)

## TL;DR recommendation

**Yes, this is worth doing.** parakeet-mlx runs the same-family NVIDIA Parakeet-TDT
model on Apple's **MLX/Metal** stack instead of NeMo+PyTorch on CPU. On M3 Max it
benchmarks at **~73x RTF, ~3 GB memory, 1.67% WER**, versus the current NeMo
offline-CPU path whose latency is dominated by a slow RNNT decode and the
well-known 16-32 s torch/NeMo warmup. The integration surface is small: the
existing sidecar protocol and `SidecarServer` are reusable, and `settings.rs`
already dispatches backends generically.

**One important naming correction:** parakeet-mlx cannot load raw NeMo
checkpoints such as `nvidia/parakeet-tdt-0.6b-v2` directly. It needs the
MLX-converted weights. Based on the M1 Pro benchmark below, the plan uses
`mlx-community/parakeet-tdt-0.6b-v3` as the default model.

## Current state (what we're replacing)

- Backend `nemotron_offline` -> `services/asr/nemotron_offline_sidecar.py`.
- Loads `nvidia/nemotron-speech-streaming-en-0.6b` via NeMo, **CPU** by default on
  macOS, MPS available but unstable.
- Whole-utterance: buffers float32 audio, writes temp WAV, calls
  `model.transcribe([path])` on session finish.
- Warmup 16-32 s (torch + NeMo import + load + 1s silence warmup).
- Engine contract: `start()/accept_audio(float32)/finish()/cancel()` + a
  `backend` string attr; no partials.
- `SidecarServer` (in `nemotron_sidecar.py`) owns the NDJSON stdin/stdout protocol
  -- reusable as-is.
- `apps/daemon/src/settings.rs::sidecar_command()` maps `backend` ->
  (python, script, args); has an `asr_device` validation ladder.

## Why parakeet-mlx should be faster on Mac

1. **MLX runs on the Metal/GPU stack**, not CPU-bound PyTorch. CPU was chosen for
   NeMo only because MPS op coverage for the streaming RNNT path is poor --
   irrelevant for TDT-on-MLX.
2. **Smaller, faster warmup**: no torch+NeMo megastack; mlx import + one
   `from_pretrained`. Expect warmup in the low single-digit seconds, not 16-32 s.
3. **TDT decoder** is faster than RNNT for whole-utterance; library ships native
   chunking + bf16.
4. **Bonus -- real streaming support**: `model.transcribe_stream()` exposes
   `add_audio()` + `finalized_tokens`/`draft_tokens`. This could restore **live
   partials** in the macOS overlay (which the offline backend deliberately
   dropped). For push-to-talk we can ship offline-first and treat streaming as a
   follow-up.

## Phase 0 benchmark result (M1 Pro)

Phase 0 was run on the target M1 Pro with MLX default device `Device(gpu, 0)`.
`parakeet-mlx` is installed in the existing `.venv-nemotron-mac` environment.
`ffmpeg` was installed for the benchmark/CLI file-loading path; the daemon's
current Parakeet-MLX hot path now uses direct PCM -> `get_logmel()` ->
`model.generate()` and does not require ffmpeg for normal dictation.

| Metric | v2 cached | v3 cached |
|---|---:|---:|
| Model | `mlx-community/parakeet-tdt-0.6b-v2` | `mlx-community/parakeet-tdt-0.6b-v3` |
| Load | 1.219s | 1.354s |
| Warmup | 8.305s | 1.218s |
| Peak RSS | ~1.07 GiB | ~1.61 GiB |
| p50 latency | 0.313s | 0.241s |
| p95/max latency | 0.665s | 0.613s |
| p50 RTF | 0.025 | 0.024 |
| p50 speed | ~39.9x realtime | ~41.6x realtime |
| Mean WER | 3.76% | 1.18% |

Artifacts:

- v2: `/tmp/sunoto-parakeet-bench.json`, `/tmp/sunoto-parakeet-bench.log`
- v3 first run: `/tmp/sunoto-parakeet-v3-bench.json`, `/tmp/sunoto-parakeet-v3-bench.log`
- v3 cached: `/tmp/sunoto-parakeet-v3-cached-bench.json`, `/tmp/sunoto-parakeet-v3-cached-bench.log`

Decision: **proceed with `mlx-community/parakeet-tdt-0.6b-v3`**. Cached load is
comparable to v2, warmup is much faster, runtime is slightly faster, and accuracy
is better on this sample.

## Risks / unknowns to keep in mind

- **Device comparison remains empirical across Macs.** The target M1 Pro result
  is strong, but other Apple Silicon chips should still run the Phase 0 script
  before changing defaults.
- **Float32 contract parity**: `SidecarServer` converts wire i16 -> float32 in
  [-1,1]. parakeet-mlx's `add_audio`/`load_audio` expects `mx.array`/numpy
  float at the model's `sample_rate` (16 kHz). Conversion is trivial but must be
  exact; guard with a test mirroring
  `test_accept_audio_converts_sidecar_float32_to_i16`.
- **VRAM/GPU safety rules**: MLX shares the unified memory/core GPU. The "never
  start a second Nemotron" rule applies analogously -- don't run the benchmark
  while the live daemon sidecar is warm.
- **Cold-first-utterance**: parakeet-mlx does eager decode per `add_audio`; we
  should warm the streaming context (or do one offline `transcribe` of silence)
  at startup like the NeMo warmup.

## Proposed work phases

### Phase 0 -- Spike / validate (complete)
1. Installed `parakeet-mlx` into the existing `.venv-nemotron-mac` environment.
2. Installed `ffmpeg` via Homebrew for the benchmark/CLI file-loading path.
3. Added `tools/phase0/parakeet_mlx_measure.py` for comparable load/warmup,
   latency, RTF, RSS, and WER measurements.
4. Benchmarked v2 and v3 on the target M1 Pro.
5. Decision: use `mlx-community/parakeet-tdt-0.6b-v3`.
6. Optimized the daemon sidecar hot path to bypass temp WAV + ffmpeg by feeding
   buffered 16 kHz PCM directly into parakeet-mlx's low-level `get_logmel()` +
   `model.generate()` API. Real protocol smoke on a cached LibriSpeech clip:
   4.82s audio decoded in 151ms, final text correct, `direct_pcm` path confirmed.

### Phase 1 -- Offline backend (`parakeet_mlx_offline`) -- shipping-ready, minimal risk
1. New sidecar `services/asr/parakeet_mlx_offline_sidecar.py`, subclassing the
   existing `_OfflineBufferEngine` from `nemotron_offline_sidecar.py` (import it
   -- the buffer/protocol core is already factored out and tested). The default
   finish path converts buffered i16 PCM directly to an MLX array, runs
   `get_logmel()`, then calls `model.generate(mel)` and returns `result.text`.
   `_transcribe_wav(path)` remains as a fallback only when optional
   `--chunk-duration` is enabled.
2. `BACKEND_NAME = "parakeet_mlx_offline"`; load via
   `from_pretrained("mlx-community/parakeet-tdt-0.6b-v3")` by default, with a
   `--model` override for experiments.
3. Warmup: transcribe 1 s of silence at startup (mirror existing `_warmup`).
4. New CLI args: `--model`, `--precision bf16|fp32`, (no `--device` needed -- MLX
   picks Metal/GPU + CPU automatically). Keep `--profile-ms` accepted-but-ignored
   for config parity (precedent in offline backend).
5. `settings.rs`:
   - Add `"parakeet_mlx_offline"` arm to `sidecar_command()` ->
     `services/asr/parakeet_mlx_offline_sidecar.py`.
   - Add to the backend-validation `match`.
   - `asr_device`: reuse the existing `Option<String>` but reject it for
     Parakeet-MLX backends because MLX selects compute units itself. Document
     this.
   - New optional `asr_model` field (string, default
     `mlx-community/parakeet-tdt-0.6b-v3`) so the model is config-switchable
     without code edits; plumb it to the script's `--model`.
6. Tests:
   - `tests/phase1/test_parakeet_mlx_offline_sidecar.py`: protocol-layer tests
     using a fake engine subclass of `_OfflineBufferEngine` (no MLX installed),
     mirroring `test_nemotron_offline_sidecar.py`. The float32->i16 invariant test
     must pass identically.
   - New `settings.rs` unit tests: `sidecar_command_supports_parakeet_mlx_offline`
     (mirrors the existing backend-selection tests).
7. `make test` + `cargo clippy --workspace -- -D warnings` + `cargo test --workspace`.
8. Manual end-to-end on macOS: set `backend="parakeet_mlx_offline"` in
   `~/Library/Application Support/sunoto/config.json`, restart the bare binary,
   confirm `ASR sidecar ready`, dictate, verify transcript + paste.
9. Update `docs/macos-recurring-issues.md` section 6 + AGENTS.md device notes:
   parakeet_mlx_offline is now the recommended macOS backend if the benchmark
   holds.

### Phase 2 -- Streaming partials via `transcribe_stream` (implemented, experimental)
1. Added `services/asr/parakeet_mlx_streaming_sidecar.py` with a hybrid
   streaming engine: `start()` opens a `model.transcribe_stream()` context;
   `accept_audio()` buffers daemon frames into configurable chunks, calls
   `add_audio()`, and emits `partial` protocol events when `transcriber.result`
   changes. On `finish_session`, the default `--final-mode direct` transcribes
   the full buffered utterance via direct PCM `get_logmel()` + `model.generate()`
   for offline-like final accuracy. There is still no temp WAV, no ffmpeg, and
   no file-based `model.transcribe(path)` fallback.
2. Wired `backend="parakeet_mlx_streaming"` in `settings.rs`, with `asr_model`
   override support and `asr_device` rejected (MLX selects compute units).
3. Exposed tuning knobs in the sidecar CLI: `--chunk-ms`, `--flush-ms`,
   `--min-final-ms`, `--left-context`, `--right-context`, `--depth`,
   `--keep-original-attention`, and `--final-mode direct|streaming`. Defaults:
   chunk 320ms, direct final, no silence flush, drop streaming residual tails
   below 160ms after at least one chunk. The no-flush default is intentional: a
   real smoke test showed current parakeet-mlx can emit pathological repeated
   `<unk>` output when decoding tiny residual chunks or trailing silence.
4. Tests pass. Real protocol smoke on the cached LibriSpeech clip emitted
   9 partials and the direct final recovered the offline-quality transcript
   (`Quilter`, not pure streaming's `Coulter`) with ~247ms final decode on
   4.82s audio. Treat this backend as experimental until live dictation and
   tuning prove it is worth making the default.

### Phase 3 -- Cleanup
1. If parakeet-mlx wins decisively on macOS, demote `nemotron_offline` to
   "legacy/available" in docs; keep Linux `nemotron` streaming code (Linux still
   uses the CUDA streaming backend).
2. Update `install.sh`/`README.md`/`docs/macos-port-plan.md` for the MLX
   dependency path. Normal dictation needs `parakeet-mlx`; `ffmpeg` is only
   needed for parakeet-mlx CLI/file benchmarks or the optional chunked
   `model.transcribe(path)` fallback.
3. Move the Phase 0 benchmark into `bin/` alongside the macOS verify scripts.

## Deliverables checklist
- [x] Phase 0 benchmark numbers (decision gate) -- passed on M1 Pro; v3 selected
- [x] `services/asr/parakeet_mlx_offline_sidecar.py`
- [x] `settings.rs` backend wiring + `asr_model` config field + unit tests
- [x] Protocol-layer tests (fake engine) reusing `_OfflineBufferEngine`
- [x] `cargo test --workspace --offline` + `cargo clippy --workspace --offline --all-targets -- -D warnings` green
- [x] Real sidecar health smoke (`ready` from `parakeet_mlx_offline`, MLX `Device(gpu, 0)`)
- [x] Direct-PCM protocol smoke (`get_logmel` + `model.generate`, no temp WAV/ffmpeg hot path)
- [x] Live macOS dictation verified for `parakeet_mlx_offline` via bare binary + `/tmp/sunoto-bare.log`
- [x] Experimental `parakeet_mlx_streaming` backend + fake/unit tests + real protocol smoke
- [ ] Live macOS dictation verified for `parakeet_mlx_streaming` via bare binary + `/tmp/sunoto-bare.log`
- [x] Docs: recurring-issues section 6, README, desktop config, AGENTS.md

## Parallel implementation workflow

Use the existing tmux session `sunoto-parakeet`:

- `bench`: **exclusive model-load lane**. Only run one real ASR/MLX/Nemotron
  process here at a time.
- `code`: implement the sidecar and settings changes.
- `tests`: run unit tests that do not load real ASR models.
- `logs`: watch `/tmp/sunoto-bare.log` and benchmark logs.
- `plan`: read this file.

Safe parallel work now:

1. In `code`: implement `services/asr/parakeet_mlx_offline_sidecar.py`.
2. In `code`/normal Pi edits: wire `apps/daemon/src/settings.rs`.
3. In `tests`: run Rust settings tests and fake-engine Python tests.
4. In `logs`: monitor only; do not start the daemon until the backend is built.

Unsafe parallel work:

- parakeet benchmark + daemon,
- parakeet sidecar + Nemotron sidecar,
- two parakeet sidecars,
- any live dictation daemon while a benchmark is loading a model.

## Open questions
1. Add the `asr_model` config field now, or hardcode v3 for the first pass and
   add configurability later?
2. Do you want Phase 2 (streaming partials) in scope now, or offline-only this
   round?

## Reference links
- parakeet-mlx: https://github.com/senstella/parakeet-mlx
- MLX model collection: https://huggingface.co/collections/mlx-community/parakeet
- `mlx-community/parakeet-tdt-0.6b-v3`:
  https://huggingface.co/mlx-community/parakeet-tdt-0.6b-v3
- `mlx-community/parakeet-tdt-0.6b-v2`:
  https://huggingface.co/mlx-community/parakeet-tdt-0.6b-v2
- perf/optimization notes (DeepWiki):
  https://deepwiki.com/senstella/parakeet-mlx/6.3-performance-and-memory-optimization

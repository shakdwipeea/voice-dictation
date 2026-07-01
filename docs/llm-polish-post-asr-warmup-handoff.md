# LLM Polish Post-ASR Warmup Handoff

## Purpose

This handoff describes the next implementation step for local LLM polish in
Sunoto on macOS.

The current best model remains:

```text
gemma-4-e2b-it-q4 GGUF via llama.cpp / Metal
```

Model file:

```text
models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf
```

Do not switch to MLX Gemma yet. First-pass benchmarks showed the current
llama.cpp/Metal GGUF backend is faster than the tested MLX candidates after a
real production-shaped warmup.

## User Constraints

These constraints are explicit and important:

- Do not send ASR partial transcripts to the LLM.
- Do not add speculative partial polishing.
- Do not add deterministic router/skipping as the latency strategy.
- Every non-empty final transcript must go through the LLM polish step.
- The goal is to reduce the actual LLM inference latency on macOS, not hide it.

The live architecture must remain:

```text
audio -> ASR final transcript -> LLM polish -> insertion
```

ASR partials are for overlay/display only.

## Performance Targets

Normal dictation release-to-insertion:

- p50 <= 700ms
- p95 <= 1000ms
- Any normal dictation above 1000ms is unacceptable unless the log explains
  which stage exceeded budget.

LLM polish request:

- p50 <= 300ms
- p95 <= 500ms
- Any request above 700ms must have enough diagnostics to explain whether it
  was prompt eval, generation, runtime warmup, or daemon wait.

## Current Findings

See:

```text
docs/llm-polish-mac-latency-results-2026-06-28.md
```

Important measured numbers:

| Runtime state | Clean | Repair | Long |
| --- | ---: | ---: | ---: |
| llama.cpp GGUF, LLM alone | p50 122ms / p95 157ms | p50 257ms / p95 262ms | p50 457ms / p95 460ms |
| llama.cpp GGUF, Parakeet loaded idle | p50 121ms / p95 157ms | p50 253ms / p95 262ms | p50 452ms / p95 456ms |
| llama.cpp GGUF, immediately after Parakeet final decode | p50 245ms / p95 261ms | not tested in this pass | not tested in this pass |
| MLX Gemma 3n E2B 4-bit, LLM alone | p50 821ms / p95 838ms | p50 982ms / p95 1023ms | p50 1178ms / p95 1241ms |
| MLX Gemma 3 1B 4-bit, LLM alone | p50 633ms / p95 637ms | p50 752ms / p95 764ms | p50 896ms / p95 944ms |

Conclusion:

- Parakeet loaded-and-idle did not slow the current LLM backend.
- A Parakeet final decode immediately before LLM did not reproduce the 5s
  LLM latency once the LLM had a real warmup.
- The first production-shaped llama.cpp request can take 4-5.5s.
- The current sidecar warmup is too small and/or happens at the wrong time.

The likely fix is:

```text
start Parakeet
wait for ASR ready
ensure LLM sidecar is ready
run production-shaped LLM warmup after ASR readiness
then accept dictation
```

## Current Problem To Fix

The daemon currently has a router/skipping mitigation that was added during
latency debugging. The user rejected this. Remove it.

Desired behavior:

```text
final transcript -> deterministic polish -> LLM polish -> insertion
```

Every non-empty final transcript should call the LLM polish sidecar.

The LLM sidecar also currently does a tiny startup generation warmup. That is
not sufficient. The warmup should use the same production prompt shape as real
requests and should happen after ASR readiness.

## Required Code Changes

### 1. Remove Deterministic Router Behavior

Remove daemon logic that skips LLM calls based on:

- no repair cues
- clean greeting
- filler/correction detection
- deterministic pipeline changes

The daemon may still skip LLM for an empty final transcript. Otherwise, if
`llm_polish_enabled` is true, call LLM.

Expected log for all non-empty final transcripts:

```text
session N: llm polish accepted in ...ms: "..." -> "..."
```

or, on failure:

```text
session N: llm polish skipped: <actual error>
```

No logs like this should remain:

```text
llm polish skipped by router: no repair cues
```

### 2. Keep And Improve Diagnostics

Do keep diagnostic logging. Diagnostics are for measurement only, not routing.

For each LLM request, log when available:

- request wall latency
- input chars
- input word count
- raw output chars
- cleaned output chars
- `finish_reason`
- `prompt_tokens`
- `completion_tokens`
- `total_tokens`
- `max_tokens`
- review flags
- hard validation flags if rejected

The current sidecar already reports some of these fields:

```text
finish_reason
max_tokens
raw_chars
prompt_tokens
completion_tokens
total_tokens
```

Make sure the daemon log includes them in a compact way for accepted requests.

### 3. Add Explicit LLM Warmup Protocol

Add a new sidecar request type:

```json
{"type":"warmup","texts":["Hey, how are you doing?","Her email is jane, no, janet dot smith at example dot com."]}
```

The sidecar should:

1. Run each warmup text through the same prompt/message path as real polish.
2. Use production `dynamic_tokens(text)`, not a tiny `max_tokens=4`.
3. Return one protocol event:

```json
{
  "type": "warmed",
  "latency_ms": 1234,
  "requests": [
    {
      "text": "Hey, how are you doing?",
      "latency_ms": 234,
      "raw_chars": 23,
      "completion_tokens": 7,
      "finish_reason": "stop"
    }
  ]
}
```

The warmup response should be parsed by the Rust client.

### 4. Change Warmup Timing In The Daemon

Current startup roughly does:

```text
spawn ASR sidecar
spawn LLM sidecar
LLM sidecar warms immediately
daemon starts accepting hotkey after ASR ready
```

Target startup:

```text
spawn ASR sidecar
spawn LLM sidecar and load model
wait for ASR ready
run LLM production-shaped warmup
mark dictation ready
```

The important rule:

```text
the final LLM warmup must happen after ASR sidecar ready
```

This ensures the first real dictation request does not pay the 4-5s first
production-shaped LLM warmup cost.

### 5. Gate Push-To-Talk Until Warm

If `llm_polish_enabled` is true, push-to-talk should be ignored until:

```text
ASR sidecar ready
LLM sidecar ready
post-ASR LLM warmup complete
```

The user-facing overlay/log should say something like:

```text
LLM polish still warming...
```

If LLM warmup fails, keep the existing fail-soft behavior:

- log a warning,
- disable LLM polish for that daemon run or mark it unavailable,
- do not crash the daemon.

But for the experiment, a warmup failure should be obvious in logs.

### 6. Startup Logs

Startup should clearly show the readiness order:

```text
LLM polish sidecar ready: gemma-4-e2b-it-q4, loaded in Xms
ASR sidecar ready: parakeet_mlx_streaming
LLM polish post-ASR warmup complete: total Yms, clean Ams, repair Bms
Sunoto ready for dictation
```

Avoid misleading readiness logs. The daemon should not imply the app is ready
for dictation until the required LLM warmup has completed when LLM polish is
enabled.

## Files Likely To Touch

Rust daemon:

- `apps/daemon/src/daemon.rs`
  - remove router usage
  - call post-ASR LLM warmup when ASR ready
  - gate hotkey until LLM warmed
  - improve logs

- `apps/daemon/src/llm_polish.rs`
  - add warmup request/response protocol
  - parse diagnostics
  - expose a `warmup()` method on the client

- `apps/daemon/src/settings.rs`
  - probably no required config change
  - only touch if adding configurable warmup texts or timeouts

Python sidecar:

- `services/polish/llm_polish_sidecar.py`
  - add `warmup` request handling
  - reuse same `run_completion()` path as real polish
  - return per-text warmup timings

- `services/polish/llm_polish_once.py`
  - probably no change unless shared diagnostic helpers are moved here

Tests:

- Add Rust unit tests for LLM warmup message parsing if practical.
- Add Python/protocol smoke for warmup request.
- Update any daemon tests affected by router removal.

Docs:

- `docs/llm-polish-mac-latency-plan.md`
- `docs/llm-polish-mac-latency-results-2026-06-28.md`
- This handoff file if results change.

## Verification Plan

### 1. Static/Unit Verification

Run:

```bash
.venv-llm-polish-mac/bin/python -m py_compile \
  services/polish/llm_polish_sidecar.py \
  services/polish/llm_polish_once.py

cargo test --workspace --offline
cargo clippy --workspace --offline --all-targets -- -D warnings
cargo build -p sunoto-daemon --release --offline
```

### 2. Direct LLM Sidecar Smoke

Start the LLM sidecar directly, read `ready`, send `warmup`, then send a
normal `polish` request:

```text
ready -> warmup -> warmed -> polish -> polished
```

Expected:

- warmup may take several seconds once,
- following clean polish should be around 100-300ms,
- following repair polish should be around 250-500ms.

### 3. Daemon Mock-ASR Smoke

Run daemon with mock ASR and LLM enabled. Confirm:

- LLM sidecar loads.
- ASR mock ready.
- post-ASR LLM warmup runs.
- first mock final transcript still goes through LLM.
- no router skip log appears.

### 4. Real Parakeet Live Test

Restart the macOS daemon correctly:

```bash
pkill -f 'target/release/sunoto-daemon run' || true
osascript -e 'tell application "Terminal" to do script "cd /Users/antash/workspace/voice-dictation && nohup target/release/sunoto-daemon run > /tmp/sunoto-bare.log 2>&1 & disown"'
```

Wait for:

```text
ASR sidecar ready
LLM polish post-ASR warmup complete
```

Then dictate:

1. Clean phrase:

   ```text
   Hey, how are you doing?
   ```

2. Repair phrase:

   ```text
   Her email is jane, no, janet dot smith at example dot com.
   ```

3. Longer normal phrase:

   ```text
   I wanted to follow up on the notes from yesterday and confirm that the next draft will be ready before lunch.
   ```

For each session, verify:

- ASR final logs.
- LLM accepted logs.
- LLM diagnostics logs.
- insertion logs.
- no router skip logs.

## Acceptance Criteria

Functional:

- Every non-empty final transcript goes through LLM.
- ASR partials do not go to LLM.
- No deterministic router/skipping remains.
- Daemon does not accept dictation until ASR ready and LLM post-ASR warmup is
  complete.

Latency:

- First real clean dictation after startup should no longer take 4-5s in LLM.
- Clean LLM request should be under 300ms in steady state.
- Repair LLM request should be under 500ms in steady state.
- Normal release-to-insertion should stay under 1000ms when ASR final is fast
  enough.

Logging:

- Startup logs show model load, ASR ready, post-ASR LLM warmup, and final ready.
- Slow requests above 700ms have token/finish/raw-length diagnostics.
- No misleading app-ready log before post-ASR LLM warmup.

## Runtime Note, 2026-06-29

Latency is now the priority, even if accuracy moves temporarily.

The LLM still runs for every non-empty final transcript. The latency change is
the sidecar output contract:

```text
UNCHANGED
```

for no-op transcripts, or:

```text
EDITED: <cleaned transcript>
```

when a rewrite is needed. This is not router/skipping behavior; it only asks the
model to generate fewer tokens. The daemon parses `UNCHANGED` back to the
original transcript before insertion and logs `output_mode=minimal` with the
normal timing diagnostics.

Model bakeoff status:

- Installed local GGUFs: Gemma E2B Q4, Qwen3.5 4B Q4, LFM2 2.6B Q4.
- Stored reports show LFM2.5 1.2B is very fast (`p50 61-74ms`) but that GGUF is
  not currently installed in `models/`.
- The installed macOS GGUF report favors Gemma over local LFM2 2.6B for latency:
  Gemma `p50 186ms / p95 608ms`; LFM2 2.6B `p50 894ms / p95 1241ms`.
- Current live daemon therefore keeps Gemma and uses minimal output mode first.

## Known Risks

- The baseline prompt sometimes drops greeting filler like `Hey,`.
  This is a correctness/prompt issue, not an inference-latency issue.
- If ASR final itself takes around 740ms, the LLM has only about 200ms left to
  keep release-to-insertion under 1000ms.
- The current llama.cpp first production-shaped request can take 4-5.5s; if
  post-ASR warmup does not absorb this, we need deeper llama.cpp timing
  instrumentation.
- MLX candidates tested so far were slower than llama.cpp/Metal. Do not switch
  runtime unless a later benchmark beats the current baseline in the same
  architecture.

## Related Files

- `docs/llm-polish-mac-latency-plan.md`
- `docs/llm-polish-mac-latency-results-2026-06-28.md`
- `docs/llm-polish-optimization-handoff.md`
- `services/polish/llm_polish_sidecar.py`
- `services/polish/llm_polish_once.py`
- `apps/daemon/src/llm_polish.rs`
- `apps/daemon/src/daemon.rs`
- `apps/daemon/src/settings.rs`
- `llm-polish-bench/out/macos-live-latency/`

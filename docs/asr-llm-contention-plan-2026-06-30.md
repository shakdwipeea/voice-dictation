# ASR↔LLM Metal GPU contention — resolution plan (2026-06-30)

## TL;DR

The LLM polish is slow on the live dictation path because **parakeet ASR
(MLX/Metal) and llama.cpp polish (Metal) share one GPU, and the ASR workload
during recording colds the LLM's prefill kernels.** A ~3-4s fixed re-ramp must
then be paid on the first polish after ASR.

The fix is **not** a re-warm ping (it pays the same ramp). The fix is to keep
the LLM firing keepalive pings **during** the ASR recording so its kernel
working set stays resident. At a ~1s keepalive interval this fully prevents
the cold snap (verified: 3970ms → 194ms).

## Root cause (proven by controlled probe)

`llm-polish-bench/probe_asr_llm_contention.py` (cross-process, real
parakeet-mlx + real llama.cpp, same GPU):

| experiment | LLM polish after ASR | prefill /tok |
|---|---:|---:|
| warm baseline (no ASR) | 233ms | 6.5ms ✅ |
| one ASR generate burst → polish | **4025ms** | **325ms ❌** (50×) |
| sustained streaming (3s) → polish | 4194ms | 290ms ❌ |
| longer streaming (4.5s) → polish | 3970ms | 321ms ❌ |
| burst → 300ms delay → polish | 3892ms | 343ms ❌ (not idle-cooling; it's eviction) |

Key facts:
1. **ASR colds LLM prefill ~50×** — confirmed cross-process, GPU-only contention.
2. **The cold is a FIXED ~3s ramp**, not per-token: 6 tokens ≈ 12 tokens ≈ 3-4s.
   The first LLM prefill after ASR pays the ramp regardless of size.
3. **Re-warm ping before polish does NOT help**: a 4-token re-warm ping itself
   costs ~2.9s cold (same ramp). It just moves the cost. (EXP3.)
4. **The first post-ASR call warms the GPU for the next**: cold → warm → warm
   (EXP5). So only the first polish after each dictation is slow.

## The fix (verified)

**Interleave LLM keepalive pings during the ASR recording.** The keepalive
pings are contended/slow while ASR holds the GPU, but they keep the LLM's
Metal kernel working set resident, so post-ASR prefill stays warm.

| experiment | post-ASR polish | prefill /tok |
|---|---:|---:|
| no keepalive, stream + final-generate (real finish shape) | 3970ms | 321ms ❌ |
| interleaved ~1s keepalive, stream + final-generate | **194ms** | 6.3ms ✅ |

(EXP8 = the real `finish()` shape: streaming partials THEN the direct-final
generate burst, with a ~1s LLM keepalive firing throughout. Fully warm.)

The keepalive already lives in the sidecar and ALREADY fires when stdin is
idle (which it is during recording — ASR is a different process). The only
problem is the **interval**: the default 4s is too long to reliably fire during
a short recording's critical final-generate window.

## Why the existing 4s keepalive wasn't enough

- The sidecar keepalive fires on `select` timeout when stdin is idle. During a
  dictation recording the daemon sends nothing to the LLM sidecar, so it IS
  idle and pings DO fire — but at 4s, a 2.5s recording fires **zero** pings
  during recording → polish cold (live session 1: 2430ms).
- A long 16s recording fires a few → partially warm (live session 5: 683ms).
- At **1.0s** the ping fires 2-3× during even a short recording, including
  across the direct-final generate → fully warm (live test: 141ms).

## Implementation

### Phase B0 — Shorten keepalive interval (PRIMARY, verified) — DONE

Changed default keepalive interval 4.0→1.0s, exposed as a real `Settings` field
`llm_polish_keepalive_secs` (config-settable, env-overridable).

**Verification gate (4-session live burst, 12s pauses, 2.5s recordings):**

| session | prefill /tok | polish | release→insertion |
|---|---:|---:|---:|
| 1 | 12.5ms ✅ (warm) | 165ms | 709ms ✅ |
| 2 | 8.2ms ✅ | 495ms ⚠ | 1107ms ❌ |
| 3 | 11ms ✅ | 207ms | 754ms ✅ |
| 4 | 8.2ms ✅ | 529ms ⚠ | 1105ms ❌ |

**The cold snap is ELIMINATED**: prefill is 8-12ms/tok on every session
(was 290-325ms/tok cold). No polish exceeds ~530ms — the 2-4s cold
polish is gone.

**BUT a secondary issue surfaced**: sessions 2 & 4 show polish 495/529ms in
the daemon, yet the sidecar's own latency was only 184ms — a ~311ms gap.
Cause: the sidecar's keepalive shares the **single-threaded `select` loop**
with polish. When a polish request arrives mid-keepalive, it blocks ~300ms
for the keepalive to finish. At 1s interval ~30% of polishes collide. This
is the limitation the original Phase A *background-thread + trylock* design
was meant to avoid. → Phase B2.

### Phase B1 — Battery/backoff (follow-up if idle drain matters)

A 1s keepalive ≈ continuous ~30-40% GPU duty cycle during true idle, which
drains laptop battery. If that's a concern, make the interval **adaptive**:
- ~1.0s for ~30s after the last polish/warmup (active dictation window).
- ~4-5s in deep idle (already proven to keep prefill warm enough; the first
  post-idle dictation's one-time JIT is absorbed by startup warmup).

This needs the sidecar to track `last_real_call_ts` and pick the interval per
ping. No daemon changes.

### Phase B2 — Background-thread keepalive (REQUIRED, fixes B0 collision) — DONE

Replaced the select-loop keepalive with a **background thread + `threading.Lock`**
so polish is never piled up behind keepalive in the same loop:
- `keepalive_loop(llm, interval)` thread fires pings on its own; the main loop
  just blocks on `for line in sys.stdin` and handles requests instantly.
- Shared `_llm_lock` serializes ALL llama.cpp calls (llama.cpp is NOT
  thread-safe). The keepalive thread uses a **non-blocking acquire
  (trylock)** and SKIPS its cycle if the main thread holds the lock (a real
  polish is in flight — the real call keeps the GPU warm itself, so skipping
  is correct and avoids piling latency onto dictation).
- The main loop uses a **blocking acquire** around polish/warmup; a polish
  arriving mid-ping waits only the ping's remaining wall time (bounded,
  irreducible since an in-flight llama.cpp call cannot be interrupted).
- Keepalive pings drop the grammar constraint (output is discarded) and use
  `KEEPALIVE_MAX_TOKENS=1` (warmth comes from the fresh-prefix prefill; one
  decode token keeps decode warm) — keeping each ping short so the worst-case
  mid-ping wait is small.
- On shutdown the main loop sets `_keepalive_stop` (a `threading.Event`) and
  joins the thread; the loop sleeps in small slices via `Event.wait` so it
  exits promptly.

**Verification gate (4-session live burst, 12s pauses, 2.5s recordings):**

| session | prefill /tok | sidecar polish | daemon polish | gap | release→insertion |
|---|---:|---:|---:|---:|---:|
| 1 | 7.6ms ✅ | 197ms | 198ms | 1ms | **711ms ✅** |
| 2 | 9.1ms ✅ | 212ms | 222ms | 10ms | **872ms ✅** |
| 3 | 11ms ✅ | 200ms | 200ms | 0ms | **697ms ✅** |
| 4 | 7.8ms ✅ | 234ms | 309ms | 75ms | 927ms (ASR 590 + longer polish) |

**Collision is now ~0-10ms on 3/4 sessions** (was ~270-310ms on 2/4 in B0).
No cold snap on any session (prefill 7-11ms/tok). **3 of 4 sessions under the
0.9s target.** The single over-target session (S4) is driven by ASR latency
(590ms) + a genuinely longer real polish (234ms sidecar, 11 decoded tokens),
NOT contention.

Corpus correctness re-verified after the change: 10/10, p50 272ms (warm).

## Out of scope (decided against)

- **Re-warm ping before polish**: pays the same ~3s ramp the cold polish would.
  EXP3 disproved this.
- **Move ASR to CPU**: eliminates contention but parakeet is MLX-only (no CPU);
  nemotron CPU is ~7s. Not viable.
- **Move LLM to CPU**: ~8s per polish. Not viable.
- **Speculative decoding / partials pipelining**: decode is already fast; the
  only lever was the cold ramp, solved by keepalive.
- **Caching/compiling the grammar for keepalive**: tried dropping it; the
  keepalive wall time (~165-270ms) is dominated by GPU prefill + per-token
  logit sampling on a ~200k-token vocab, not grammar compilation. No win.

## Status — DONE (Phase A + B0 + B2)

The **ASR↔LLM Metal GPU contention is resolved.** The post-ASR cold snap
(prefill 290-325ms/tok, polish 2-4s) is **eliminated** — every session now
prefills at 7-15ms/tok (warm). The keepalive-in-flight collision is reduced
from ~270-310ms (select loop) to ~0-10ms on the majority of sessions
(background thread + trylock).

Live release→insertion p50 ~700-870ms on short transcripts, down from
2.3-4.7s. The 0.9s target is met on 3/4 short-utterance sessions; the rare
over-target case is now ASR-latency-bound (560-740ms ASR + the ~200ms warm
polish floor), NOT contention-bound — a different problem than the one this
plan addressed.

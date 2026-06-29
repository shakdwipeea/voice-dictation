# LLM polish latency plan

## Goal

Make synchronous LLM polish fast enough (release→insertion p50 ≤ 0.9s) to run
on every transcript without a skip gate. Scope: **merge mid-utterance
self-corrections only**, not grammar/tense/punctuation.

## Architecture

```
Rust daemon (one event loop, owns hotkey+audio+insertion)
  │
  release ─► ASR sidecar (MLX parakeet, GPU) ─► ~0.5–0.8s final transcript
         ─► deterministic polish (Rust crate, ~0ms: caps, fillers, equal-swaps;
                                   defers unequal rewordings to LLM)
         ─► LLM polish sidecar (llama.cpp, Phi-4-mini Q5, Metal) ─► merge only
         ─► paste (pbcopy + Cmd+V, ~15ms)
```

macOS config: `parakeet_mlx_streaming`, `constrained_one_call`, grammar OFF,
LLM polish opt-in, 512MiB prompt cache, all Metal layers, flash_attn.

## Root cause (proven 2026-06-30, unique-transcript probe)

The bench hid the cost by repeating the same transcript → full prompt cache →
zero suffix prefill. Live utterances are unique, so the **transcript suffix
(5–23 new tokens) must be prefilled every call**:

- Decode is **always fast** (26ms/tok), idle or not.
- **Suffix prefill is the bottleneck and is GPU cold-ramp**:
  warm ~13ms/tok → 15s idle ~40ms/tok → 30s idle ~131–157ms/tok (~10×).
- This is why live was 1.7–3.9s while bench was ~300ms.
- **A decode-only keepalive (cached-prefix ping every ~4s) keeps prefill warm**:
  with it, suffix prefill stays ~13ms/tok even after 30s idle; without, 30s idle
  → ~157ms/tok. Confirmed experimentally.
- First-ever-call JIT (~2.3s) is already absorbed by the startup warmup.

## Plan

### Phase A — Keepalive heartbeat (the latency fix) — DONE

Keepalive runs **inside the Python sidecar** (`llm_polish_sidecar.py`): the
main loop switched from blocking `for line in sys.stdin` to a
`select.select(stdin, timeout=4s)` loop. When stdin is idle for 4s, it fires a
keepalive that sends a cached-prefix prompt with a **cycling suffix** (8 variants)
so ~1 new token must be prefilled each ping — exercising the prefill path,
not just decode. Keeps both prefill and decode Metal kernels warm.

Key design choices:
- Emits **nothing on stdout** — protocol stays clean, zero response-collision
  risk with daemon `polish()`/`warmup()` calls.
- Fires only after the first real request/warmup (`_keepalive_ready` flag).
- Interval configurable via `SUNOTO_LLM_POLISH_KEEPALIVE_S` (default 4.0; 0 = off).
- Logs `keepalive: latency=Xms pe=Yms/Ztok ev=…ms/…tok reused=…` to stderr.

No daemon Rust changes needed — the keepalive is entirely sidecar-internal.

#### Root cause confirmed (unique-transcript idle probe)

Decode is always fast (26ms/tok) regardless of idle. **Suffix prefill of new
tokens is the bottleneck**: Metal ramps prefill kernels independently of
decode. A decode-only keepalive kept decode warm but NOT prefill (idle-30s still
~157ms/tok). The cycling-suffix keepalive exercises prefill: idle-30s suffix
prefill dropped from **787ms→162ms** (4.9×).

#### Simulated live dictation (4 unique utterances, 20s pauses)

| session | LLM polish | est. release→insertion | under 0.9s? |
|--------:|-----------:|----------------------:|:---:|
| 1 | 256ms | ~747ms | ✓ |
| 2 | 398ms | ~889ms | ✓ |
| 3 | 370ms | ~861ms | ✓ |
| 4 | 319ms | ~810ms | ✓ |

All 4 under target. Before keepalive: 1.7–3.9s. Corpus bench still 10/10 correct
(all gates green) — keepalive didn't break correctness.

#### Verification gates
- `cargo test --workspace --offline`: 81 passed. clippy: clean.
- Python suites (phase0/1/2/ui): all OK (83 phase1 incl. 5 new keepalive tests).
- `git diff --check`: clean.

### Phase B — Accuracy (deferred, after latency)

Hallucination guard: session 5 invented "ship it on wednesday?" from an
abandoned clause ("should we sorry"). Prompt rule: abandoned/incomplete clause =
delete it, never complete it. Validator: reject output that adds new content
words not present in input (subsequence check).

### Phase C — Promotion gate (final)

25-session real-dictation run; only then set `llm_polish_enabled: true` as the
macOS default.

## Rejected approaches

- **Deterministic skip gate** (twice): can't detect unequal rewordings → silent
  data corruption. Rejected.
- **Speculative decoding / partials pipelining**: not needed. Decode is already
  fast; the only lever was cold-ramp, solved by keepalive.
- **Quant sweep Q5→Q4**: unnecessary; latency is not generation-bound.

## Measurements

### Phase 5 — narrowed prompt + deterministic unequal-reword deferral

`constrained_one_call`, Phi-4-mini Q5, warm corpus N=5/case, unique transcripts.

Overall warm p50 **299ms**; all 10 cases correct (9 exact + 1 safely preserved);
all latency gates green. The 4 Phase-1 correctness mismatches (clean_long,
clean_dict_term, reword_unequal, reword_discourse) and both latency-gate
failures all fixed. Reused tokens = full prefix; generation 1–9 tokens.

### Live (the gap keepalive must close)

| session | ASR | LLM polish | release→insertion |
|--------:|----:|----------:|------------------:|
| 3 | 478ms | 2897ms | 3389ms |
| 4 | 530ms | 1734ms | 2293ms |
| 5 | 819ms | 3884ms | 4725ms |
| 6 | 536ms | 2366ms | 2919ms |

Live slowness = suffix-prefill cold-ramp (see root cause), not ASR or decode.

## Verification gates

`cargo test --workspace --offline`, `cargo clippy --workspace --offline
--all-targets -- -D warnings`, `make test` (incl. Python suites), `git diff
--check`.

## Repo context

- Model: `models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf`
- Sidecar: `services/polish/llm_polish_sidecar.py` (stdin loop; `_llama_ctx`,
  `reset_llama_timings`/`llama_timings`/`log_timings`; `constrained_payload`)
- Prompt: `services/polish/llm_polish_once.py`
- Daemon spawn/client: `apps/daemon/src/llm_polish.rs` (`LlmPolishClient::spawn`,
  `warmup`, `polish`); `daemon.rs` (spawn+post-ASR warmup, `polish` control cmd)
- Config: `apps/daemon/src/settings.rs` (`llm_polish_*`); env overrides in `main.rs`
- Deterministic: `crates/sunoto-polish/src/lib.rs` (`apply_swap_marker`)
- Bench/probes: `llm-polish-bench/bench_corpus_phase1.py`,
  `probe_idle_latency2.py`, `corpus-live-20260630.jsonl`
- venv: `.venv-llm-polish-mac`; live log: `/tmp/sunoto-daemon.log`
- Branch: `llm-polish-research`

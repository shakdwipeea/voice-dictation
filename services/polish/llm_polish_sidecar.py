#!/usr/bin/env python3
"""Persistent local LLM polish sidecar for Sunoto daemon trials.

Protocol is newline-delimited JSON over stdin/stdout:

  {"type":"polish","session_id":1,"text":"..."}
  {"type":"polished","session_id":1,"text":"...","latency_ms":123,...}
  {"type":"warmup","texts":["Hey, how are you doing?"]}
  {"type":"warmed","latency_ms":123,"requests":[...]}

Model loading happens once at startup. Progress and llama.cpp diagnostics go to
stderr so stdout remains clean protocol JSON.
"""

from __future__ import annotations

import json
import os
import sys
import threading
import time
from typing import Any

try:
    from llama_cpp import Llama, LlamaGrammar, LlamaRAMCache
except ImportError:  # pragma: no cover - exercised only with older llama-cpp-python.
    from llama_cpp import Llama, LlamaRAMCache

    LlamaGrammar = None  # type: ignore[assignment]

from llm_polish_once import (
    clean_constrained_output,
    clean_model_output,
    constrained_max_tokens,
    constrained_messages_for,
    decision_max_tokens,
    decision_messages_for,
    dynamic_tokens,
    messages_for,
    model_label,
    model_path,
    output_mode,
    parse_decision_output,
    polish_mode,
    rewrite_messages_for,
    validate,
    word_tokens,
)

CONSTRAINED_OUTPUT_GRAMMAR = r'''
root ::= ok | edit
ok ::= "OK"
edit ::= "EDIT: " text
text ::= [^\n]+
'''


def emit(event: dict[str, object]) -> None:
    print(json.dumps(event, separators=(",", ":")), flush=True)


def log(message: str) -> None:
    print(f"[llm-polish-sidecar] {message}", file=sys.stderr, flush=True)


def env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, str(default)))
    except ValueError:
        return default


def env_bool(name: str, default: bool) -> bool:
    value = os.environ.get(name)
    if value is None:
        return default
    return value.strip().lower() not in {"0", "false", "no", "off"}


def llama_runtime_config() -> dict[str, object]:
    return {
        "n_gpu_layers": env_int("SUNOTO_LLM_POLISH_GPU_LAYERS", -1),
        "n_ctx": env_int("SUNOTO_LLM_POLISH_CTX", 2048),
        "n_batch": env_int("SUNOTO_LLM_POLISH_BATCH", 512),
        "n_ubatch": env_int("SUNOTO_LLM_POLISH_UBATCH", 512),
        "n_threads": env_int("SUNOTO_LLM_POLISH_THREADS", 8),
        "flash_attn": env_bool("SUNOTO_LLM_POLISH_FLASH_ATTN", True),
        "seed": env_int("SUNOTO_LLM_POLISH_SEED", 42),
    }


class DiagnosticLlamaRAMCache(LlamaRAMCache):
    """RAM prompt cache with per-request hit/miss diagnostics."""

    def __init__(self, capacity_bytes: int) -> None:
        super().__init__(capacity_bytes=capacity_bytes)
        self.last_lookup: dict[str, object] | None = None
        self.last_save_tokens: int | None = None

    @staticmethod
    def _common_prefix_len(left: tuple[int, ...], right: tuple[int, ...]) -> int:
        count = 0
        for left_token, right_token in zip(left, right):
            if left_token != right_token:
                break
            count += 1
        return count

    def begin_request(self) -> None:
        self.last_lookup = None
        self.last_save_tokens = None

    def __getitem__(self, key):  # type: ignore[no-untyped-def]
        key_tuple = tuple(key)
        match = self._find_longest_prefix_key(key_tuple)
        if match is None:
            self.last_lookup = {
                "cache_hit": False,
                "cache_prompt_tokens": len(key_tuple),
                "cache_matched_tokens": 0,
            }
            raise KeyError("Key not found")
        self.last_lookup = {
            "cache_hit": True,
            "cache_prompt_tokens": len(key_tuple),
            "cache_matched_tokens": self._common_prefix_len(match, key_tuple),
        }
        return super().__getitem__(key)

    def __setitem__(self, key, value) -> None:  # type: ignore[no-untyped-def]
        self.last_save_tokens = len(tuple(key))
        super().__setitem__(key, value)

    def diagnostics(self) -> dict[str, object]:
        diagnostics = dict(
            self.last_lookup
            or {
                "cache_hit": None,
                "cache_prompt_tokens": None,
                "cache_matched_tokens": None,
            }
        )
        diagnostics.update(
            {
                "cache_saved_tokens": self.last_save_tokens,
                "cache_entries": len(self.cache_state),
                "cache_size_bytes": self.cache_size,
            }
        )
        return diagnostics


def load_model() -> Llama:
    path = model_path()
    if not path.is_file():
        raise SystemExit(f"model file not found: {path}")
    label = model_label(path)
    log(f"loading model: {path}")
    started = time.time()
    runtime_config = llama_runtime_config()
    log(
        "llama.cpp runtime: "
        + " ".join(f"{key}={value}" for key, value in runtime_config.items())
    )
    llm = Llama(
        model_path=str(path),
        verbose=False,
        logits_all=False,
        **runtime_config,
    )
    load_ms = round((time.time() - started) * 1000)
    cache_mib = env_int("SUNOTO_LLM_POLISH_CACHE_MIB", 512)
    if cache_mib > 0:
        cache = DiagnosticLlamaRAMCache(capacity_bytes=cache_mib * 1024 * 1024)
        llm.set_cache(cache)
        log(f"prompt cache enabled: {cache_mib}MiB RAM")
    else:
        log("prompt cache disabled")
    log(
        f"model loaded in {load_ms}ms; "
        f"polish_mode={polish_mode()} output_mode={output_mode()}"
    )
    emit(
        {
            "type": "ready",
            "backend": label,
            "load_ms": load_ms,
        }
    )
    return llm


def run_chat_completion(
    llm: Llama,
    messages: list[dict[str, str]],
    max_tokens: int,
    grammar: object | None = None,
) -> dict:
    kwargs: dict[str, object] = {
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": float(os.environ.get("SUNOTO_LLM_POLISH_TEMPERATURE", "0.1")),
        "top_k": int(os.environ.get("SUNOTO_LLM_POLISH_TOP_K", "50")),
        "top_p": float(os.environ.get("SUNOTO_LLM_POLISH_TOP_P", "0.95")),
        "repeat_penalty": float(os.environ.get("SUNOTO_LLM_POLISH_REPEAT_PENALTY", "1.05")),
    }
    if grammar is not None:
        kwargs["grammar"] = grammar
    return llm.create_chat_completion(**kwargs)


def run_completion(llm: Llama, transcript: str, max_tokens: int | None = None) -> dict:
    mode = output_mode()
    active_max_tokens = max_tokens if max_tokens is not None else dynamic_tokens(transcript, mode)
    return run_chat_completion(llm, messages_for(transcript, mode), active_max_tokens)


def begin_cache_request(llm: Llama) -> None:
    cache = getattr(llm, "cache", None)
    if isinstance(cache, DiagnosticLlamaRAMCache):
        cache.begin_request()


def cache_diagnostics(llm: Llama) -> dict[str, object]:
    cache = getattr(llm, "cache", None)
    if isinstance(cache, DiagnosticLlamaRAMCache):
        return cache.diagnostics()
    return {
        "cache_hit": None,
        "cache_prompt_tokens": None,
        "cache_matched_tokens": None,
        "cache_saved_tokens": None,
        "cache_entries": None,
        "cache_size_bytes": None,
    }


def _llama_ctx(llm: Llama) -> object | None:
    ctx = getattr(llm, "_ctx", None)
    if ctx is None:
        return None
    # Newer llama-cpp-python wraps the raw llama_context_p in a context
    # manager; the real ctypes pointer lives at .ctx. Fall back to the
    # object itself for bindings that expose the pointer directly.
    raw = getattr(ctx, "ctx", None)
    return raw if raw is not None else ctx


def reset_llama_timings(llm: Llama) -> None:
    """Reset the cumulative llama.cpp perf counters before a completion.

    The legacy name is kept for callers; the implementation now uses the
    real C API (llama_perf_context_reset) because Llama._ctx is a ctypes
    pointer with no Python "reset_timings" method (the previous lookup was a
    silent no-op, so the timings shown below never appeared in the log).
    """
    ctx = _llama_ctx(llm)
    if ctx is None:
        return
    try:
        from llama_cpp import llama_perf_context_reset
    except ImportError:
        return
    try:
        llama_perf_context_reset(ctx)
    except Exception:
        pass


def llama_timings(llm: Llama) -> dict[str, object]:
    """Read llama.cpp perf counters: prompt eval vs generation vs cache reuse.

    Returns {} if the binding is unavailable so a mismatch never breaks the
    polish path. Fields:
      prompt_eval_ms / prompt_eval_tokens - prompt (input) processing
      eval_ms / eval_tokens              - generation (output) processing
      reused_tokens                      - tokens served from prompt cache
      load_ms                            - model load (cumulative; ref)
    """
    ctx = _llama_ctx(llm)
    if ctx is None:
        return {}
    try:
        from llama_cpp import llama_perf_context
    except ImportError:
        return {}
    try:
        data = llama_perf_context(ctx)
    except Exception:
        return {}
    if data is None:
        return {}
    return {
        "prompt_eval_ms": round(float(data.t_p_eval_ms), 1),
        "prompt_eval_tokens": int(data.n_p_eval),
        "eval_ms": round(float(data.t_eval_ms), 1),
        "eval_tokens": int(data.n_eval),
        "reused_tokens": int(data.n_reused),
        "load_ms": round(float(data.t_load_ms), 1),
    }


def log_timings(label: str, perf: dict[str, object]) -> None:
    if not perf:
        return
    log(
        f"{label}: llama-perf "
        f"prompt_eval={perf['prompt_eval_ms']}ms/{perf['prompt_eval_tokens']}tok "
        f"eval={perf['eval_ms']}ms/{perf['eval_tokens']}tok "
        f"reused={perf['reused_tokens']}"
    )


def print_llama_timings(llm: Llama) -> dict[str, object]:
    perf = llama_timings(llm)
    log_timings("llama-perf", perf)
    return perf


def cache_label(payload: dict[str, Any]) -> str:
    hit = payload.get("cache_hit")
    if hit is True:
        return f"hit matched={payload.get('cache_matched_tokens')}"
    if hit is False:
        return "miss"
    return "unavailable"


def log_completion(label: str, payload: dict[str, Any]) -> None:
    log(
        f"{label}: latency={payload['latency_ms']}ms "
        f"mode={payload['output_mode']} "
        f"input_chars={payload['input_chars']} input_words={payload['input_words']} "
        f"prompt_tokens={payload.get('prompt_tokens')} "
        f"completion_tokens={payload.get('completion_tokens')} "
        f"total_tokens={payload.get('total_tokens')} "
        f"max_tokens={payload['max_tokens']} finish={payload.get('finish_reason')} "
        f"raw_chars={payload['raw_chars']} cleaned_chars={payload['cleaned_chars']} "
        f"cache={cache_label(payload)} cache_entries={payload.get('cache_entries')} "
        f"cache_size_bytes={payload.get('cache_size_bytes')}"
    )


def usage_payload(response: dict) -> dict[str, object]:
    usage = response.get("usage") or {}
    return {
        "prompt_tokens": usage.get("prompt_tokens"),
        "completion_tokens": usage.get("completion_tokens"),
        "total_tokens": usage.get("total_tokens"),
    }


def sum_optional_numbers(payloads: list[dict[str, object] | None], key: str) -> int | None:
    values = [payload.get(key) for payload in payloads if payload is not None]
    numbers = [value for value in values if isinstance(value, int)]
    return sum(numbers) if numbers else None


def prefixed_edited_raw(raw: str, fallback: str) -> str:
    text = raw.strip() or fallback.strip()
    if text:
        return f"EDITED: {text}"
    return "EDITED:"


def constrained_output_grammar() -> object | None:
    if os.environ.get("SUNOTO_LLM_POLISH_GRAMMAR", "0").strip().lower() not in {
        "1",
        "true",
        "yes",
        "on",
    }:
        return None
    if LlamaGrammar is None:
        return None
    return LlamaGrammar.from_string(CONSTRAINED_OUTPUT_GRAMMAR)


def completion_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    started = time.time()
    mode = output_mode()
    max_tokens = dynamic_tokens(transcript, mode)
    begin_cache_request(llm)
    reset_llama_timings(llm)
    response = run_completion(llm, transcript, max_tokens=max_tokens)
    perf = llama_timings(llm)
    raw = response["choices"][0]["message"].get("content") or ""
    text = clean_model_output(raw, transcript, mode)
    validation = validate(transcript, text)
    usage = response.get("usage") or {}
    payload = {
        "text": text,
        "raw_output": raw.strip(),
        "latency_ms": round((time.time() - started) * 1000),
        "output_mode": mode,
        "input_chars": len(transcript),
        "input_words": len(word_tokens(transcript)),
        "max_tokens": max_tokens,
        "finish_reason": response["choices"][0].get("finish_reason"),
        "raw_chars": len(raw),
        "cleaned_chars": len(text),
        **usage_payload(response),
        **cache_diagnostics(llm),
        **validation,
        "llama_perf": perf,
    }
    log_completion(label, payload)
    log_timings(label, perf)
    return payload


def constrained_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    started = time.time()
    max_tokens = constrained_max_tokens(transcript)
    begin_cache_request(llm)
    reset_llama_timings(llm)
    response = run_chat_completion(
        llm,
        constrained_messages_for(transcript),
        max_tokens,
        grammar=constrained_output_grammar(),
    )
    perf = llama_timings(llm)
    raw = response["choices"][0]["message"].get("content") or ""
    text, decision_label, malformed = clean_constrained_output(raw, transcript)
    validation = validate(transcript, text)
    validation_rejected = bool(validation.get("hard_unsafe"))
    if validation_rejected:
        text = transcript
        validation = validate(transcript, text)
    payload: dict[str, object] = {
        "text": text,
        "raw_output": raw.strip(),
        "latency_ms": round((time.time() - started) * 1000),
        "output_mode": "constrained_one_call",
        "polish_mode": "constrained_one_call",
        "input_chars": len(transcript),
        "input_words": len(word_tokens(transcript)),
        "max_tokens": max_tokens,
        "finish_reason": response["choices"][0].get("finish_reason"),
        "raw_chars": len(raw),
        "cleaned_chars": len(text),
        "decision_label": "UNCHANGED" if decision_label == "OK" else decision_label,
        "decision_malformed": malformed,
        "rewrite_called": decision_label == "EDIT",
        "validation_rejected": validation_rejected,
        **usage_payload(response),
        **cache_diagnostics(llm),
        **validation,
        "llama_perf": perf,
    }
    log_completion(label, payload)
    log_timings(label, perf)
    return payload


def decision_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    started = time.time()
    max_tokens = decision_max_tokens()
    begin_cache_request(llm)
    reset_llama_timings(llm)
    response = run_chat_completion(llm, decision_messages_for(transcript), max_tokens)
    perf = llama_timings(llm)
    raw = response["choices"][0]["message"].get("content") or ""
    decision = parse_decision_output(raw)
    payload: dict[str, object] = {
        "decision": decision,
        "decision_malformed": decision is None,
        "raw_output": raw.strip(),
        "latency_ms": round((time.time() - started) * 1000),
        "output_mode": "decision",
        "input_chars": len(transcript),
        "input_words": len(word_tokens(transcript)),
        "max_tokens": max_tokens,
        "finish_reason": response["choices"][0].get("finish_reason"),
        "raw_chars": len(raw),
        "cleaned_chars": len(decision or ""),
        **usage_payload(response),
        **cache_diagnostics(llm),
        "llama_perf": perf,
    }
    log_completion(label, {**payload, "hard_unsafe": [], "review_flags": []})
    log_timings(label, perf)
    return payload


def rewrite_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    started = time.time()
    max_tokens = dynamic_tokens(transcript, "full")
    begin_cache_request(llm)
    reset_llama_timings(llm)
    response = run_chat_completion(llm, rewrite_messages_for(transcript), max_tokens)
    perf = llama_timings(llm)
    raw = response["choices"][0]["message"].get("content") or ""
    text = clean_model_output(raw, transcript, "full")
    validation = validate(transcript, text)
    payload: dict[str, object] = {
        "text": text,
        "raw_output": raw.strip(),
        "latency_ms": round((time.time() - started) * 1000),
        "output_mode": "rewrite",
        "input_chars": len(transcript),
        "input_words": len(word_tokens(transcript)),
        "max_tokens": max_tokens,
        "finish_reason": response["choices"][0].get("finish_reason"),
        "raw_chars": len(raw),
        "cleaned_chars": len(text),
        **usage_payload(response),
        **cache_diagnostics(llm),
        **validation,
        "llama_perf": perf,
    }
    log_completion(label, payload)
    log_timings(label, perf)
    return payload


def two_step_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    started = time.time()
    decision = decision_payload(llm, transcript, f"{label} decision")
    decision_label = decision.get("decision")
    rewrite: dict[str, object] | None = None
    validation_rejected = False

    if decision_label == "UNCHANGED":
        text = transcript
        raw_output = "UNCHANGED"
        validation = validate(transcript, text)
    else:
        rewrite = rewrite_payload(llm, transcript, f"{label} rewrite")
        rewrite_text = str(rewrite.get("text") or "")
        rewrite_hard_unsafe = rewrite.get("hard_unsafe")
        if isinstance(rewrite_hard_unsafe, list) and rewrite_hard_unsafe:
            validation_rejected = True
            text = transcript
            validation = validate(transcript, text)
        else:
            text = rewrite_text
            validation = {
                "hard_unsafe": rewrite.get("hard_unsafe") or [],
                "review_flags": rewrite.get("review_flags") or [],
            }
        raw_output = prefixed_edited_raw(str(rewrite.get("raw_output") or ""), rewrite_text)

    call_payloads = [decision, rewrite]
    last_call = rewrite or decision
    payload: dict[str, object] = {
        "text": text,
        "raw_output": raw_output,
        "latency_ms": round((time.time() - started) * 1000),
        "output_mode": "two_step",
        "polish_mode": "two_step",
        "input_chars": len(transcript),
        "input_words": len(word_tokens(transcript)),
        "max_tokens": sum_optional_numbers(call_payloads, "max_tokens"),
        "finish_reason": last_call.get("finish_reason"),
        "raw_chars": len(raw_output),
        "cleaned_chars": len(text),
        "prompt_tokens": sum_optional_numbers(call_payloads, "prompt_tokens"),
        "completion_tokens": sum_optional_numbers(call_payloads, "completion_tokens"),
        "total_tokens": sum_optional_numbers(call_payloads, "total_tokens"),
        "cache_hit": last_call.get("cache_hit"),
        "cache_prompt_tokens": last_call.get("cache_prompt_tokens"),
        "cache_matched_tokens": last_call.get("cache_matched_tokens"),
        "cache_saved_tokens": last_call.get("cache_saved_tokens"),
        "cache_entries": last_call.get("cache_entries"),
        "cache_size_bytes": last_call.get("cache_size_bytes"),
        "decision_label": decision_label,
        "decision_malformed": bool(decision.get("decision_malformed")),
        "rewrite_called": rewrite is not None,
        "validation_rejected": validation_rejected,
        "decision": decision,
        "rewrite": rewrite,
        **validation,
    }
    log(
        f"{label}: two_step total={payload['latency_ms']}ms "
        f"decision={decision_label or 'MALFORMED'} "
        f"rewrite_called={payload['rewrite_called']} "
        f"validation_rejected={validation_rejected}"
    )
    return payload


def polish_payload(llm: Llama, transcript: str, label: str) -> dict[str, object]:
    if polish_mode() == "constrained_one_call":
        return constrained_payload(llm, transcript, label)
    if polish_mode() == "two_step":
        return two_step_payload(llm, transcript, label)
    return completion_payload(llm, transcript, label)


_keepalive_text: str | None = None
_keepalive_ready = False
_keepalive_counter = 0
# Serializes ALL llama.cpp calls. llama.cpp is NOT thread-safe, so the
# background keepalive thread and the main request loop must never run a
# completion concurrently. The keepalive thread uses a non-blocking acquire
# (trylock) and SKIPS its turn when a real polish/warmup is in flight; the
# main loop uses a blocking acquire so a just-started keepalive (unavoidable,
# since an in-flight llama.cpp call cannot be interrupted) blocks polish only
# for the remainder of that ping (~<=keepalive wall time).
_llm_lock = threading.Lock()
# Signals the keepalive thread to exit on shutdown.
_keepalive_stop = threading.Event()
KEEPALIVE_FALLBACK_TEXT = "Hey, how are you doing?"
# Cap generated tokens for keepalive pings. The ping's job is to warm the
# prefill + decode paths, not to produce a real rewrite. Warmth comes from the
# fresh-prefix prefill (the monotonic counter suffix); a single decoded token
# is enough to keep decode kernels warm while keeping each ping short (smaller
# ping = smaller worst-case collision wait if a real polish arrives mid-ping).
# Real polish decode stays fast because real calls always prefill fresh transcript
# tokens (keeping Metal high-power).
KEEPALIVE_MAX_TOKENS = 1


def keepalive_interval_s() -> float:
    try:
        value = float(os.environ.get("SUNOTO_LLM_POLISH_KEEPALIVE_S", "1.0"))
    except ValueError:
        return 1.0
    return value if value >= 0 else 0.0


def keepalive(llm: Llama) -> None:
    """Cached-prefix ping to keep Metal GPU kernels warm.

    Uses a transcript already processed during warmup so the prompt prefix is
    cached, but appends a MONOTONIC counter so the suffix tokens are fresh on
    every ping — guaranteeing a short prefill (a few new tokens) each time.
    This matters because a fully-cached, peek-only ping (prompt_eval=0) does
    almost no GPU compute, so Metal downclocks between pings and decode goes
    cold (observed: 600ms-2000ms for 2 tokens). A small but real prefill each
    ping keeps the GPU in its high-power state, so both prefill AND decode
    stay fast (~26ms/tok). Does NOT emit on stdout (protocol stays clean);
    logs llama-perf to stderr. The background keepalive thread calls this;
    it holds `_llm_lock` for the duration so it never overlaps a real polish.
    """
    global _keepalive_ready, _keepalive_counter
    if not _keepalive_ready:
        return
    base = _keepalive_text or KEEPALIVE_FALLBACK_TEXT
    _keepalive_counter += 1
    text = f"{base} {_keepalive_counter}."
    started = time.time()
    begin_cache_request(llm)
    reset_llama_timings(llm)
    try:
        # No grammar constraint on the ping: its output is discarded, and
        # skipping grammar compilation keeps each ping short (smaller keepalive
        # wall time = smaller worst-case collision wait if a real polish
        # arrives mid-ping). Warmth comes from the fresh-prefix prefill + a
        # short decode, both of which run regardless of the grammar.
        run_chat_completion(
            llm,
            constrained_messages_for(text),
            KEEPALIVE_MAX_TOKENS,
        )
    except Exception as error:
        log(f"keepalive error: {error}")
        return
    perf = llama_timings(llm)
    latency_ms = round((time.time() - started) * 1000)
    log(
        f"keepalive: latency={latency_ms}ms "
        f"pe={perf.get('prompt_eval_ms')}ms/{perf.get('prompt_eval_tokens')}tok "
        f"ev={perf.get('eval_ms')}ms/{perf.get('eval_tokens')}tok "
        f"reused={perf.get('reused_tokens')}"
    )


def keepalive_loop(llm: Llama, interval: float) -> None:
    """Background thread: fire a keepalive ping every `interval` seconds.

    Uses a non-blocking lock acquire (trylock): if a real polish/warmup is in
    flight on the main thread, this cycle is SKIPPED entirely (the real call
    is keeping the GPU warm itself, so skipping is correct and avoids piling
    latency onto dictation). Only when the lock is free does a ping run; it
    then holds the lock for the ping's duration (~<=few hundred ms), so a
    polish arriving mid-ping is bounded by the ping's remaining wall time.
    Exits cleanly when `_keepalive_stop` is set on shutdown.
    """
    while not _keepalive_stop.is_set():
        # Sleep in small slices so shutdown is responsive.
        if _keepalive_stop.wait(interval):
            return
        if not _keepalive_ready:
            continue
        # trylock: skip this cycle if the main thread is mid-polish; never
        # block the keepalive thread waiting on the main thread (that would
        # just defer, not avoid, the ping and pile up).
        if not _llm_lock.acquire(blocking=False):
            continue
        try:
            keepalive(llm)
        finally:
            _llm_lock.release()


def polish(llm: Llama, session_id: int, transcript: str) -> None:
    global _keepalive_ready
    _keepalive_ready = True
    # Hold the lock so the background keepalive thread cannot run a llama.cpp
    # call concurrently (llama.cpp is not thread-safe). A keepalive in flight
    # when this acquires will have released first; the bounded wait is at most
    # the remaining keepalive wall time.
    with _llm_lock:
        payload = polish_payload(llm, transcript, f"polish session={session_id}")
    emit(
        {
            "type": "polished",
            "session_id": session_id,
            **payload,
        }
    )


def warmup(llm: Llama, texts: list[str]) -> None:
    global _keepalive_text, _keepalive_ready
    if texts and not _keepalive_text:
        _keepalive_text = texts[0]
    _keepalive_ready = True
    started = time.time()
    requests: list[dict[str, object]] = []
    with _llm_lock:
        for index, text in enumerate(texts):
            payload = polish_payload(llm, text, f"warmup[{index}]")
            requests.append(
                {
                    "text": text,
                    "latency_ms": payload["latency_ms"],
                    "output_mode": payload["output_mode"],
                    "input_chars": payload["input_chars"],
                    "input_words": payload["input_words"],
                    "max_tokens": payload["max_tokens"],
                    "raw_chars": payload["raw_chars"],
                    "cleaned_chars": payload["cleaned_chars"],
                    "prompt_tokens": payload["prompt_tokens"],
                    "completion_tokens": payload["completion_tokens"],
                    "total_tokens": payload["total_tokens"],
                    "cache_hit": payload["cache_hit"],
                    "cache_prompt_tokens": payload["cache_prompt_tokens"],
                    "cache_matched_tokens": payload["cache_matched_tokens"],
                    "cache_saved_tokens": payload["cache_saved_tokens"],
                    "cache_entries": payload["cache_entries"],
                    "cache_size_bytes": payload["cache_size_bytes"],
                    "finish_reason": payload["finish_reason"],
                    "hard_unsafe": payload["hard_unsafe"],
                    "review_flags": payload["review_flags"],
                    "decision_label": payload.get("decision_label"),
                    "decision_malformed": payload.get("decision_malformed"),
                    "rewrite_called": payload.get("rewrite_called"),
                    "validation_rejected": payload.get("validation_rejected"),
                    "llama_perf": payload.get("llama_perf"),
                }
            )
    emit(
        {
            "type": "warmed",
            "latency_ms": round((time.time() - started) * 1000),
            "requests": requests,
        }
    )


def main() -> int:
    llm = load_model()
    interval = keepalive_interval_s()
    keepalive_thread: threading.Thread | None = None
    if interval > 0:
        # Background keepalive fires pings on its own; the main loop just
        # blocks on stdin and handles requests. The shared `_llm_lock`
        # serializes all llama.cpp calls (not thread-safe).
        keepalive_thread = threading.Thread(
            target=keepalive_loop,
            args=(llm, interval),
            name="llm-keepalive",
            daemon=True,
        )
        keepalive_thread.start()
    try:
        # Blocking read: stdin is idle during ASR recording, so the keepalive
        # thread (not this loop) keeps Metal warm. A request is handled the
        # instant it arrives (no select-timeout latency).
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            request: object = None
            try:
                request = json.loads(line)
                if not isinstance(request, dict):
                    raise ValueError("request is not an object")
                request_type = request.get("type")
                if request_type == "shutdown":
                    return 0
                if request_type == "warmup":
                    texts = request.get("texts")
                    if not isinstance(texts, list) or not all(
                        isinstance(text, str) for text in texts
                    ):
                        raise ValueError("warmup requires texts as a list of strings")
                    warmup(llm, texts)
                    continue
                if request_type != "polish":
                    raise ValueError(f"unknown request type: {request_type!r}")
                session_id = request.get("session_id")
                text = request.get("text")
                if not isinstance(session_id, int) or not isinstance(text, str):
                    raise ValueError("polish requires integer session_id and string text")
                polish(llm, session_id, text)
            except Exception as error:
                emit(
                    {
                        "type": "error",
                        "session_id": request.get("session_id") if isinstance(request, dict) else None,
                        "message": str(error),
                    }
                )
    finally:
        _keepalive_stop.set()
        if keepalive_thread is not None:
            keepalive_thread.join(timeout=2.0)
    log("stdin closed; exiting")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

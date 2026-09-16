//! Human and JSON renderings of the LLM polish sidecar's diagnostics, for
//! the session log line and the control-socket `polish` reply.

use crate::llm_polish;

pub(crate) fn llm_diagnostics_json(
    diagnostics: &llm_polish::LlmPolishDiagnostics,
) -> serde_json::Value {
    serde_json::json!({
        "polish_mode": diagnostics.polish_mode,
        "output_mode": diagnostics.output_mode,
        "input_chars": diagnostics.input_chars,
        "input_words": diagnostics.input_words,
        "finish_reason": diagnostics.finish_reason,
        "max_tokens": diagnostics.max_tokens,
        "raw_chars": diagnostics.raw_chars,
        "cleaned_chars": diagnostics.cleaned_chars,
        "prompt_tokens": diagnostics.prompt_tokens,
        "completion_tokens": diagnostics.completion_tokens,
        "total_tokens": diagnostics.total_tokens,
        "cache_hit": diagnostics.cache_hit,
        "cache_prompt_tokens": diagnostics.cache_prompt_tokens,
        "cache_matched_tokens": diagnostics.cache_matched_tokens,
        "cache_saved_tokens": diagnostics.cache_saved_tokens,
        "cache_entries": diagnostics.cache_entries,
        "cache_size_bytes": diagnostics.cache_size_bytes,
        "decision_label": diagnostics.decision_label,
        "decision_malformed": diagnostics.decision_malformed,
        "rewrite_called": diagnostics.rewrite_called,
        "decision": diagnostics
            .decision
            .as_ref()
            .map(llm_call_diagnostics_json),
        "rewrite": diagnostics
            .rewrite
            .as_ref()
            .map(llm_call_diagnostics_json),
        "llama_perf": diagnostics.llama_perf.as_ref().map(llama_perf_json),
        "ttft_ms": diagnostics.ttft_ms,
        "streamed": diagnostics.streamed,
        "stream_chunks": diagnostics.stream_chunks,
    })
}

pub(crate) fn llama_perf_json(perf: &llm_polish::LlamaPerf) -> serde_json::Value {
    serde_json::json!({
        "prompt_eval_ms": perf.prompt_eval_ms,
        "prompt_eval_tokens": perf.prompt_eval_tokens,
        "eval_ms": perf.eval_ms,
        "eval_tokens": perf.eval_tokens,
        "reused_tokens": perf.reused_tokens,
        "load_ms": perf.load_ms,
    })
}

pub(crate) fn llm_call_diagnostics_json(
    call: &llm_polish::LlmPolishCallDiagnostics,
) -> serde_json::Value {
    serde_json::json!({
        "decision": call.decision,
        "decision_malformed": call.decision_malformed,
        "text": call.text,
        "raw_output": call.raw_output,
        "latency_ms": call.latency_ms,
        "output_mode": call.output_mode,
        "input_chars": call.input_chars,
        "input_words": call.input_words,
        "finish_reason": call.finish_reason,
        "max_tokens": call.max_tokens,
        "raw_chars": call.raw_chars,
        "cleaned_chars": call.cleaned_chars,
        "prompt_tokens": call.prompt_tokens,
        "completion_tokens": call.completion_tokens,
        "total_tokens": call.total_tokens,
        "cache_hit": call.cache_hit,
        "cache_prompt_tokens": call.cache_prompt_tokens,
        "cache_matched_tokens": call.cache_matched_tokens,
        "cache_saved_tokens": call.cache_saved_tokens,
        "cache_entries": call.cache_entries,
        "cache_size_bytes": call.cache_size_bytes,
        "llama_perf": call.llama_perf.as_ref().map(llama_perf_json),
    })
}

pub(crate) fn format_llm_warmup_summary(outcome: &llm_polish::LlmPolishWarmupOutcome) -> String {
    let labels = ["clean", "repair"];
    let mut timings = vec![format!("total {}ms", outcome.latency_ms)];
    let mut details = Vec::new();
    for (index, request) in outcome.requests.iter().enumerate() {
        let label =
            labels
                .get(index)
                .copied()
                .unwrap_or(if index == 0 { "warmup" } else { "extra" });
        timings.push(format!("{label} {}ms", request.latency_ms));
        let mut request_details = vec![
            format!("input_chars={}", request.text.chars().count()),
            format!("latency={}ms", request.latency_ms),
        ];
        if let Some(mode) = request.output_mode.as_deref() {
            request_details.push(format!("output_mode={mode}"));
        }
        if let Some(chars) = request.raw_chars {
            request_details.push(format!("raw_chars={chars}"));
        }
        if let Some(chars) = request.cleaned_chars {
            request_details.push(format!("cleaned_chars={chars}"));
        }
        if let Some(tokens) = request.completion_tokens {
            request_details.push(format!("completion_tokens={tokens}"));
        }
        if let Some(reason) = request.finish_reason.as_deref() {
            request_details.push(format!("finish={reason}"));
        }
        if let Some(hit) = request.cache_hit {
            request_details.push(format!("cache={}", if hit { "hit" } else { "miss" }));
        }
        if let Some(tokens) = request.cache_matched_tokens {
            request_details.push(format!("cache_matched_tokens={tokens}"));
        }
        if let Some(entries) = request.cache_entries {
            request_details.push(format!("cache_entries={entries}"));
        }
        if let Some(bytes) = request.cache_size_bytes {
            request_details.push(format!("cache_size_bytes={bytes}"));
        }
        details.push(format!("{label}: {}", request_details.join(", ")));
    }
    if details.is_empty() {
        timings.join(", ")
    } else {
        format!("{} [{}]", timings.join(", "), details.join("; "))
    }
}

pub(crate) fn format_llm_diagnostics(diagnostics: &llm_polish::LlmPolishDiagnostics) -> String {
    let mut parts = Vec::new();
    if let Some(mode) = diagnostics.polish_mode.as_deref() {
        parts.push(format!("polish_mode={mode}"));
    }
    if let Some(mode) = diagnostics.output_mode.as_deref() {
        parts.push(format!("output_mode={mode}"));
    }
    if diagnostics.decision_malformed == Some(true) {
        parts.push("decision=MALFORMED".to_string());
    } else if let Some(decision) = diagnostics.decision_label.as_deref() {
        parts.push(format!("decision={decision}"));
    }
    if let Some(rewrite_called) = diagnostics.rewrite_called {
        parts.push(format!("rewrite_called={rewrite_called}"));
    }
    if let Some(decision) = diagnostics.decision.as_ref() {
        if let Some(ms) = decision.latency_ms {
            parts.push(format!("decision_latency={ms}ms"));
        }
        if let Some(tokens) = decision.completion_tokens {
            parts.push(format!("decision_completion_tokens={tokens}"));
        }
    }
    if let Some(rewrite) = diagnostics.rewrite.as_ref() {
        if let Some(ms) = rewrite.latency_ms {
            parts.push(format!("rewrite_latency={ms}ms"));
        }
        if let Some(tokens) = rewrite.completion_tokens {
            parts.push(format!("rewrite_completion_tokens={tokens}"));
        }
    }
    if let Some(chars) = diagnostics.input_chars {
        parts.push(format!("input_chars={chars}"));
    }
    if let Some(ttft) = diagnostics.ttft_ms {
        parts.push(format!("ttft={ttft}ms"));
    }
    if let Some(streamed) = diagnostics.streamed {
        parts.push(format!("streamed={streamed}"));
    }
    if let Some(chunks) = diagnostics.stream_chunks {
        parts.push(format!("stream_chunks={chunks}"));
    }
    if let Some(words) = diagnostics.input_words {
        parts.push(format!("input_words={words}"));
    }
    if let Some(reason) = diagnostics.finish_reason.as_deref() {
        parts.push(format!("finish={reason}"));
    }
    if let Some(tokens) = diagnostics.completion_tokens {
        parts.push(format!("completion_tokens={tokens}"));
    }
    if let Some(tokens) = diagnostics.prompt_tokens {
        parts.push(format!("prompt_tokens={tokens}"));
    }
    if let Some(tokens) = diagnostics.total_tokens {
        parts.push(format!("total_tokens={tokens}"));
    }
    if let Some(max_tokens) = diagnostics.max_tokens {
        parts.push(format!("max_tokens={max_tokens}"));
    }
    if let Some(chars) = diagnostics.raw_chars {
        parts.push(format!("raw_chars={chars}"));
    }
    if let Some(chars) = diagnostics.cleaned_chars {
        parts.push(format!("cleaned_chars={chars}"));
    }
    if let Some(hit) = diagnostics.cache_hit {
        parts.push(format!("cache={}", if hit { "hit" } else { "miss" }));
    }
    if let Some(tokens) = diagnostics.cache_prompt_tokens {
        parts.push(format!("cache_prompt_tokens={tokens}"));
    }
    if let Some(tokens) = diagnostics.cache_matched_tokens {
        parts.push(format!("cache_matched_tokens={tokens}"));
    }
    if let Some(tokens) = diagnostics.cache_saved_tokens {
        parts.push(format!("cache_saved_tokens={tokens}"));
    }
    if let Some(entries) = diagnostics.cache_entries {
        parts.push(format!("cache_entries={entries}"));
    }
    if let Some(bytes) = diagnostics.cache_size_bytes {
        parts.push(format!("cache_size_bytes={bytes}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join(", "))
    }
}

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

pub const WARMUP_TEXTS: [&str; 2] = [
    "Hey, how are you doing?",
    "Her email is jane, no, janet dot smith at example dot com.",
];

/// llama.cpp perf counters captured per completion call.
/// Populated from the sidecar's `llama_perf` payload; `None` fields when the
/// binding did not report them (older sidecar).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LlamaPerf {
    #[serde(default)]
    pub prompt_eval_ms: Option<f64>,
    #[serde(default)]
    pub prompt_eval_tokens: Option<i64>,
    #[serde(default)]
    pub eval_ms: Option<f64>,
    #[serde(default)]
    pub eval_tokens: Option<i64>,
    #[serde(default)]
    pub reused_tokens: Option<i64>,
    #[serde(default)]
    pub load_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LlmPolishOutcome {
    pub text: String,
    pub raw_output: Option<String>,
    pub latency_ms: u64,
    pub review_flags: Vec<String>,
    pub diagnostics: LlmPolishDiagnostics,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmPolishDiagnostics {
    pub polish_mode: Option<String>,
    pub output_mode: Option<String>,
    pub input_chars: Option<u64>,
    pub input_words: Option<u64>,
    pub finish_reason: Option<String>,
    pub max_tokens: Option<u64>,
    pub raw_chars: Option<u64>,
    pub cleaned_chars: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_hit: Option<bool>,
    pub cache_prompt_tokens: Option<u64>,
    pub cache_matched_tokens: Option<u64>,
    pub cache_saved_tokens: Option<u64>,
    pub cache_entries: Option<u64>,
    pub cache_size_bytes: Option<u64>,
    pub decision_label: Option<String>,
    pub decision_malformed: Option<bool>,
    pub rewrite_called: Option<bool>,
    pub validation_rejected: Option<bool>,
    pub decision: Option<LlmPolishCallDiagnostics>,
    pub rewrite: Option<LlmPolishCallDiagnostics>,
    pub llama_perf: Option<LlamaPerf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LlmPolishCallDiagnostics {
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub decision_malformed: Option<bool>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub raw_output: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub output_mode: Option<String>,
    #[serde(default)]
    pub input_chars: Option<u64>,
    #[serde(default)]
    pub input_words: Option<u64>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub raw_chars: Option<u64>,
    #[serde(default)]
    pub cleaned_chars: Option<u64>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cache_hit: Option<bool>,
    #[serde(default)]
    pub cache_prompt_tokens: Option<u64>,
    #[serde(default)]
    pub cache_matched_tokens: Option<u64>,
    #[serde(default)]
    pub cache_saved_tokens: Option<u64>,
    #[serde(default)]
    pub cache_entries: Option<u64>,
    #[serde(default)]
    pub cache_size_bytes: Option<u64>,
    #[serde(default)]
    pub hard_unsafe: Vec<String>,
    #[serde(default)]
    pub review_flags: Vec<String>,
    #[serde(default)]
    pub llama_perf: Option<LlamaPerf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmPolishWarmupOutcome {
    pub latency_ms: u64,
    pub requests: Vec<LlmPolishWarmupRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmPolishWarmupRequest {
    pub text: String,
    pub latency_ms: u64,
    pub output_mode: Option<String>,
    pub raw_chars: Option<u64>,
    pub cleaned_chars: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub finish_reason: Option<String>,
    pub cache_hit: Option<bool>,
    pub cache_matched_tokens: Option<u64>,
    pub cache_entries: Option<u64>,
    pub cache_size_bytes: Option<u64>,
}

pub struct LlmPolishClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<LlmPolishMessage>,
    reader: Option<JoinHandle<()>>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LlmPolishRequest<'a> {
    Polish { session_id: u64, text: &'a str },
    Warmup { texts: &'a [&'a str] },
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LlmPolishEvent {
    Ready {
        backend: String,
        load_ms: u64,
        #[serde(default)]
        warmup_ms: u64,
    },
    Polished {
        session_id: u64,
        text: String,
        latency_ms: u64,
        #[serde(default)]
        raw_output: Option<String>,
        #[serde(default)]
        hard_unsafe: Vec<String>,
        #[serde(default)]
        review_flags: Vec<String>,
        #[serde(default)]
        polish_mode: Option<String>,
        #[serde(default)]
        output_mode: Option<String>,
        #[serde(default)]
        input_chars: Option<u64>,
        #[serde(default)]
        input_words: Option<u64>,
        #[serde(default)]
        finish_reason: Option<String>,
        #[serde(default)]
        max_tokens: Option<u64>,
        #[serde(default)]
        raw_chars: Option<u64>,
        #[serde(default)]
        cleaned_chars: Option<u64>,
        #[serde(default)]
        prompt_tokens: Option<u64>,
        #[serde(default)]
        completion_tokens: Option<u64>,
        #[serde(default)]
        total_tokens: Option<u64>,
        #[serde(default)]
        cache_hit: Option<bool>,
        #[serde(default)]
        cache_prompt_tokens: Option<u64>,
        #[serde(default)]
        cache_matched_tokens: Option<u64>,
        #[serde(default)]
        cache_saved_tokens: Option<u64>,
        #[serde(default)]
        cache_entries: Option<u64>,
        #[serde(default)]
        cache_size_bytes: Option<u64>,
        #[serde(default)]
        decision_label: Option<String>,
        #[serde(default)]
        decision_malformed: Option<bool>,
        #[serde(default)]
        rewrite_called: Option<bool>,
        #[serde(default)]
        validation_rejected: Option<bool>,
        #[serde(default)]
        decision: Option<LlmPolishCallDiagnostics>,
        #[serde(default)]
        rewrite: Option<LlmPolishCallDiagnostics>,
        #[serde(default)]
        llama_perf: Option<LlamaPerf>,
    },
    Warmed {
        latency_ms: u64,
        #[serde(default)]
        requests: Vec<LlmPolishWarmupEvent>,
    },
    Error {
        session_id: Option<u64>,
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct LlmPolishWarmupEvent {
    text: String,
    latency_ms: u64,
    #[serde(default)]
    output_mode: Option<String>,
    #[serde(default)]
    raw_chars: Option<u64>,
    #[serde(default)]
    cleaned_chars: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    cache_hit: Option<bool>,
    #[serde(default)]
    cache_matched_tokens: Option<u64>,
    #[serde(default)]
    cache_entries: Option<u64>,
    #[serde(default)]
    cache_size_bytes: Option<u64>,
}

#[derive(Debug)]
enum LlmPolishMessage {
    Event(Box<LlmPolishEvent>),
    Garbage(String),
    Closed,
}

impl LlmPolishClient {
    pub fn spawn(settings: &Settings) -> Result<Self, String> {
        let (program, args, envs) = settings.llm_polish_command();
        let mut child = Command::new(&program)
            .args(&args)
            .envs(
                envs.iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("cannot spawn LLM polish sidecar {program:?}: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "LLM polish sidecar stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "LLM polish sidecar stdout unavailable".to_string())?;
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(LlmPolishMessage::Closed);
                        return;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        let message = match serde_json::from_str::<LlmPolishEvent>(&trimmed) {
                            Ok(event) => LlmPolishMessage::Event(Box::new(event)),
                            Err(_) => LlmPolishMessage::Garbage(trimmed),
                        };
                        if tx.send(message).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            rx,
            reader: Some(reader),
        };
        client.wait_ready(Duration::from_millis(settings.llm_polish_timeout_ms))?;
        Ok(client)
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<(), String> {
        match self.recv(timeout)? {
            LlmPolishMessage::Event(event) => match *event {
                LlmPolishEvent::Ready {
                    backend,
                    load_ms,
                    warmup_ms,
                } => {
                    if warmup_ms == 0 {
                        crate::logging::info(&format!(
                            "LLM polish sidecar ready: {backend}, loaded in {load_ms}ms"
                        ));
                    } else {
                        crate::logging::info(&format!(
                            "LLM polish sidecar ready: {backend}, loaded in {load_ms}ms, legacy startup warmup {warmup_ms}ms"
                        ));
                    }
                    Ok(())
                }
                LlmPolishEvent::Error { message, .. } => {
                    Err(format!("LLM polish sidecar startup error: {message}"))
                }
                other => Err(format!(
                    "LLM polish sidecar sent unexpected startup message: {other:?}"
                )),
            },
            LlmPolishMessage::Garbage(line) => Err(format!(
                "LLM polish sidecar printed non-protocol line: {line}"
            )),
            LlmPolishMessage::Closed => Err("LLM polish sidecar exited during startup".to_string()),
        }
    }

    pub fn polish(
        &mut self,
        session_id: u64,
        input: &str,
        timeout_ms: u64,
    ) -> Result<LlmPolishOutcome, String> {
        self.send(&LlmPolishRequest::Polish {
            session_id,
            text: input,
        })?;
        let timeout = Duration::from_millis(timeout_ms);
        loop {
            match self.recv(timeout)? {
                LlmPolishMessage::Event(event) => match *event {
                    LlmPolishEvent::Polished {
                        session_id: response_session,
                        text,
                        latency_ms,
                        raw_output,
                        hard_unsafe,
                        review_flags,
                        polish_mode,
                        output_mode,
                        input_chars,
                        input_words,
                        finish_reason,
                        max_tokens,
                        raw_chars,
                        cleaned_chars,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cache_hit,
                        cache_prompt_tokens,
                        cache_matched_tokens,
                        cache_saved_tokens,
                        cache_entries,
                        cache_size_bytes,
                        decision_label,
                        decision_malformed,
                        rewrite_called,
                        validation_rejected,
                        decision,
                        rewrite,
                        llama_perf,
                    } if response_session == session_id => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            return Err("LLM polish returned empty output".to_string());
                        }
                        if !hard_unsafe.is_empty() {
                            return Err(format!(
                                "LLM polish hard validation flags: {}",
                                hard_unsafe.join(",")
                            ));
                        }
                        return Ok(LlmPolishOutcome {
                            text,
                            raw_output,
                            latency_ms,
                            review_flags,
                            diagnostics: LlmPolishDiagnostics {
                                polish_mode,
                                output_mode,
                                input_chars,
                                input_words,
                                finish_reason,
                                max_tokens,
                                raw_chars,
                                cleaned_chars,
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                                cache_hit,
                                cache_prompt_tokens,
                                cache_matched_tokens,
                                cache_saved_tokens,
                                cache_entries,
                                cache_size_bytes,
                                decision_label,
                                decision_malformed,
                                rewrite_called,
                                validation_rejected,
                                decision,
                                rewrite,
                                llama_perf,
                            },
                        });
                    }
                    LlmPolishEvent::Polished { .. } => {
                        // Stale response from an earlier timed-out request.
                    }
                    LlmPolishEvent::Error {
                        session_id: Some(response_session),
                        message,
                    } if response_session == session_id => {
                        return Err(format!("LLM polish sidecar error: {message}"));
                    }
                    LlmPolishEvent::Error { message, .. } => {
                        crate::logging::warn(&format!(
                            "ignored stale LLM polish sidecar error: {message}"
                        ));
                    }
                    LlmPolishEvent::Ready {
                        backend,
                        load_ms,
                        warmup_ms,
                    } => {
                        crate::logging::warn(&format!(
                            "duplicate LLM polish ready event: {backend}, load {load_ms}ms, warmup {warmup_ms}ms"
                        ));
                    }
                    LlmPolishEvent::Warmed { .. } => {
                        crate::logging::warn("ignored unexpected LLM polish warmup response");
                    }
                },
                LlmPolishMessage::Garbage(line) => {
                    crate::logging::warn(&format!(
                        "ignored non-protocol LLM polish sidecar output: {line}"
                    ));
                }
                LlmPolishMessage::Closed => {
                    return Err("LLM polish sidecar exited".to_string());
                }
            }
        }
    }

    pub fn warmup(
        &mut self,
        texts: &[&str],
        timeout_ms: u64,
    ) -> Result<LlmPolishWarmupOutcome, String> {
        let started = Instant::now();
        self.send(&LlmPolishRequest::Warmup { texts })?;
        let timeout = Duration::from_millis(timeout_ms);
        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(format!("LLM polish timed out after {timeout_ms}ms"));
            }
            let remaining = timeout - elapsed;
            match self.recv(remaining)? {
                LlmPolishMessage::Event(event) => match *event {
                    LlmPolishEvent::Warmed {
                        latency_ms,
                        requests,
                    } => {
                        return Ok(LlmPolishWarmupOutcome {
                            latency_ms,
                            requests: requests
                                .into_iter()
                                .map(|request| LlmPolishWarmupRequest {
                                    text: request.text,
                                    latency_ms: request.latency_ms,
                                    output_mode: request.output_mode,
                                    raw_chars: request.raw_chars,
                                    cleaned_chars: request.cleaned_chars,
                                    completion_tokens: request.completion_tokens,
                                    finish_reason: request.finish_reason,
                                    cache_hit: request.cache_hit,
                                    cache_matched_tokens: request.cache_matched_tokens,
                                    cache_entries: request.cache_entries,
                                    cache_size_bytes: request.cache_size_bytes,
                                })
                                .collect(),
                        });
                    }
                    LlmPolishEvent::Polished { .. } => {
                        crate::logging::warn("ignored stale LLM polish response during warmup");
                    }
                    LlmPolishEvent::Error { message, .. } => {
                        return Err(format!("LLM polish sidecar warmup error: {message}"));
                    }
                    LlmPolishEvent::Ready {
                        backend,
                        load_ms,
                        warmup_ms,
                    } => {
                        crate::logging::warn(&format!(
                            "duplicate LLM polish ready event during warmup: {backend}, load {load_ms}ms, warmup {warmup_ms}ms"
                        ));
                    }
                },
                LlmPolishMessage::Garbage(line) => {
                    crate::logging::warn(&format!(
                        "ignored non-protocol LLM polish sidecar output: {line}"
                    ));
                }
                LlmPolishMessage::Closed => {
                    return Err("LLM polish sidecar exited during warmup".to_string());
                }
            }
        }
    }

    fn send(&mut self, request: &LlmPolishRequest<'_>) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, request)
            .map_err(|error| format!("cannot encode LLM polish request: {error}"))?;
        self.stdin
            .write_all(b"\n")
            .and_then(|_| self.stdin.flush())
            .map_err(|error| format!("cannot write LLM polish request: {error}"))
    }

    fn recv(&self, timeout: Duration) -> Result<LlmPolishMessage, String> {
        self.rx.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => {
                format!("LLM polish timed out after {}ms", timeout.as_millis())
            }
            RecvTimeoutError::Disconnected => "LLM polish sidecar reader stopped".to_string(),
        })
    }
}

impl Drop for LlmPolishClient {
    fn drop(&mut self) {
        let _ = self.send(&LlmPolishRequest::Shutdown);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_warmed_response_with_per_request_diagnostics() {
        let event = serde_json::from_str::<LlmPolishEvent>(
            r#"{
                "type":"warmed",
                "latency_ms":1234,
                "requests":[
                    {
                        "text":"Hey, how are you doing?",
                        "latency_ms":234,
                        "raw_chars":23,
                        "cleaned_chars":23,
                        "completion_tokens":7,
                        "finish_reason":"stop",
                        "cache_hit":true,
                        "cache_matched_tokens":211,
                        "cache_entries":2,
                        "cache_size_bytes":1048576
                    }
                ]
            }"#,
        )
        .expect("warmed event parses");
        match event {
            LlmPolishEvent::Warmed {
                latency_ms,
                requests,
            } => {
                assert_eq!(latency_ms, 1234);
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].text, "Hey, how are you doing?");
                assert_eq!(requests[0].latency_ms, 234);
                assert_eq!(requests[0].completion_tokens, Some(7));
                assert_eq!(requests[0].finish_reason.as_deref(), Some("stop"));
                assert_eq!(requests[0].cache_hit, Some(true));
                assert_eq!(requests[0].cache_matched_tokens, Some(211));
                assert_eq!(requests[0].cache_entries, Some(2));
                assert_eq!(requests[0].cache_size_bytes, Some(1_048_576));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn parses_two_step_polished_response_with_nested_diagnostics() {
        let event = serde_json::from_str::<LlmPolishEvent>(
            r#"{
                "type":"polished",
                "session_id":7,
                "text":"Please send this to Priya tomorrow.",
                "raw_output":"EDITED: Please send this to Priya tomorrow.",
                "latency_ms":321,
                "output_mode":"two_step",
                "polish_mode":"two_step",
                "completion_tokens":11,
                "hard_unsafe":[],
                "review_flags":[],
                "decision_label":"EDIT",
                "decision_malformed":false,
                "rewrite_called":true,
                "validation_rejected":false,
                "decision":{
                    "decision":"EDIT",
                    "raw_output":"EDIT",
                    "latency_ms":101,
                    "output_mode":"decision",
                    "completion_tokens":1
                },
                "rewrite":{
                    "text":"Please send this to Priya tomorrow.",
                    "raw_output":"Please send this to Priya tomorrow.",
                    "latency_ms":220,
                    "output_mode":"rewrite",
                    "completion_tokens":10
                }
            }"#,
        )
        .expect("polished event parses");
        match event {
            LlmPolishEvent::Polished {
                output_mode,
                polish_mode,
                decision_label,
                rewrite_called,
                decision,
                rewrite,
                ..
            } => {
                assert_eq!(output_mode.as_deref(), Some("two_step"));
                assert_eq!(polish_mode.as_deref(), Some("two_step"));
                assert_eq!(decision_label.as_deref(), Some("EDIT"));
                assert_eq!(rewrite_called, Some(true));
                assert_eq!(decision.and_then(|call| call.completion_tokens), Some(1));
                assert_eq!(rewrite.and_then(|call| call.latency_ms), Some(220));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

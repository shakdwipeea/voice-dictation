//! The control socket: how compositor keybindings, the `sunoto-daemon`
//! CLI, and `setup` talk to a running daemon. One JSON object (or a bare
//! `press`/`release`) per connection; replies are written back on the same
//! stream by the event loop, never from this thread.

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sunoto_core::SessionMode;
use sunoto_desktop::HotkeyEvent;
use sunoto_polish::polish;

use crate::events::{ControlCommand, DaemonEvent, ModeHotkeyEvent};
use crate::llm_polish;
use crate::llm_report::llm_diagnostics_json;
use crate::logging;
use crate::settings::{self, Settings};

pub(crate) fn spawn_control_thread(
    events: Sender<DaemonEvent>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, Box<dyn Error>> {
    let path = settings::control_socket_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|error| format!("cannot remove stale {}: {error}", path.display()))?;
    }
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("cannot bind {}: {error}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot configure {}: {error}", path.display()))?;
    logging::info(&format!("control socket listening: {}", path.display()));
    Ok(std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    if !handle_control_stream(stream, &events) {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    let _ = events.send(DaemonEvent::Fatal(format!(
                        "control socket failed: {error}"
                    )));
                    break;
                }
            }
        }
        let _ = fs::remove_file(&path);
    }))
}

pub(crate) fn handle_control_stream(stream: UnixStream, events: &Sender<DaemonEvent>) -> bool {
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    if reader.read_line(&mut line).is_err() {
        return true;
    }
    let trimmed = line.trim();
    let event = match trimmed {
        "press" => Some(ModeHotkeyEvent {
            mode: SessionMode::Dictation,
            edge: HotkeyEvent::Pressed,
        }),
        "release" => Some(ModeHotkeyEvent {
            mode: SessionMode::Dictation,
            edge: HotkeyEvent::Released,
        }),
        other => match serde_json::from_str::<ControlCommand>(other) {
            Ok(ControlCommand::Polish { text }) => {
                return events
                    .send(DaemonEvent::ControlPolish {
                        text,
                        response: reader.into_inner(),
                    })
                    .is_ok();
            }
            Ok(ControlCommand::PlanSystem {
                text,
                dry_run: true,
            }) => {
                return events
                    .send(DaemonEvent::ControlSystemPlan {
                        text,
                        response: reader.into_inner(),
                    })
                    .is_ok();
            }
            Ok(ControlCommand::PlanSystem { dry_run: false, .. }) => {
                let mut stream = reader.into_inner();
                let _ = serde_json::to_writer(
                    &mut stream,
                    &serde_json::json!({
                        "type": "error",
                        "ok": false,
                        "error": "plan_system currently requires dry_run=true",
                    }),
                );
                let _ = stream.write_all(b"\n");
                return true;
            }
            Ok(ControlCommand::Trigger { mode, edge }) => Some(ModeHotkeyEvent {
                mode: mode.into(),
                edge: edge.into(),
            }),
            Ok(ControlCommand::Status) => {
                return events
                    .send(DaemonEvent::ControlStatus {
                        response: reader.into_inner(),
                    })
                    .is_ok();
            }
            Err(_) => {
                let mut stream = reader.into_inner();
                let _ = serde_json::to_writer(
                    &mut stream,
                    &serde_json::json!({
                        "type": "error",
                        "ok": false,
                        "error": "unknown control command",
                    }),
                );
                let _ = stream.write_all(b"\n");
                logging::warn(&format!("ignored unknown control command: {other:?}"));
                return true;
            }
        },
    };
    events
        .send(DaemonEvent::Hotkey(event.expect("event set above")))
        .is_ok()
}

pub(crate) fn control_polish_response(
    settings: &Settings,
    llm_polish: &mut Option<llm_polish::LlmPolishClient>,
    llm_post_asr_warmed: bool,
    text: &str,
) -> serde_json::Value {
    let started = Instant::now();
    let deterministic_started = Instant::now();
    let deterministic_output = if settings.polish_enabled {
        polish(text, &settings.polish).text
    } else {
        text.to_string()
    };
    let deterministic_latency_ms = deterministic_started.elapsed().as_millis();
    let mut output = deterministic_output.clone();
    let mut llm_report = serde_json::json!({
        "enabled": settings.llm_polish_enabled,
        "accepted": false,
    });

    if settings.llm_polish_enabled && !text.trim().is_empty() {
        if !llm_post_asr_warmed {
            llm_report = serde_json::json!({
                "enabled": true,
                "accepted": false,
                "error": "LLM polish is still warming",
            });
        } else if let Some(client) = llm_polish.as_mut() {
            let llm_input = output.clone();
            match client.polish_stream(
                0,
                &llm_input,
                settings.llm_polish_timeout_ms,
                &mut |_: &str| {},
            ) {
                Ok(outcome) => {
                    let llm_output = outcome.text.clone();
                    llm_report = serde_json::json!({
                        "enabled": true,
                        "accepted": true,
                        "input": llm_input,
                        "output": llm_output,
                        "raw_output": outcome.raw_output,
                        "latency_ms": outcome.latency_ms,
                        "diagnostics": llm_diagnostics_json(&outcome.diagnostics),
                    });
                    output = outcome.text;
                }
                Err(error) => {
                    llm_report = serde_json::json!({
                        "enabled": true,
                        "accepted": false,
                        "error": error,
                    });
                }
            }
        } else {
            llm_report = serde_json::json!({
                "enabled": true,
                "accepted": false,
                "error": "LLM polish sidecar is unavailable",
            });
        }
    }

    serde_json::json!({
        "type": "polish_result",
        "ok": true,
        "input": text,
        "deterministic_output": deterministic_output,
        "deterministic_latency_ms": deterministic_latency_ms,
        "output": output,
        "changed": output != text,
        "total_latency_ms": started.elapsed().as_millis(),
        "llm": llm_report,
    })
}

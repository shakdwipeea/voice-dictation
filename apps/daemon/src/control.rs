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

/// How long a client gets to send its one line. Connections are served one at
/// a time, and shutdown joins this thread, so a silent client must not hold it.
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(1);
/// The event loop writes replies on these streams and must never wait on a
/// client that stopped reading. A reply that fits the socket buffer never waits.
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

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

/// On macOS a connection inherits the listener's non-blocking flag, so reading
/// before the client has sent fails and the client gets dropped. Bounded
/// blocking waits for it instead, and keeps its stream safe for the event loop.
fn prepare_control_stream(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(CONTROL_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_WRITE_TIMEOUT))
}

pub(crate) fn handle_control_stream(stream: UnixStream, events: &Sender<DaemonEvent>) -> bool {
    if let Err(error) = prepare_control_stream(&stream) {
        logging::warn(&format!("cannot configure a control connection: {error}"));
        return true;
    }
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
            Ok(ControlCommand::UserControl { action, confirmed }) => {
                if !confirmed && matches!(action.as_str(), "pause" | "quit") { crate::insertion::suspend_insertion(); }
                if confirmed && matches!(action.as_str(), "pause" | "quit") {
                    crate::insertion::allow_insertion(false);
                }
                let queued = events.send(DaemonEvent::Overlay(sunoto_ipc::SidecarMessage::Event(sunoto_ipc::SidecarEvent::UserControl { action, confirmed }))).is_ok();
                let _ = reader.get_mut().write_all(if queued { b"{\"queued\":true}\n" } else { b"{\"queued\":false}\n" });
                return queued;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A server end as `accept` hands it over on macOS, where a connection
    /// inherits the non-blocking flag of its listener.
    fn accepted_pair() -> (UnixStream, UnixStream) {
        let (client, server) = UnixStream::pair().expect("socket pair");
        server.set_nonblocking(true).expect("non-blocking");
        (client, server)
    }

    #[test]
    fn a_client_that_sends_a_moment_after_connecting_is_still_heard() {
        let (mut client, server) = accepted_pair();
        let (tx, rx) = mpsc::channel();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            client.write_all(b"press\n").expect("send");
            client
        });
        assert!(handle_control_stream(server, &tx));
        let _client = sender.join().expect("sender");
        assert!(matches!(
            rx.try_recv(),
            Ok(DaemonEvent::Hotkey(ModeHotkeyEvent {
                edge: HotkeyEvent::Pressed,
                ..
            }))
        ));
    }

    #[test]
    fn a_client_that_never_sends_cannot_hold_the_thread() {
        let (_client, server) = accepted_pair();
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        assert!(handle_control_stream(server, &tx));
        assert!(started.elapsed() < CONTROL_READ_TIMEOUT * 2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_reply_stream_cannot_block_the_event_loop_forever() {
        let (mut client, server) = accepted_pair();
        client.write_all(b"{\"type\":\"status\"}\n").expect("send");
        let (tx, rx) = mpsc::channel();
        assert!(handle_control_stream(server, &tx));
        match rx.try_recv() {
            Ok(DaemonEvent::ControlStatus { response }) => {
                assert_eq!(
                    response.write_timeout().expect("timeout"),
                    Some(CONTROL_WRITE_TIMEOUT)
                );
            }
            _ => panic!("expected a status request"),
        }
    }
}

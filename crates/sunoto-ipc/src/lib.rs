use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SidecarRequest {
    Health,
    StartSession { session_id: u64, profile_ms: u16 },
    AudioChunk { session_id: u64, samples: Vec<i16> },
    FinishSession { session_id: u64 },
    CancelSession { session_id: u64 },
}

/// Commands for the GTK overlay UI sidecar (ui_sidecar.py). Same NDJSON
/// transport as the ASR protocol; the overlay answers only with `Ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayRequest {
    Show,
    Hide,
    Recording {
        elapsed_s: f64,
        peak: f64,
        rms: f64,
        segments: u32,
    },
    Status {
        text: String,
    },
    Segment {
        text: String,
    },
    Clear,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SidecarEvent {
    Ready {
        backend: String,
    },
    SessionStarted {
        session_id: u64,
    },
    Partial {
        session_id: u64,
        text: String,
    },
    Final {
        session_id: u64,
        text: String,
    },
    Error {
        session_id: Option<u64>,
        message: String,
    },
}

/// Everything the reader thread can deliver. The sidecar streams events on its
/// own schedule (partials arrive unsolicited), so consumers must treat this as
/// an event pump rather than pairing one reply to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarMessage {
    Event(SidecarEvent),
    /// A stdout line that was not valid protocol JSON. Reported, not fatal:
    /// one stray library print must never take the daemon down.
    Garbage { line: String },
    /// The sidecar closed stdout (crash or clean exit); a restart is needed.
    Closed,
}

#[derive(Debug)]
pub enum SidecarError {
    Io(std::io::Error),
    Json(serde_json::Error),
    MissingPipe(&'static str),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "sidecar I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "sidecar JSON failed: {error}"),
            Self::MissingPipe(name) => write!(formatter, "sidecar {name} pipe is unavailable"),
        }
    }
}

impl std::error::Error for SidecarError {}

impl From<std::io::Error> for SidecarError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SidecarError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub struct SidecarClient {
    child: Child,
    stdin: ChildStdin,
    reader: Option<JoinHandle<()>>,
}

impl SidecarClient {
    /// Spawn the sidecar process. A reader thread parses every stdout line and
    /// hands it to `sink`; `sink` returning `false` stops the thread (used when
    /// the consumer has gone away).
    pub fn spawn(
        program: &str,
        args: &[&str],
        sink: impl FnMut(SidecarMessage) -> bool + Send + 'static,
    ) -> Result<Self, SidecarError> {
        Self::spawn_with_env(program, args, &[], sink)
    }

    /// Like `spawn`, with extra environment variables (e.g. PYTHONPATH for
    /// sidecars that run as Python modules).
    pub fn spawn_with_env(
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
        sink: impl FnMut(SidecarMessage) -> bool + Send + 'static,
    ) -> Result<Self, SidecarError> {
        let mut child = Command::new(program)
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or(SidecarError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(SidecarError::MissingPipe("stdout"))?;
        let reader = std::thread::spawn(move || pump_events(BufReader::new(stdout), sink));
        Ok(Self {
            child,
            stdin,
            reader: Some(reader),
        })
    }

    pub fn send<Request: Serialize>(&mut self, request: &Request) -> Result<(), SidecarError> {
        serde_json::to_writer(&mut self.stdin, request)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

fn pump_events(
    mut stdout: BufReader<impl std::io::Read>,
    mut sink: impl FnMut(SidecarMessage) -> bool,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = sink(SidecarMessage::Closed);
                return;
            }
            Ok(_) => {
                let message = match serde_json::from_str::<SidecarEvent>(&line) {
                    Ok(event) => SidecarMessage::Event(event),
                    Err(_) => SidecarMessage::Garbage {
                        line: line.trim_end().to_string(),
                    },
                };
                if !sink(message) {
                    return;
                }
            }
        }
    }
}

impl Drop for SidecarClient {
    fn drop(&mut self) {
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
    use std::sync::mpsc;
    use std::time::Duration;

    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn spawn_mock(args: &[&str]) -> (SidecarClient, mpsc::Receiver<SidecarMessage>) {
        let sidecar = repo_root().join("services/asr/mock_sidecar.py");
        let mut full_args = vec![sidecar.to_str().unwrap().to_string()];
        full_args.extend(args.iter().map(|arg| arg.to_string()));
        let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
        let (tx, rx) = mpsc::channel();
        let client = SidecarClient::spawn("python3", &arg_refs, move |message| {
            tx.send(message).is_ok()
        })
        .unwrap();
        (client, rx)
    }

    fn expect_event(rx: &mpsc::Receiver<SidecarMessage>) -> SidecarEvent {
        match rx.recv_timeout(RECV_TIMEOUT).expect("sidecar message") {
            SidecarMessage::Event(event) => event,
            other => panic!("expected protocol event, received {other:?}"),
        }
    }

    #[test]
    fn protocol_round_trip_is_typed_json() {
        let request = SidecarRequest::StartSession {
            session_id: 7,
            profile_ms: 160,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"start_session","session_id":7,"profile_ms":160}"#
        );
        assert_eq!(
            serde_json::from_str::<SidecarRequest>(&encoded).unwrap(),
            request
        );
    }

    #[test]
    fn overlay_protocol_round_trip_is_typed_json() {
        let request = OverlayRequest::Recording {
            elapsed_s: 1.5,
            peak: 0.4,
            rms: 0.05,
            segments: 1,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"recording","elapsed_s":1.5,"peak":0.4,"rms":0.05,"segments":1}"#
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::Status {
                text: "transcribing".into()
            })
            .unwrap(),
            r#"{"type":"status","text":"transcribing"}"#
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::Show).unwrap(),
            r#"{"type":"show"}"#
        );
    }

    #[test]
    fn managed_mock_sidecar_completes_session() {
        let (mut client, rx) = spawn_mock(&["--final-text", "hello"]);
        client.send(&SidecarRequest::Health).unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Ready {
                backend: "mock".into()
            }
        );
        client
            .send(&SidecarRequest::StartSession {
                session_id: 1,
                profile_ms: 160,
            })
            .unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::SessionStarted { session_id: 1 }
        );
        client
            .send(&SidecarRequest::AudioChunk {
                session_id: 1,
                samples: vec![0; 320],
            })
            .unwrap();
        client
            .send(&SidecarRequest::FinishSession { session_id: 1 })
            .unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Final {
                session_id: 1,
                text: "hello".into()
            }
        );
    }

    #[test]
    fn mock_sidecar_streams_partials_between_requests() {
        let (mut client, rx) = spawn_mock(&[
            "--final-text",
            "hello world",
            "--partial-every-chunks",
            "1",
        ]);
        client.send(&SidecarRequest::Health).unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Ready {
                backend: "mock".into()
            }
        );
        client
            .send(&SidecarRequest::StartSession {
                session_id: 3,
                profile_ms: 160,
            })
            .unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::SessionStarted { session_id: 3 }
        );
        for _ in 0..2 {
            client
                .send(&SidecarRequest::AudioChunk {
                    session_id: 3,
                    samples: vec![0; 320],
                })
                .unwrap();
        }
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Partial {
                session_id: 3,
                text: "hello".into()
            }
        );
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Partial {
                session_id: 3,
                text: "hello world".into()
            }
        );
        client
            .send(&SidecarRequest::FinishSession { session_id: 3 })
            .unwrap();
        assert_eq!(
            expect_event(&rx),
            SidecarEvent::Final {
                session_id: 3,
                text: "hello world".into()
            }
        );
    }

    #[test]
    fn sidecar_exit_is_reported_as_closed() {
        let (client, rx) = spawn_mock(&[]);
        drop(client);
        let mut saw_closed = false;
        while let Ok(message) = rx.recv_timeout(RECV_TIMEOUT) {
            if message == SidecarMessage::Closed {
                saw_closed = true;
                break;
            }
        }
        assert!(saw_closed, "reader thread must report Closed on sidecar exit");
    }

    #[test]
    fn garbage_lines_are_reported_not_fatal() {
        let (tx, rx) = mpsc::channel();
        let mut client = SidecarClient::spawn(
            "sh",
            &[
                "-c",
                r#"echo 'not json'; echo '{"type":"ready","backend":"mock"}'"#,
            ],
            move |message| tx.send(message).is_ok(),
        )
        .unwrap();
        assert_eq!(
            rx.recv_timeout(RECV_TIMEOUT).unwrap(),
            SidecarMessage::Garbage {
                line: "not json".into()
            }
        );
        assert_eq!(
            rx.recv_timeout(RECV_TIMEOUT).unwrap(),
            SidecarMessage::Event(SidecarEvent::Ready {
                backend: "mock".into()
            })
        );
        assert_eq!(rx.recv_timeout(RECV_TIMEOUT).unwrap(), SidecarMessage::Closed);
        let _ = client.send(&SidecarRequest::Health);
    }
}

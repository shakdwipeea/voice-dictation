use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SidecarRequest {
    Health,
    StartSession {
        session_id: u64,
        profile_ms: u16,
    },
    /// Words to favour in this dictation, strongest source first (see the
    /// daemon's `vocabulary` module). Sent once, soon after StartSession;
    /// backends without word boosting ignore it.
    SessionWords {
        session_id: u64,
        screen: Vec<String>,
        project: Vec<String>,
        everywhere: Vec<String>,
    },
    AudioChunk {
        session_id: u64,
        samples: Vec<i16>,
    },
    FinishSession {
        session_id: u64,
    },
    CancelSession {
        session_id: u64,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Commands for the GTK overlay UI sidecar (ui_sidecar.py). Same NDJSON
/// transport as the ASR protocol; the overlay answers only with `Ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayRequest {
    Show,
    Hide,
    /// The dictation was pasted at the cursor. Sent just before `Hide`; the
    /// macOS pill shows a check mark as it closes.
    Pasted,
    Recording {
        elapsed_s: f64,
        peak: f64,
        rms: f64,
        segments: u32,
    },
    Status {
        text: String,
        /// A failure the user should notice (timeout, focus moved, password
        /// field). The macOS pill draws it with a warning mark instead of as
        /// a live transcript. Omitted from the wire when false.
        #[serde(default, skip_serializing_if = "is_false")]
        error: bool,
    },
    /// Daemon health for the idle pill: `loading_asr`, `warming_polish`,
    /// `hotkey_blocked`, `mic_unavailable`, or `ready`. Anything but `ready`
    /// shows the pill with a neutral dot and `detail` as the caption; `ready`
    /// hides it. Sent on every transition and whenever the overlay comes up.
    State {
        name: String,
        detail: String,
    },
    Segment {
        text: String,
    },
    Clear,
    /// Focusable command palette shown only after System recording ends.
    SystemPalette {
        session_id: u64,
        transcript: String,
        suggestions: Vec<OverlaySuggestion>,
    },
    DismissSystemPalette {
        session_id: u64,
    },
    /// First-run permission guide (macOS overlay only). The daemon sends the
    /// live TCC state edge-triggered. The overlay enables one row at a time
    /// and remains visible until all flags are true and the user clicks Done.
    Onboarding {
        accessibility: bool,
        microphone: bool,
        /// The dictation shortcut as people read it ("Right Option"), so the
        /// panel can teach it. Omitted when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shortcut: Option<String>,
        /// The speech model a first start is downloading. Omitted when the
        /// model was already on disk; kept, complete, once it arrives.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_download: Option<ModelDownload>,
    },
    /// What the menu bar item shows (macOS overlay only). Sent when the
    /// overlay comes up and whenever any of it changes; the menu is built
    /// from the latest one when it opens.
    Menu {
        /// "Sunoto is ready", or what it is waiting for.
        status: String,
        /// The status is ready: a tap dictates.
        ready: bool,
        /// How to dictate: "Tap Right Option to dictate".
        hint: String,
        version: String,
        /// Updates install by themselves (`auto_update` is "install").
        auto_update: bool,
        /// Why this copy cannot update itself, whatever the switch says (a
        /// build from source). The switch is greyed out while set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_blocked: Option<String>,
        /// A line under Check for Updates: a downloaded update waiting for
        /// an idle moment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_note: Option<String>,
        /// Check for Updates is running.
        #[serde(default, skip_serializing_if = "is_false")]
        checking: bool,
        history_enabled: bool,
        /// The newest dictations kept in history, newest first.
        recent: Vec<RecentDictation>,
    },
    /// A message box from the menu bar item, such as the answer to Check
    /// for Updates. With `offer_install`, its buttons are Install Now and
    /// Later; Install Now answers `install_update`.
    Alert {
        title: String,
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        offer_install: bool,
    },
    /// The "This was wrong" panel for one dictation (macOS overlay only):
    /// what a report would hold, in plain lines, and the dictation's text,
    /// which goes in only if the user ticks Include the text.
    ReportPanel {
        /// The history record's id; Send answers `send_report` with it.
        id: String,
        text: String,
        included: Vec<String>,
        /// The dictation's audio was kept, so Include the audio can be ticked.
        #[serde(default, skip_serializing_if = "is_false")]
        audio_kept: bool,
        /// This build can upload reports. Without it the button is Save and
        /// the report stays on this Mac.
        #[serde(default, skip_serializing_if = "is_false")]
        can_send: bool,
    },
    /// The answer to `send_report`: the report's code once it is saved, or
    /// why it could not be.
    ReportSaved {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The report is queued for upload, not only saved on this Mac.
        #[serde(default, skip_serializing_if = "is_false")]
        sending: bool,
    },
    Shutdown,
}

/// How far the speech model download of a first start is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDownload {
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// The last try failed (no connection, say); the app tries again by
    /// itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retrying: bool,
}

/// One dictation in the menu bar's Recent Dictations: the text that was
/// pasted, and when (milliseconds since the Unix epoch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentDictation {
    /// The history record's id, for This was wrong. Empty in records the
    /// overlay cannot report.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub text: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlaySuggestion {
    pub suggestion_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub action_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Accessibility,
    Microphone,
}

/// One dictation's decode, for the accuracy record: counts only, never text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrDiag {
    /// Time spent decoding on the GPU, all pieces together.
    #[serde(default)]
    pub decode_ms: u64,
    #[serde(default)]
    pub pieces: u32,
    /// Decodes run again because they came back with `<unk>`.
    #[serde(default)]
    pub unk_retries: u32,
    /// The last piece came from the decode made in the pause before the tap.
    #[serde(default)]
    pub pause_reused: bool,
    /// Tokens where the word-list bonus changed the decoder's pick.
    #[serde(default)]
    pub boosted_tokens: u32,
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
        /// How the decode went, in counts (see `AsrDiag`). Engines that do
        /// not report it leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diag: Option<AsrDiag>,
    },
    Error {
        session_id: Option<u64>,
        message: String,
    },
    SystemSelection {
        session_id: u64,
        suggestion_id: String,
    },
    SystemCancelled {
        session_id: u64,
    },
    /// The user clicked one permission row in the macOS onboarding panel.
    PermissionAction {
        permission: PermissionKind,
    },
    /// The user confirmed every onboarding row is granted.
    OnboardingDone,
    /// Menu bar: Check for Updates.
    CheckUpdates,
    /// Menu bar: Install Now, in the alert offering a downloaded update.
    InstallUpdate,
    /// Menu bar: the Install Updates Automatically switch.
    SetAutoUpdate {
        on: bool,
    },
    /// Menu bar: Clear History, after the user confirmed it.
    ClearHistory,
    /// Menu bar: This was wrong, on one recent dictation.
    FlagDictation {
        id: String,
    },
    /// The report panel's Send.
    SendReport {
        id: String,
        #[serde(default)]
        include_text: bool,
        #[serde(default)]
        include_audio: bool,
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
    Garbage {
        line: String,
    },
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
    fn final_reads_with_and_without_diag() {
        let plain: SidecarEvent =
            serde_json::from_str(r#"{"type":"final","session_id":2,"text":"hi"}"#).unwrap();
        assert_eq!(
            plain,
            SidecarEvent::Final {
                session_id: 2,
                text: "hi".into(),
                diag: None
            }
        );
        let with: SidecarEvent = serde_json::from_str(
            r#"{"type":"final","session_id":2,"text":"hi","diag":{"decode_ms":118,"pieces":2,"unk_retries":1,"pause_reused":true,"boosted_tokens":3}}"#,
        )
        .unwrap();
        assert_eq!(
            with,
            SidecarEvent::Final {
                session_id: 2,
                text: "hi".into(),
                diag: Some(AsrDiag {
                    decode_ms: 118,
                    pieces: 2,
                    unk_retries: 1,
                    pause_reused: true,
                    boosted_tokens: 3,
                })
            }
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
                text: "transcribing".into(),
                error: false,
            })
            .unwrap(),
            r#"{"type":"status","text":"transcribing"}"#
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::Pasted).unwrap(),
            r#"{"type":"pasted"}"#
        );
        let failure = OverlayRequest::Status {
            text: "ASR timed out".into(),
            error: true,
        };
        let encoded = serde_json::to_string(&failure).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"status","text":"ASR timed out","error":true}"#
        );
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(&encoded).unwrap(),
            failure
        );
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(r#"{"type":"status","text":"x"}"#).unwrap(),
            OverlayRequest::Status {
                text: "x".into(),
                error: false,
            }
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::Show).unwrap(),
            r#"{"type":"show"}"#
        );

        let palette = OverlayRequest::SystemPalette {
            session_id: 9,
            transcript: "Open Chrome".into(),
            suggestions: vec![OverlaySuggestion {
                suggestion_id: "session-9:suggestion-1".into(),
                title: "Open Google Chrome".into(),
                subtitle: Some("Application".into()),
                action_label: "Open".into(),
            }],
        };
        let encoded = serde_json::to_string(&palette).unwrap();
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(&encoded).unwrap(),
            palette
        );
        assert!(!encoded.contains("application_id"));
        assert!(!encoded.contains("path"));

        let onboarding = OverlayRequest::Onboarding {
            accessibility: false,
            microphone: true,
            shortcut: None,
            model_download: None,
        };
        let encoded = serde_json::to_string(&onboarding).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"onboarding","accessibility":false,"microphone":true}"#
        );
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(&encoded).unwrap(),
            onboarding
        );
        let downloading = OverlayRequest::Onboarding {
            accessibility: true,
            microphone: false,
            shortcut: None,
            model_download: Some(ModelDownload {
                done_bytes: 5,
                total_bytes: 10,
                retrying: false,
            }),
        };
        let encoded = serde_json::to_string(&downloading).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"onboarding","accessibility":true,"microphone":false,"model_download":{"done_bytes":5,"total_bytes":10}}"#
        );
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(&encoded).unwrap(),
            downloading
        );
        let action = SidecarEvent::PermissionAction {
            permission: PermissionKind::Accessibility,
        };
        let encoded = serde_json::to_string(&action).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"permission_action","permission":"accessibility"}"#
        );
        assert_eq!(
            serde_json::from_str::<SidecarEvent>(&encoded).unwrap(),
            action
        );
        assert_eq!(
            serde_json::to_string(&SidecarEvent::OnboardingDone).unwrap(),
            r#"{"type":"onboarding_done"}"#
        );
    }

    #[test]
    fn menu_bar_protocol_is_typed_json() {
        let menu = OverlayRequest::Menu {
            status: "Sunoto is ready".into(),
            ready: true,
            hint: "Tap Right Option to dictate".into(),
            version: "0.1.12".into(),
            auto_update: true,
            update_blocked: None,
            update_note: None,
            checking: false,
            history_enabled: true,
            recent: vec![RecentDictation {
                id: "1a1-3".into(),
                text: "hello".into(),
                at_ms: 1_760_000_000_000,
            }],
        };
        let encoded = serde_json::to_string(&menu).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"menu","status":"Sunoto is ready","ready":true,"hint":"Tap Right Option to dictate","version":"0.1.12","auto_update":true,"history_enabled":true,"recent":[{"id":"1a1-3","text":"hello","at_ms":1760000000000}]}"#
        );
        assert_eq!(
            serde_json::from_str::<OverlayRequest>(&encoded).unwrap(),
            menu
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::Alert {
                title: "Sunoto 0.1.13 is ready to install".into(),
                text: "Sunoto restarts in a few seconds.".into(),
                offer_install: true,
            })
            .unwrap(),
            r#"{"type":"alert","title":"Sunoto 0.1.13 is ready to install","text":"Sunoto restarts in a few seconds.","offer_install":true}"#
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::ReportPanel {
                id: "1a1-3".into(),
                text: "hello".into(),
                included: vec!["Sunoto 0.1.15".into()],
                audio_kept: false,
                can_send: true,
            })
            .unwrap(),
            r#"{"type":"report_panel","id":"1a1-3","text":"hello","included":["Sunoto 0.1.15"],"can_send":true}"#
        );
        assert_eq!(
            serde_json::to_string(&OverlayRequest::ReportSaved {
                id: "1a1-3".into(),
                code: Some("SUN-7F3A".into()),
                error: None,
                sending: true,
            })
            .unwrap(),
            r#"{"type":"report_saved","id":"1a1-3","code":"SUN-7F3A","sending":true}"#
        );
        for (line, event) in [
            (r#"{"type":"check_updates"}"#, SidecarEvent::CheckUpdates),
            (r#"{"type":"install_update"}"#, SidecarEvent::InstallUpdate),
            (
                r#"{"type":"set_auto_update","on":false}"#,
                SidecarEvent::SetAutoUpdate { on: false },
            ),
            (r#"{"type":"clear_history"}"#, SidecarEvent::ClearHistory),
            (
                r#"{"type":"flag_dictation","id":"1a1-3"}"#,
                SidecarEvent::FlagDictation { id: "1a1-3".into() },
            ),
            (
                r#"{"type":"send_report","id":"1a1-3","include_text":true}"#,
                SidecarEvent::SendReport {
                    id: "1a1-3".into(),
                    include_text: true,
                    include_audio: false,
                },
            ),
        ] {
            assert_eq!(serde_json::from_str::<SidecarEvent>(line).unwrap(), event);
        }
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
                text: "hello".into(),
                diag: None,
            }
        );
    }

    #[test]
    fn mock_sidecar_streams_partials_between_requests() {
        let (mut client, rx) =
            spawn_mock(&["--final-text", "hello world", "--partial-every-chunks", "1"]);
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
                text: "hello world".into(),
                diag: None,
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
        assert!(
            saw_closed,
            "reader thread must report Closed on sidecar exit"
        );
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
        assert_eq!(
            rx.recv_timeout(RECV_TIMEOUT).unwrap(),
            SidecarMessage::Closed
        );
        let _ = client.send(&SidecarRequest::Health);
    }
}

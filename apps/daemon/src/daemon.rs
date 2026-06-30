use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::raw::c_int;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Deserialize;
use sunoto_audio::{AudioEvent, CaptureConfig, start_capture};
use sunoto_core::{AudioPreRoll, SessionAction, SessionMachine, SessionState};
use sunoto_desktop::{
    BubbleKind, HotkeyEvent, HotkeyListener, InsertionOutcome, Shortcut, UiAdapter, X11Error,
};
use sunoto_ipc::{OverlayRequest, SidecarClient, SidecarEvent, SidecarMessage, SidecarRequest};
use sunoto_polish::{polish, resolve_style};

use crate::llm_polish;
use crate::logging;
use crate::settings::{self, Settings, sanitize_for_insertion};

const SAMPLES_PER_MS: usize = 16;
const TICK: Duration = Duration::from_millis(50);
const ERROR_BUBBLE_VISIBLE: Duration = Duration::from_millis(2500);
const SIDECAR_BACKOFF_START: Duration = Duration::from_millis(500);
const SIDECAR_BACKOFF_CAP: Duration = Duration::from_secs(5);

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
}

unsafe extern "C" fn on_termination_signal(_signum: c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        signal(SIGINT, on_termination_signal as *const () as usize);
        signal(SIGTERM, on_termination_signal as *const () as usize);
    }
}

pub enum DaemonEvent {
    Hotkey(HotkeyEvent),
    Audio(AudioEvent),
    Sidecar(SidecarMessage),
    /// Messages from the GTK overlay UI sidecar (ready handshake, exit).
    Overlay(SidecarMessage),
    Ui(UiReport),
    /// WM_CLASS (instance, class) of the window focused at shortcut release;
    /// reported by the UI thread right after CaptureFocus.
    FocusClass(Option<(String, String)>),
    ControlPolish {
        text: String,
        response: UnixStream,
    },
    Fatal(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlCommand {
    Polish { text: String },
}

pub struct UiReport {
    pub session_id: u64,
    pub result: Result<InsertionOutcome, String>,
    pub insert_duration: Duration,
}

/// Per-session streaming-insertion context owned by the UI thread.
struct StreamSession {
    session_id: u64,
    /// Focus token captured at release (consumed on the first chunk).
    focus_token: Option<String>,
    /// Whether the focused window still matched at stream start. When false
    /// the UI thread never types and instead pastes the final text once at
    /// end (or clipboard-parks it if focus moved).
    focus_ok: bool,
    /// False after a mid-stream typing error forces clipboard fallback.
    typed_ok: bool,
    /// Every delta typed so far (kept for the clipboard-fallback commit and
    /// for diagnostics).
    accumulated: String,
}

pub enum UiCommand {
    CaptureFocus,
    ShowBubble(BubbleKind, String),
    HideBubble,
    Insert { session_id: u64, text: String },
    /// Progressive LLM-polish streaming insertion.
    /// `first` is true on the first chunk of a session (the UI thread uses it
    /// to consume the captured focus token and choose typing vs clipboard
    /// fallback). Deltas are typed (CGEvent keystrokes) as they arrive.
    InsertStreamChunk {
        session_id: u64,
        first: bool,
        delta: String,
    },
    /// Finalize a streaming insertion session. `streamed_ok` is false when the
    /// sidecar reverted (content-loss guard) or errored mid-stream; in that
    /// case the UI thread pastes `final_text` atomically via the reliable
    /// clipboard path. With `streamed_ok` and successful typing the typed
    /// text is kept as-is (no re-paste).
    InsertStreamEnd {
        session_id: u64,
        final_text: String,
        streamed_ok: bool,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesktopBackend {
    X11,
    Wayland,
    Macos,
}

fn desktop_backend(settings: &Settings) -> DesktopBackend {
    if cfg!(target_os = "macos") {
        return DesktopBackend::Macos;
    }
    if settings.overlay_backend == "wayland"
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
        || std::env::var("WAYLAND_DISPLAY").is_ok()
    {
        DesktopBackend::Wayland
    } else {
        DesktopBackend::X11
    }
}

struct FocusSnapshot {
    token: Option<String>,
    class: Option<(String, String)>,
}

enum UiBackend {
    X11(UiAdapter),
    Wayland(WaylandUiAdapter),
    // Phase 2-4: replace with a real `sunoto-macos` adapter (CGEventTap,
    // CoreAudio, CGEvent insertion, NSPasteboard, NSWorkspace focus).
    Macos(UiAdapter),
}

impl UiBackend {
    fn open(backend: DesktopBackend) -> Result<Self, String> {
        match backend {
            DesktopBackend::X11 => UiAdapter::open()
                .map(Self::X11)
                .map_err(|error| format!("X11 UI unavailable: {error}")),
            DesktopBackend::Wayland => WaylandUiAdapter::open().map(Self::Wayland),
            DesktopBackend::Macos => UiAdapter::open()
                .map(Self::Macos)
                .map_err(|error| format!("macOS UI unavailable: {error}")),
        }
    }

    fn capture_focus(&mut self) -> FocusSnapshot {
        match self {
            Self::X11(adapter) => {
                let focus = adapter.focused_window();
                FocusSnapshot {
                    token: Some(focus.to_string()),
                    class: adapter.window_class(focus),
                }
            }
            Self::Wayland(adapter) => adapter.capture_focus(),
            Self::Macos(adapter) => {
                let focus = adapter.focused_window();
                FocusSnapshot {
                    token: Some(focus.to_string()),
                    class: adapter.window_class(focus),
                }
            }
        }
    }

    fn show_bubble(&mut self, kind: BubbleKind, text: &str) {
        match self {
            Self::X11(adapter) => adapter.bubble_show(kind, text),
            Self::Macos(adapter) => adapter.bubble_show(kind, text),
            Self::Wayland(_) => {
                let _ = (kind, text);
            }
        }
    }

    fn hide_bubble(&mut self) {
        match self {
            Self::X11(adapter) => adapter.bubble_hide(),
            Self::Macos(adapter) => adapter.bubble_hide(),
            Self::Wayland(_) => {}
        }
    }

    fn insert(
        &mut self,
        focus_at_release: Option<String>,
        text: &str,
    ) -> Result<InsertionOutcome, String> {
        match self {
            Self::X11(adapter) => insert_x11(adapter, focus_at_release, text),
            // macOS: CGEvent per-char unicode typing is unreliable across
            // Cocoa apps (many ignore the unicode string on a synthetic
            // event), so paste via the clipboard first and fall back to
            // direct typing — the same ordering the Wayland path uses.
            Self::Macos(adapter) => insert_macos(adapter, focus_at_release, text),
            Self::Wayland(adapter) => adapter.insert(focus_at_release, text),
        }
    }

    /// True when the currently focused window still matches the token captured
    /// at press/release time. Used by streaming insertion to decide whether to
    /// type progressively or fall back to the reliable clipboard commit at end.
    fn focus_matches(&self, expected: Option<&str>) -> bool {
        match expected {
            None => true,
            Some(expected) => match self {
                Self::X11(adapter) | Self::Macos(adapter) => {
                    adapter.focused_window().to_string() == expected
                }
                Self::Wayland(adapter) => adapter.focus_matches(expected),
            },
        }
    }

    /// Type `text` directly into the focused window (CGEvent keystrokes on
    /// macOS, XTEST on X11, wtype on Wayland). Used by streaming insertion to
    /// surface decoded tokens progressively. This is the existing typing path
    /// (the fallback for the canonical clipboard paste); some apps may drop
    /// characters, which the caller handles by switching to clipboard fallback.
    fn type_chunk(&mut self, text: &str) -> Result<(), String> {
        match self {
            Self::X11(adapter) | Self::Macos(adapter) => {
                adapter.insert_direct(text).map_err(|error| error.to_string())
            }
            Self::Wayland(adapter) => adapter.type_direct(text),
        }
    }

    fn pump(&mut self) {
        match self {
            Self::X11(adapter) | Self::Macos(adapter) => adapter.pump(),
            Self::Wayland(_) => {}
        }
    }
}

struct WaylandUiAdapter;

impl WaylandUiAdapter {
    fn open() -> Result<Self, String> {
        require_program("hyprctl")?;
        require_program("wtype")?;
        require_program("wl-copy")?;
        Ok(Self)
    }

    fn capture_focus(&self) -> FocusSnapshot {
        match active_hyprland_window() {
            Some(window) => FocusSnapshot {
                token: Some(window.address),
                class: window.class.map(|class| (class.clone(), class)),
            },
            None => FocusSnapshot {
                token: None,
                class: None,
            },
        }
    }

    fn insert(
        &self,
        focus_at_release: Option<String>,
        text: &str,
    ) -> Result<InsertionOutcome, String> {
        let focus_now = active_hyprland_window();
        if let Some(expected) = focus_at_release
            && focus_now.as_ref().map(|window| window.address.as_str()) != Some(expected.as_str())
        {
            return self
                .set_clipboard(text)
                .map(|_| InsertionOutcome::ClipboardOnly);
        }

        self.set_clipboard(text)?;
        let terminal = focus_now
            .as_ref()
            .and_then(|window| window.class.as_deref())
            .is_some_and(is_terminal_class);
        match self.paste_clipboard(terminal) {
            Ok(()) => Ok(InsertionOutcome::Pasted),
            Err(paste_error) => match self.type_direct(text) {
                Ok(()) => Ok(InsertionOutcome::Typed),
                Err(type_error) => {
                    logging::warn(&format!(
                        "Wayland paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                    ));
                    Ok(InsertionOutcome::ClipboardOnly)
                }
            },
        }
    }

    fn paste_clipboard(&self, terminal: bool) -> Result<(), String> {
        let mut command = Command::new("wtype");
        if terminal {
            command.args([
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
            ]);
        } else {
            command.args(["-M", "ctrl", "-k", "v", "-m", "ctrl"]);
        }
        run_command(command, "wtype paste")
    }

    fn type_direct(&self, text: &str) -> Result<(), String> {
        let mut command = Command::new("wtype");
        command.arg("--").arg(text);
        run_command(command, "wtype direct")
    }

    fn focus_matches(&self, expected: &str) -> bool {
        active_hyprland_window()
            .as_ref()
            .map(|window| window.address.as_str())
            == Some(expected)
    }

    fn set_clipboard(&self, text: &str) -> Result<(), String> {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run wl-copy: {error}"))?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err("wl-copy stdin unavailable".to_string());
        };
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("cannot write wl-copy input: {error}"))?;
        drop(stdin);
        let status = child
            .wait()
            .map_err(|error| format!("cannot wait for wl-copy: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("wl-copy exited with {status}"))
    }
}

fn is_terminal_class(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    [
        "terminal",
        "ghostty",
        "kitty",
        "alacritty",
        "foot",
        "wezterm",
        "xterm",
        "urxvt",
        "konsole",
        "tilix",
        "terminator",
    ]
    .iter()
    .any(|needle| class.contains(needle))
}

fn run_command(mut command: Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot run {label}: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{label} exited with {status}"))
}

struct HyprlandWindow {
    address: String,
    class: Option<String>,
}

fn active_hyprland_window() -> Option<HyprlandWindow> {
    let output = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let address = payload.get("address")?.as_str()?.to_string();
    if address.is_empty() || address == "0x0" {
        return None;
    }
    let class = payload
        .get("class")
        .and_then(serde_json::Value::as_str)
        .filter(|class| !class.is_empty())
        .map(str::to_string);
    Some(HyprlandWindow { address, class })
}

fn require_program(program: &str) -> Result<(), String> {
    match Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{program} unavailable: {error}")),
    }
}

/// UI thread: owns insertion, clipboard, and fallback status-bubble operations.
fn ui_thread(commands: Receiver<UiCommand>, events: Sender<DaemonEvent>, backend: DesktopBackend) {
    let mut adapter = match UiBackend::open(backend) {
        Ok(adapter) => adapter,
        Err(error) => {
            let _ = events.send(DaemonEvent::Fatal(error));
            return;
        }
    };
    let mut focus_at_release: Option<String> = None;
    // Streaming-insertion per-session context. `first` chunk consumes
    // focus_at_release and chooses Typing vs ClipboardFallback; the End
    // commit finalizes.
    let mut stream: Option<StreamSession> = None;
    loop {
        match commands.recv_timeout(Duration::from_millis(25)) {
            Ok(UiCommand::CaptureFocus) => {
                let focus = adapter.capture_focus();
                focus_at_release = focus.token;
                if events.send(DaemonEvent::FocusClass(focus.class)).is_err() {
                    return;
                }
            }
            Ok(UiCommand::ShowBubble(kind, text)) => adapter.show_bubble(kind, &text),
            Ok(UiCommand::HideBubble) => adapter.hide_bubble(),
            Ok(UiCommand::Insert { session_id, text }) => {
                let started = Instant::now();
                let result = adapter.insert(focus_at_release.take(), &text);
                let report = UiReport {
                    session_id,
                    result,
                    insert_duration: started.elapsed(),
                };
                if events.send(DaemonEvent::Ui(report)).is_err() {
                    return;
                }
            }
            Ok(UiCommand::InsertStreamChunk {
                session_id,
                first,
                delta,
            }) => {
                if first || stream.as_ref().map(|s| s.session_id) != Some(session_id) {
                    // Drop any stale streaming session from a different id.
                    stream = Some(StreamSession {
                        session_id,
                        focus_token: focus_at_release.take(),
                        focus_ok: false,
                        typed_ok: false,
                        accumulated: String::new(),
                    });
                    let session = stream.as_mut().expect("stream just initialized");
                    session.focus_ok = adapter.focus_matches(session.focus_token.as_deref());
                    session.typed_ok = session.focus_ok;
                }
                let Some(session) = stream.as_mut() else {
                    continue;
                };
                session.accumulated.push_str(&delta);
                if session.focus_ok && session.typed_ok
                    && let Err(error) = adapter.type_chunk(&delta)
                {
                    // Typing failed mid-stream: switch to clipboard
                    // fallback (commit the full accumulated text at End).
                    logging::warn(&format!(
                        "streaming insert typing failed for session {session_id}: {error}; falling back to clipboard commit"
                    ));
                    session.typed_ok = false;
                }
            }
            Ok(UiCommand::InsertStreamEnd {
                session_id,
                final_text,
                streamed_ok,
            }) => {
                let started = Instant::now();
                let session = stream.take().unwrap_or_else(|| StreamSession {
                    session_id,
                    focus_token: None,
                    focus_ok: false,
                    typed_ok: false,
                    accumulated: final_text.clone(),
                });
                // Decide the final outcome.
                let keep_typed = streamed_ok
                    && session.focus_ok
                    && session.typed_ok
                    && !session.accumulated.is_empty();
                let (result, insert_duration) = if keep_typed {
                    // Text already on screen via progressive typing.
                    (Ok(InsertionOutcome::Typed), started.elapsed())
                } else {
                    // Either focus moved, typing failed, or the sidecar
                    // reverted — paste the authoritative final text once via
                    // the reliable clipboard path (which itself falls back to
                    // clipboard-park if focus changed).
                    let outcome = adapter.insert(session.focus_token.clone(), &final_text);
                    (outcome, started.elapsed())
                };
                let report = UiReport {
                    session_id,
                    result,
                    insert_duration,
                };
                if events.send(DaemonEvent::Ui(report)).is_err() {
                    return;
                }
            }
            Ok(UiCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        adapter.pump();
    }
}

fn insert_x11(
    adapter: &mut UiAdapter,
    focus_at_release: Option<String>,
    text: &str,
) -> Result<InsertionOutcome, String> {
    let focus_now = adapter.focused_window();
    if let Some(expected) = focus_at_release
        && expected != focus_now.to_string()
    {
        // The user moved on; typing into the new window would put text
        // somewhere they did not dictate it. Park it on the clipboard.
        return adapter
            .set_clipboard(text)
            .map(|_| InsertionOutcome::ClipboardOnly)
            .map_err(|error| error.to_string());
    }
    match adapter.insert_direct(text) {
        Ok(()) => Ok(InsertionOutcome::Typed),
        Err(X11Error::UnsupportedCharacter(_)) => adapter
            .insert_via_clipboard(text)
            .map(|_| InsertionOutcome::Pasted)
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

fn insert_macos(
    adapter: &mut UiAdapter,
    focus_at_release: Option<String>,
    text: &str,
) -> Result<InsertionOutcome, String> {
    let focus_now = adapter.focused_window();
    if let Some(expected) = focus_at_release
        && expected != focus_now.to_string()
    {
        // The user moved on; typing into the new window would put text
        // somewhere they did not dictate it. Park it on the clipboard.
        return adapter
            .set_clipboard(text)
            .map(|_| InsertionOutcome::ClipboardOnly)
            .map_err(|error| error.to_string());
    }
    // Paste via clipboard is the reliable macOS insertion mechanism; direct
    // CGEvent typing is the fallback (works in some apps, ignored in others).
    match adapter.insert_via_clipboard(text) {
        Ok(()) => Ok(InsertionOutcome::Pasted),
        Err(paste_error) => match adapter.insert_direct(text) {
            Ok(()) => Ok(InsertionOutcome::Typed),
            Err(type_error) => {
                logging::warn(&format!(
                    "macOS paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                ));
                Ok(InsertionOutcome::ClipboardOnly)
            }
        },
    }
}

/// Live overlay sidecar: requests go through a bounded channel serviced by a
/// writer thread, so a wedged overlay drops UI frames instead of ever
/// stalling the daemon loop. Dropping the handle ends the writer thread,
/// which drops the client and kills the process.
struct OverlayHandle {
    tx: mpsc::SyncSender<OverlayRequest>,
}

fn spawn_overlay(
    settings: &Settings,
    events: Sender<DaemonEvent>,
) -> Result<OverlayHandle, String> {
    let (python, args, envs) = settings.overlay_command();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = envs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut client = SidecarClient::spawn_with_env(&python, &arg_refs, &env_refs, move |message| {
        events.send(DaemonEvent::Overlay(message)).is_ok()
    })
    .map_err(|error| format!("cannot start overlay UI sidecar: {error}"))?;
    let (tx, rx) = mpsc::sync_channel::<OverlayRequest>(64);
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            let stop = request == OverlayRequest::Shutdown;
            if client.send(&request).is_err() || stop {
                return;
            }
        }
    });
    Ok(OverlayHandle { tx })
}

/// Status-UI front-end: prefers the GTK overlay sidecar when it is running
/// and ready, otherwise the native X11 bubble. Insertion/clipboard/focus
/// commands keep going to the UI thread directly; only the visuals route
/// through here.
struct UiFront {
    bubble: Sender<UiCommand>,
    overlay: Option<OverlayHandle>,
    overlay_ready: bool,
}

impl UiFront {
    fn overlay_active(&self) -> bool {
        self.overlay.is_some() && self.overlay_ready
    }

    fn overlay_send(&self, request: OverlayRequest) {
        if let Some(handle) = self.overlay.as_ref() {
            // try_send: dropping a UI frame beats blocking the event loop.
            let _ = handle.tx.try_send(request);
        }
    }

    fn show(&self, kind: BubbleKind, text: &str) {
        if self.overlay_active() {
            self.overlay_send(OverlayRequest::Show);
            // While recording, the pill's dot and meter say it all. While
            // finalizing a streaming session, keep the latest partial visible
            // instead of replacing it with a generic "transcribing..." label.
            if matches!(kind, BubbleKind::Recording) && text == "recording..." {
                self.overlay_send(OverlayRequest::Status {
                    text: String::new(),
                });
            } else if matches!(kind, BubbleKind::Transcribing) && text == "transcribing..." {
                // Preserve the current partial/caption until the final arrives.
            } else {
                self.overlay_send(OverlayRequest::Status {
                    text: text.to_string(),
                });
            }
        } else {
            let _ = self
                .bubble
                .send(UiCommand::ShowBubble(kind, text.to_string()));
        }
    }

    fn hide(&self) {
        self.overlay_send(OverlayRequest::Hide);
        let _ = self.bubble.send(UiCommand::HideBubble);
    }

    fn level(&self, elapsed_s: f64, peak: f64, rms: f64) {
        if self.overlay_active() {
            self.overlay_send(OverlayRequest::Recording {
                elapsed_s,
                peak,
                rms,
                segments: 0,
            });
        }
    }
}

fn spawn_sidecar(
    settings: &Settings,
    events: Sender<DaemonEvent>,
) -> Result<SidecarClient, String> {
    let (python, args) = settings.sidecar_command()?;
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut client = SidecarClient::spawn(&python, &arg_refs, move |message| {
        events.send(DaemonEvent::Sidecar(message)).is_ok()
    })
    .map_err(|error| format!("cannot start ASR sidecar: {error}"))?;
    client
        .send(&SidecarRequest::Health)
        .map_err(|error| format!("cannot health-check ASR sidecar: {error}"))?;
    Ok(client)
}

struct SessionTiming {
    session_id: u64,
    pressed_at: Instant,
    released_at: Instant,
    finish_sent_at: Option<Instant>,
    final_at: Option<Instant>,
    polish_done_at: Option<Instant>,
    insert_dispatched_at: Option<Instant>,
}

#[derive(Default)]
struct SessionAudioStats {
    samples: usize,
    sum_squares: f64,
    peak: u16,
}

impl SessionAudioStats {
    fn observe(&mut self, samples: &[i16]) {
        self.samples += samples.len();
        for &sample in samples {
            let sample_f64 = f64::from(sample);
            self.sum_squares += sample_f64 * sample_f64;
            self.peak = self.peak.max(sample.unsigned_abs());
        }
    }

    fn summary(&self) -> String {
        let duration_ms = self.samples / SAMPLES_PER_MS;
        let rms = if self.samples == 0 {
            0.0
        } else {
            (self.sum_squares / self.samples as f64).sqrt()
        };
        format!(
            "{duration_ms}ms audio, {} samples, rms={rms:.0}, peak={}",
            self.samples, self.peak
        )
    }
}

pub fn run(settings: Settings) -> Result<(), Box<dyn Error>> {
    install_signal_handlers();
    let backend = desktop_backend(&settings);
    // Wayland has no global-grab primitive; it relies on compositor bindings
    // driving `sunoto-daemon trigger press|release` over the control socket.
    // X11 (XGrabKey) and macOS (CGEventTap) both install a real global hotkey.
    let shortcut = if backend == DesktopBackend::Wayland {
        None
    } else {
        Some(Shortcut::parse(&settings.shortcut)?)
    };
    let (events_tx, events) = mpsc::channel::<DaemonEvent>();

    // UI thread (insertion/clipboard/bubble on its own backend connection).
    let (ui_tx, ui_rx) = mpsc::channel::<UiCommand>();
    let ui_events = events_tx.clone();
    let ui_handle = std::thread::spawn(move || ui_thread(ui_rx, ui_events, backend));

    // Control socket for compositor/user-triggered push-to-talk edges.
    let control_stop = Arc::new(AtomicBool::new(false));
    let control_handle = spawn_control_thread(events_tx.clone(), Arc::clone(&control_stop))?;

    // Hotkey thread (second X11 connection, blocking with poll timeouts).
    let hotkey_stop = Arc::new(AtomicBool::new(false));
    let hotkey_handle = shortcut
        .map(|shortcut| spawn_hotkey_thread(shortcut, events_tx.clone(), Arc::clone(&hotkey_stop)));
    if backend == DesktopBackend::Wayland {
        logging::info(
            "Wayland session detected; use compositor bindings to call `sunoto-daemon trigger press|release`",
        );
    }

    // Persistent microphone capture bridged into the event channel.
    let capture_stop = Arc::new(AtomicBool::new(false));
    let capture_handle =
        spawn_capture_thread(&settings, events_tx.clone(), Arc::clone(&capture_stop))?;

    // Status UI: GTK overlay sidecar when enabled and startable, X11 bubble
    // otherwise. The overlay is cosmetic — any failure degrades, never aborts.
    let mut ui = UiFront {
        bubble: ui_tx.clone(),
        overlay: None,
        overlay_ready: false,
    };
    let mut overlay_ever_ready = false;
    let mut overlay_respawn_at: Option<Instant> = None;
    let mut overlay_backoff = SIDECAR_BACKOFF_START;
    if settings.overlay_enabled {
        match spawn_overlay(&settings, events_tx.clone()) {
            Ok(handle) => ui.overlay = Some(handle),
            Err(error) => logging::warn(&format!("{error}; using the native X11 bubble")),
        }
    }

    let mut sidecar = Some(spawn_sidecar(&settings, events_tx.clone())?);
    let mut llm_polish = if settings.llm_polish_enabled {
        match llm_polish::LlmPolishClient::spawn(&settings) {
            Ok(client) => Some(client),
            Err(error) => {
                logging::warn(&format!("LLM polish disabled for this daemon run: {error}"));
                None
            }
        }
    } else {
        None
    };
    logging::info(&format!(
        "Sunoto daemon starting: backend={}, desktop={backend:?}, profile={}ms, shortcut={}. Wait for the ASR sidecar ready message; Ctrl+C exits.",
        settings.backend, settings.profile_ms, settings.shortcut
    ));

    let mut machine = SessionMachine::default();
    let mut preroll = AudioPreRoll::new(settings.preroll_ms as usize * SAMPLES_PER_MS);
    let mut focused_class: Option<(String, String)> = None;
    let mut timing: Option<SessionTiming> = None;
    let mut last_pressed_at: Option<Instant> = None;
    let mut audio_stats: Option<SessionAudioStats> = None;
    let mut sidecar_ready = false;
    let mut transcribe_deadline: Option<Instant> = None;
    let mut bubble_hide_at: Option<Instant> = None;
    let mut respawn_at: Option<Instant> = None;
    let mut respawn_backoff = SIDECAR_BACKOFF_START;
    let mut llm_post_asr_warmed = llm_polish.is_none();
    let mut exit_error: Option<String> = None;

    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        match events.recv_timeout(TICK) {
            Ok(DaemonEvent::Hotkey(HotkeyEvent::Pressed)) => {
                if !sidecar_ready {
                    logging::warn("push-to-talk ignored while ASR sidecar is loading");
                    show_error(&ui, "ASR still loading...", &mut bubble_hide_at);
                    continue;
                }
                if llm_polish.is_some() && !llm_post_asr_warmed {
                    logging::warn("push-to-talk ignored while LLM polish is warming");
                    show_error(&ui, "LLM polish still warming...", &mut bubble_hide_at);
                    continue;
                }
                if let SessionAction::Started { session_id } = machine.press() {
                    last_pressed_at = Some(Instant::now());
                    logging::info(&format!("session {session_id}: recording"));
                    ui.show(BubbleKind::Recording, "recording...");
                    bubble_hide_at = None;
                    let preroll_samples = preroll.snapshot();
                    preroll.clear();
                    let mut stats = SessionAudioStats::default();
                    stats.observe(&preroll_samples);
                    audio_stats = Some(stats);
                    if let Some(client) = sidecar.as_mut() {
                        let mut send_result = client.send(&SidecarRequest::StartSession {
                            session_id,
                            profile_ms: settings.profile_ms,
                        });
                        if send_result.is_ok() && !preroll_samples.is_empty() {
                            send_result = client.send(&SidecarRequest::AudioChunk {
                                session_id,
                                samples: preroll_samples,
                            });
                        }
                        if let Err(error) = send_result {
                            handle_sidecar_loss(
                                &format!("sidecar write failed: {error}"),
                                &mut machine,
                                &mut sidecar,
                                &mut respawn_at,
                                &mut respawn_backoff,
                                &ui,
                                &mut bubble_hide_at,
                                &mut timing,
                                &mut transcribe_deadline,
                                &mut sidecar_ready,
                                &mut llm_post_asr_warmed,
                                llm_polish.is_some(),
                            );
                        }
                    } else {
                        machine.fail("ASR sidecar is not running");
                        show_error(&ui, "ASR backend restarting...", &mut bubble_hide_at);
                    }
                }
            }
            Ok(DaemonEvent::Hotkey(HotkeyEvent::Released)) => {
                if let SessionAction::FinishRequested { session_id } = machine.release() {
                    // The class arrives with the fresh focus capture below; a
                    // stale one from an earlier session must not style this one.
                    focused_class = None;
                    let released_at = Instant::now();
                    let recording_ms = last_pressed_at
                        .map(|pressed| released_at.duration_since(pressed).as_millis());
                    timing = Some(SessionTiming {
                        session_id,
                        pressed_at: last_pressed_at.unwrap_or(released_at),
                        released_at,
                        finish_sent_at: None,
                        final_at: None,
                        polish_done_at: None,
                        insert_dispatched_at: None,
                    });
                    // Adaptive transcribe deadline: the offline backend's
                    // latency grows ~linearly with utterance length, so give
                    // long utterances proportionally more headroom than a flat
                    // timeout allows. recorded_ms * rtf + final_timeout_ms.
                    let recorded_ms = last_pressed_at
                        .map(|p| released_at.duration_since(p).as_millis() as u64)
                        .unwrap_or(0);
                    let deadline_ms = settings.final_timeout_ms
                        + (recorded_ms as f64 * settings.final_timeout_rtf) as u64;
                    transcribe_deadline = Some(released_at + Duration::from_millis(deadline_ms));
                    if let Some(stats) = audio_stats.as_ref() {
                        logging::info(&format!(
                            "session {session_id}: sent to ASR: {}{}, timeout {deadline_ms}ms",
                            stats.summary(),
                            recording_ms
                                .map(|ms| format!(", recorded {ms}ms"))
                                .unwrap_or_default(),
                        ));
                    }
                    // Focus is captured at release: that window is where the
                    // user expects the dictated text to land.
                    let _ = ui_tx.send(UiCommand::CaptureFocus);
                    ui.show(BubbleKind::Transcribing, "transcribing...");
                    let request = SidecarRequest::FinishSession { session_id };
                    let send_error = sidecar
                        .as_mut()
                        .and_then(|client| client.send(&request).err());
                    if send_error.is_none()
                        && let Some(timing) = timing.as_mut()
                    {
                        timing.finish_sent_at = Some(Instant::now());
                    }
                    if let Some(error) = send_error {
                        handle_sidecar_loss(
                            &format!("sidecar write failed: {error}"),
                            &mut machine,
                            &mut sidecar,
                            &mut respawn_at,
                            &mut respawn_backoff,
                            &ui,
                            &mut bubble_hide_at,
                            &mut timing,
                            &mut transcribe_deadline,
                            &mut sidecar_ready,
                            &mut llm_post_asr_warmed,
                            llm_polish.is_some(),
                        );
                    }
                }
            }
            Ok(DaemonEvent::Audio(AudioEvent::Frame(samples))) => match machine.state() {
                SessionState::Recording { session_id } => {
                    let session_id = *session_id;
                    if let Some(stats) = audio_stats.as_mut() {
                        stats.observe(&samples);
                    }
                    if ui.overlay_active() {
                        let (peak, rms) = frame_levels(&samples);
                        let elapsed_s = audio_stats
                            .as_ref()
                            .map(|stats| stats.samples as f64 / (SAMPLES_PER_MS as f64 * 1000.0))
                            .unwrap_or(0.0);
                        ui.level(elapsed_s, peak, rms);
                    }
                    let request = SidecarRequest::AudioChunk {
                        session_id,
                        samples,
                    };
                    let send_error = sidecar
                        .as_mut()
                        .and_then(|client| client.send(&request).err());
                    if let Some(error) = send_error {
                        handle_sidecar_loss(
                            &format!("sidecar write failed: {error}"),
                            &mut machine,
                            &mut sidecar,
                            &mut respawn_at,
                            &mut respawn_backoff,
                            &ui,
                            &mut bubble_hide_at,
                            &mut timing,
                            &mut transcribe_deadline,
                            &mut sidecar_ready,
                            &mut llm_post_asr_warmed,
                            llm_polish.is_some(),
                        );
                    }
                }
                _ => preroll.push(&samples),
            },
            Ok(DaemonEvent::Audio(AudioEvent::Started {
                device,
                description,
            })) => {
                let description = description.unwrap_or_else(|| "<unknown>".to_string());
                logging::info(&format!(
                    "microphone capture started: {description} (PulseAudio source: {device})"
                ));
            }
            Ok(DaemonEvent::Audio(AudioEvent::Stopped { reason })) => {
                logging::warn(&format!("microphone capture stopped: {reason}"));
                if matches!(machine.state(), SessionState::Recording { .. }) {
                    logging::warn("microphone lost mid-dictation; the session keeps running");
                }
            }
            Ok(DaemonEvent::Sidecar(SidecarMessage::Event(event))) => match event {
                SidecarEvent::Ready { backend } => {
                    sidecar_ready = true;
                    respawn_backoff = SIDECAR_BACKOFF_START;
                    logging::info(&format!("ASR sidecar ready: {backend}"));
                    llm_post_asr_warmed = llm_polish.is_none();
                    if let Some(client) = llm_polish.as_mut() {
                        logging::info("LLM polish still warming...");
                        ui.show(BubbleKind::Transcribing, "LLM polish still warming...");
                        match client
                            .warmup(&llm_polish::WARMUP_TEXTS, settings.llm_polish_timeout_ms)
                        {
                            Ok(outcome) => {
                                llm_post_asr_warmed = true;
                                logging::info(&format!(
                                    "LLM polish post-ASR warmup complete: {}",
                                    format_llm_warmup_summary(&outcome)
                                ));
                                ui.hide();
                            }
                            Err(error) => {
                                logging::warn(&format!(
                                    "LLM polish disabled for this daemon run after post-ASR warmup failure: {error}"
                                ));
                                llm_polish = None;
                                llm_post_asr_warmed = true;
                                show_error(&ui, "LLM polish unavailable", &mut bubble_hide_at);
                            }
                        }
                    }
                    logging::info(&format!(
                        "Sunoto ready for dictation. Hold {} to dictate.",
                        settings.shortcut
                    ));
                }
                SidecarEvent::SessionStarted { session_id } => {
                    logging::info(&format!("session {session_id}: sidecar accepted"));
                }
                SidecarEvent::Partial { session_id, text } => {
                    if let SessionAction::PartialUpdated { text, .. } =
                        machine.partial(session_id, text)
                    {
                        let kind = match machine.state() {
                            SessionState::Transcribing { .. } => BubbleKind::Transcribing,
                            _ => BubbleKind::Recording,
                        };
                        let tail: String = text
                            .chars()
                            .rev()
                            .take(40)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        ui.show(kind, &tail);
                    }
                }
                SidecarEvent::Final { session_id, text } => {
                    if let SessionAction::Finalized { session_id, text } =
                        machine.finalize(session_id, text)
                    {
                        transcribe_deadline = None;
                        if text.is_empty() {
                            let summary = audio_stats
                                .as_ref()
                                .map(SessionAudioStats::summary)
                                .unwrap_or_else(|| "audio statistics unavailable".to_string());
                            logging::warn(&format!(
                                "session {session_id}: ASR backend returned an empty transcript ({summary})"
                            ));
                        } else {
                            logging::info(&format!(
                                "session {session_id}: final transcript: {text:?}"
                            ));
                        }
                        audio_stats = None;
                        if let Some(timing) = timing.as_mut() {
                            timing.final_at = Some(Instant::now());
                            let release_to_final = timing.released_at.elapsed().as_millis();
                            let asr_turnaround = timing.finish_sent_at.map(|sent| {
                                timing.final_at.unwrap().duration_since(sent).as_millis()
                            });
                            logging::info(&format!(
                                "session {session_id}: release-to-final {release_to_final}ms{}",
                                asr_turnaround
                                    .map(|ms| format!(", ASR turnaround {ms}ms"))
                                    .unwrap_or_default(),
                            ));
                        }
                        let raw_text = text.clone();
                        let mut output = if settings.polish_enabled {
                            let style = resolve_style(
                                focused_class
                                    .as_ref()
                                    .map(|(instance, class)| (instance.as_str(), class.as_str())),
                                &settings.polish.app_styles,
                                settings.polish.style,
                            );
                            let mut config = std::borrow::Cow::Borrowed(&settings.polish);
                            if style != config.style {
                                logging::info(&format!(
                                    "session {session_id}: style {style:?} for focused window class {:?}",
                                    focused_class
                                        .as_ref()
                                        .map(|(_, class)| class.as_str())
                                        .unwrap_or("<unknown>")
                                ));
                                config.to_mut().style = style;
                            }
                            let outcome = polish(&text, &config);
                            for stage in &outcome.trace {
                                logging::info(&format!(
                                    "session {session_id}: polish {}: {:?} -> {:?}",
                                    stage.stage, stage.before, stage.after
                                ));
                            }
                            outcome.text
                        } else {
                            text
                        };
                        let mut streaming_inserted = false;
                        if settings.llm_polish_enabled
                            && !raw_text.trim().is_empty()
                            && let Some(client) = llm_polish.as_mut()
                        {
                            ui.show(BubbleKind::Transcribing, "polishing...");
                            let llm_input = output.clone();
                            let stream_insert = settings.llm_polish_stream_insert;
                            let first = std::cell::Cell::new(true);
                            let dispatched = std::cell::Cell::new(false);
                            let ui_tx_ref = &ui_tx;
                            let mut on_chunk = |delta: &str| {
                                if !stream_insert {
                                    return;
                                }
                                let is_first = first.get();
                                first.set(false);
                                dispatched.set(true);
                                let _ = ui_tx_ref.send(UiCommand::InsertStreamChunk {
                                    session_id,
                                    first: is_first,
                                    delta: delta.to_string(),
                                });
                            };
                            match client.polish_stream(
                                session_id,
                                &llm_input,
                                settings.llm_polish_timeout_ms,
                                &mut on_chunk,
                            ) {
                                Ok(outcome) => {
                                    let ttft = outcome.diagnostics.ttft_ms;
                                    let streamed =
                                        outcome.diagnostics.streamed == Some(true);
                                    let chunks =
                                        outcome.diagnostics.stream_chunks.unwrap_or(0);
                                    logging::info(&format!(
                                        "session {session_id}: llm polish accepted in {}ms (ttft {}ms, streamed={} {}chunks){}: {:?} -> {:?}",
                                        outcome.latency_ms,
                                        ttft
                                            .map(|ms| ms.to_string())
                                            .unwrap_or_else(|| "?".into()),
                                        streamed,
                                        chunks,
                                        format_llm_diagnostics(&outcome.diagnostics),
                                        llm_input,
                                        outcome.text
                                    ));
                                    if stream_insert && dispatched.get() {
                                        // Progressive insertion already typed
                                        // the text; finalize the streaming
                                        // session without a second atomic
                                        // paste. The typed text is kept as
                                        // authoritative (a content-loss
                                        // guard revert after streaming is an
                                        // accepted edge; see
                                        // docs/llm-polish-streaming-plan.md).
                                        let sanitized_outcome =
                                            sanitize_for_insertion(
                                                &outcome.text,
                                                settings.allow_enter_and_tab,
                                            );
                                        if let Some(timing) = timing.as_mut() {
                                            timing.polish_done_at =
                                                Some(Instant::now());
                                            timing.insert_dispatched_at =
                                                Some(Instant::now());
                                        }
                                        if sanitized_outcome.is_empty() {
                                            logging::info(&format!(
                                                "session {session_id}: empty result"
                                            ));
                                            ui.hide();
                                        } else {
                                            let _ = ui_tx.send(
                                                UiCommand::InsertStreamEnd {
                                                    session_id,
                                                    final_text: sanitized_outcome,
                                                    streamed_ok: true,
                                                },
                                            );
                                        }
                                        streaming_inserted = true;
                                    } else {
                                        output = outcome.text;
                                    }
                                }
                                Err(error) => {
                                    logging::warn(&format!(
                                        "session {session_id}: llm polish skipped: {error}"
                                    ));
                                    if stream_insert && dispatched.get() {
                                        // Chunks already typed; finalize what
                                        // we have even though the call erred
                                        // (e.g. timeout past the last token).
                                        // The partial typed text is kept as-is:
                                        // pasting `output` again would
                                        // DUPLICATE what was already typed,
                                        // so we keep the typed (possibly
                                        // truncated) text and log it.
                                        logging::warn(&format!(
                                            "session {session_id}: streaming insert kept partial text after llm error; result may be truncated"
                                        ));
                                        if let Some(timing) = timing.as_mut() {
                                            timing.polish_done_at =
                                                Some(Instant::now());
                                            timing.insert_dispatched_at =
                                                Some(Instant::now());
                                        }
                                        let _ = ui_tx.send(
                                            UiCommand::InsertStreamEnd {
                                                session_id,
                                                final_text: output.clone(),
                                                streamed_ok: true,
                                            },
                                        );
                                        streaming_inserted = true;
                                    }
                                }
                            }
                        }
                        if streaming_inserted {
                            // Streaming insertion already dispatched the text;
                            // skip the standard atomic paste path below.
                        } else {
                            let sanitized =
                                sanitize_for_insertion(&output, settings.allow_enter_and_tab);
                            if let Some(timing) = timing.as_mut() {
                                timing.polish_done_at = Some(Instant::now());
                            }
                            if sanitized.is_empty() {
                                logging::info(&format!("session {session_id}: empty result"));
                                ui.hide();
                            } else {
                                if let Some(timing) = timing.as_mut() {
                                    timing.insert_dispatched_at = Some(Instant::now());
                                }
                                let _ = ui_tx.send(UiCommand::Insert {
                                    session_id,
                                    text: sanitized,
                                });
                            }
                        }
                    }
                }
                SidecarEvent::Error {
                    session_id,
                    message,
                } => {
                    if message == "superseded" {
                        logging::warn(&format!("sidecar superseded session {session_id:?}"));
                    } else if session_id.is_none() || session_id == machine.current_session() {
                        if let SessionAction::Failed { message } = machine.fail(message) {
                            logging::error(&format!("sidecar error: {message}"));
                            show_error(&ui, &message, &mut bubble_hide_at);
                        }
                        timing = None;
                        transcribe_deadline = None;
                        audio_stats = None;
                    } else {
                        logging::warn(&format!(
                            "sidecar error for stale session {session_id:?}: {message}"
                        ));
                    }
                }
            },
            Ok(DaemonEvent::Sidecar(SidecarMessage::Garbage { line })) => {
                logging::warn(&format!("ignored non-protocol sidecar output: {line}"));
            }
            Ok(DaemonEvent::Sidecar(SidecarMessage::Closed)) => {
                handle_sidecar_loss(
                    "ASR sidecar exited",
                    &mut machine,
                    &mut sidecar,
                    &mut respawn_at,
                    &mut respawn_backoff,
                    &ui,
                    &mut bubble_hide_at,
                    &mut timing,
                    &mut transcribe_deadline,
                    &mut sidecar_ready,
                    &mut llm_post_asr_warmed,
                    llm_polish.is_some(),
                );
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(SidecarEvent::Ready { backend }))) => {
                ui.overlay_ready = true;
                overlay_ever_ready = true;
                overlay_backoff = SIDECAR_BACKOFF_START;
                logging::info(&format!("overlay UI ready ({backend})"));
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(event))) => {
                logging::warn(&format!("unexpected overlay event: {event:?}"));
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Garbage { line })) => {
                logging::warn(&format!("ignored non-protocol overlay output: {line}"));
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Closed)) => {
                if ui.overlay.is_none() {
                    // Already torn down (shutdown path); nothing to do.
                } else if overlay_ever_ready {
                    logging::warn("overlay UI exited; respawning");
                    ui.overlay = None;
                    ui.overlay_ready = false;
                    overlay_respawn_at = Some(Instant::now() + overlay_backoff);
                    overlay_backoff = (overlay_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                } else {
                    // Never came up — most likely GTK4 is not installed.
                    // Permanent fallback beats a respawn loop of noise.
                    logging::warn(
                        "overlay UI exited before becoming ready (GTK4 missing?); using the native X11 bubble",
                    );
                    ui.overlay = None;
                    ui.overlay_ready = false;
                }
            }
            Ok(DaemonEvent::FocusClass(class)) => {
                // Logged per session: when text "disappears", the first
                // question is always which window actually received it.
                match class.as_ref() {
                    Some((instance, class_name)) => logging::info(&format!(
                        "insertion target at release: {instance:?} / {class_name:?}"
                    )),
                    None => logging::warn(
                        "insertion target at release has no WM_CLASS (text may land in a non-text window)",
                    ),
                }
                focused_class = class;
            }
            Ok(DaemonEvent::Ui(report)) => {
                match report.result {
                    Ok(outcome) => {
                        let insert_ms = report.insert_duration.as_millis();
                        if let Some(timing) = timing
                            .as_ref()
                            .filter(|timing| timing.session_id == report.session_id)
                        {
                            let now = Instant::now();
                            let recording = timing
                                .released_at
                                .duration_since(timing.pressed_at)
                                .as_millis();
                            let release_to_finish_sent = timing
                                .finish_sent_at
                                .map(|t| t.duration_since(timing.released_at).as_millis());
                            let asr_turnaround = match (timing.finish_sent_at, timing.final_at) {
                                (Some(sent), Some(final_)) => {
                                    Some(final_.duration_since(sent).as_millis())
                                }
                                _ => None,
                            };
                            let release_to_final = timing
                                .final_at
                                .map(|f| f.duration_since(timing.released_at).as_millis());
                            let polish_ms = match (timing.final_at, timing.polish_done_at) {
                                (Some(final_), Some(done)) => {
                                    Some(done.duration_since(final_).as_millis())
                                }
                                _ => None,
                            };
                            let dispatch_ms =
                                match (timing.polish_done_at, timing.insert_dispatched_at) {
                                    (Some(done), Some(dispatched)) => {
                                        Some(dispatched.duration_since(done).as_millis())
                                    }
                                    _ => None,
                                };
                            let insert_wait_ms = match (timing.insert_dispatched_at, now) {
                                (Some(dispatched), _) => {
                                    Some(now.duration_since(dispatched).as_millis())
                                }
                                _ => None,
                            };
                            let release_to_insertion =
                                now.duration_since(timing.released_at).as_millis();
                            logging::info(&format!(
                                "session {}: inserted via {:?}; timing breakdown: recorded {recording}ms, release->finish_sent {}ms, ASR turnaround {}ms, release->final {}ms, polish {}ms, dispatch {}ms, insert {insert_ms}ms (waited {}ms for UI thread), release->insertion {release_to_insertion}ms",
                                report.session_id,
                                outcome,
                                release_to_finish_sent.unwrap_or(0),
                                asr_turnaround
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                release_to_final
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                polish_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                dispatch_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                insert_wait_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                            ));
                        } else {
                            logging::info(&format!(
                                "session {}: inserted via {:?} in {insert_ms}ms",
                                report.session_id, outcome
                            ));
                        }
                        if outcome == InsertionOutcome::ClipboardOnly {
                            show_error(
                                &ui,
                                "focus changed; result is in the clipboard",
                                &mut bubble_hide_at,
                            );
                        } else {
                            ui.hide();
                        }
                    }
                    Err(message) => {
                        logging::error(&format!(
                            "session {}: insertion failed: {message}",
                            report.session_id
                        ));
                        show_error(&ui, "insertion failed", &mut bubble_hide_at);
                    }
                }
                timing = None;
            }
            Ok(DaemonEvent::ControlPolish { text, mut response }) => {
                let result = if matches!(machine.state(), SessionState::Idle) {
                    control_polish_response(&settings, &mut llm_polish, llm_post_asr_warmed, &text)
                } else {
                    serde_json::json!({
                        "type": "polish_result",
                        "ok": false,
                        "error": "daemon is busy with a dictation session",
                    })
                };
                if serde_json::to_writer(&mut response, &result).is_ok() {
                    let _ = response.write_all(b"\n");
                }
            }
            Ok(DaemonEvent::Fatal(message)) => {
                exit_error = Some(message);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                exit_error = Some("event channel disconnected".into());
                break;
            }
        }

        // Watchdogs and deferred work, evaluated on every loop pass.
        if let Some(deadline) = transcribe_deadline
            && Instant::now() >= deadline
        {
            transcribe_deadline = None;
            if let SessionState::Transcribing { session_id } = *machine.state() {
                if let Some(client) = sidecar.as_mut() {
                    let _ = client.send(&SidecarRequest::CancelSession { session_id });
                }
                machine.fail("ASR timed out");
                logging::error(&format!("session {session_id}: ASR timed out"));
                show_error(&ui, "ASR timed out", &mut bubble_hide_at);
                timing = None;
                audio_stats = None;
            }
        }
        if let Some(hide_at) = bubble_hide_at
            && Instant::now() >= hide_at
        {
            bubble_hide_at = None;
            ui.hide();
        }
        if ui.overlay.is_none()
            && let Some(at) = overlay_respawn_at
            && Instant::now() >= at
        {
            overlay_respawn_at = None;
            match spawn_overlay(&settings, events_tx.clone()) {
                Ok(handle) => {
                    logging::info("overlay UI respawned");
                    ui.overlay = Some(handle);
                }
                Err(error) => {
                    logging::warn(&format!("overlay respawn failed: {error}"));
                    overlay_backoff = (overlay_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                    overlay_respawn_at = Some(Instant::now() + overlay_backoff);
                }
            }
        }
        if sidecar.is_none()
            && let Some(at) = respawn_at
            && Instant::now() >= at
        {
            respawn_at = None;
            match spawn_sidecar(&settings, events_tx.clone()) {
                Ok(client) => {
                    logging::info("ASR sidecar respawned");
                    sidecar = Some(client);
                }
                Err(error) => {
                    logging::error(&format!("sidecar respawn failed: {error}"));
                    respawn_backoff = (respawn_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                    respawn_at = Some(Instant::now() + respawn_backoff);
                }
            }
        }
    }

    logging::info("shutting down");
    if let SessionState::Recording { session_id } | SessionState::Transcribing { session_id } =
        *machine.state()
        && let Some(client) = sidecar.as_mut()
    {
        let _ = client.send(&SidecarRequest::CancelSession { session_id });
    }
    ui.overlay_send(OverlayRequest::Shutdown);
    ui.overlay = None;
    let _ = ui_tx.send(UiCommand::Shutdown);
    control_stop.store(true, Ordering::SeqCst);
    hotkey_stop.store(true, Ordering::SeqCst);
    capture_stop.store(true, Ordering::SeqCst);
    drop(sidecar);
    let _ = ui_handle.join();
    let _ = control_handle.join();
    if let Some(hotkey_handle) = hotkey_handle {
        let _ = hotkey_handle.join();
    }
    let _ = capture_handle.join();
    match exit_error {
        Some(message) => Err(message.into()),
        None => Ok(()),
    }
}

fn show_error(ui: &UiFront, message: &str, bubble_hide_at: &mut Option<Instant>) {
    ui.show(BubbleKind::Error, message);
    *bubble_hide_at = Some(Instant::now() + ERROR_BUBBLE_VISIBLE);
}

fn control_polish_response(
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

fn llm_diagnostics_json(diagnostics: &llm_polish::LlmPolishDiagnostics) -> serde_json::Value {
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

fn llama_perf_json(perf: &llm_polish::LlamaPerf) -> serde_json::Value {
    serde_json::json!({
        "prompt_eval_ms": perf.prompt_eval_ms,
        "prompt_eval_tokens": perf.prompt_eval_tokens,
        "eval_ms": perf.eval_ms,
        "eval_tokens": perf.eval_tokens,
        "reused_tokens": perf.reused_tokens,
        "load_ms": perf.load_ms,
    })
}

fn llm_call_diagnostics_json(call: &llm_polish::LlmPolishCallDiagnostics) -> serde_json::Value {
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

fn format_llm_warmup_summary(outcome: &llm_polish::LlmPolishWarmupOutcome) -> String {
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

fn format_llm_diagnostics(diagnostics: &llm_polish::LlmPolishDiagnostics) -> String {
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

/// Per-frame meter levels, normalized to 0..1 for the overlay.
fn frame_levels(samples: &[i16]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0u16;
    let mut sum_squares = 0.0f64;
    for &sample in samples {
        peak = peak.max(sample.unsigned_abs());
        let sample_f64 = f64::from(sample);
        sum_squares += sample_f64 * sample_f64;
    }
    let rms = (sum_squares / samples.len() as f64).sqrt();
    (f64::from(peak) / 32768.0, rms / 32768.0)
}

#[allow(clippy::too_many_arguments)]
fn handle_sidecar_loss(
    reason: &str,
    machine: &mut SessionMachine,
    sidecar: &mut Option<SidecarClient>,
    respawn_at: &mut Option<Instant>,
    respawn_backoff: &mut Duration,
    ui: &UiFront,
    bubble_hide_at: &mut Option<Instant>,
    timing: &mut Option<SessionTiming>,
    transcribe_deadline: &mut Option<Instant>,
    sidecar_ready: &mut bool,
    llm_post_asr_warmed: &mut bool,
    llm_polish_active: bool,
) {
    if sidecar.is_none() {
        return;
    }
    logging::error(reason);
    *sidecar = None;
    *sidecar_ready = false;
    *llm_post_asr_warmed = !llm_polish_active;
    if let SessionAction::Failed { .. } = machine.fail(reason) {
        show_error(ui, "ASR backend lost; restarting", bubble_hide_at);
    }
    *timing = None;
    *transcribe_deadline = None;
    *respawn_at = Some(Instant::now() + *respawn_backoff);
    *respawn_backoff = (*respawn_backoff * 2).min(SIDECAR_BACKOFF_CAP);
}

fn spawn_control_thread(
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

fn handle_control_stream(stream: UnixStream, events: &Sender<DaemonEvent>) -> bool {
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    if reader.read_line(&mut line).is_err() {
        return true;
    }
    let trimmed = line.trim();
    let event = match trimmed {
        "press" => Some(HotkeyEvent::Pressed),
        "release" => Some(HotkeyEvent::Released),
        other => match serde_json::from_str::<ControlCommand>(other) {
            Ok(ControlCommand::Polish { text }) => {
                return events
                    .send(DaemonEvent::ControlPolish {
                        text,
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

fn spawn_hotkey_thread(
    shortcut: Shortcut,
    events: Sender<DaemonEvent>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = match HotkeyListener::open(&shortcut) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = events.send(DaemonEvent::Fatal(format!(
                    "global shortcut unavailable: {error}"
                )));
                return;
            }
        };
        while !stop.load(Ordering::SeqCst) {
            if let Some(event) = listener.wait(Duration::from_millis(250))
                && events.send(DaemonEvent::Hotkey(event)).is_err()
            {
                return;
            }
        }
    })
}

fn spawn_capture_thread(
    settings: &Settings,
    events: Sender<DaemonEvent>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, Box<dyn Error>> {
    let device = settings.microphone.clone();
    Ok(std::thread::spawn(move || {
        // Retry capture startup with a backoff. On macOS the first attempt
        // may fail until Microphone TCC permission is granted (the prompt
        // appears on the first access); dying here would make the daemon
        // unstartable before the user can grant it. Mirror the Linux
        // capture-restart behavior: log, back off, and try again.
        let backoffs = [250, 500, 1000, 2000, 4000];
        let mut attempt = 0usize;
        let mut capture: Option<sunoto_audio::CaptureHandle> = None;
        while !stop.load(Ordering::SeqCst) {
            if capture.is_none() {
                match start_capture(CaptureConfig {
                    device: device.clone(),
                    ..CaptureConfig::default()
                }) {
                    Ok(handle) => capture = Some(handle),
                    Err(error) => {
                        logging::warn(&format!(
                            "microphone capture unavailable: {error}; retrying"
                        ));
                        let delay = backoffs[attempt.min(backoffs.len() - 1)];
                        attempt += 1;
                        // Sleep in slices so stop() interrupts promptly.
                        let mut remaining = Duration::from_millis(delay);
                        while !remaining.is_zero() && !stop.load(Ordering::SeqCst) {
                            let step = remaining.min(Duration::from_millis(50));
                            std::thread::sleep(step);
                            remaining = remaining.saturating_sub(step);
                        }
                        continue;
                    }
                }
            }
            let Some(handle) = capture.as_ref() else {
                continue;
            };
            match handle.events().recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if events.send(DaemonEvent::Audio(event)).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // The capture stream ended (mic unplugged, permission
                    // revoked, ...). Drop and retry.
                    if let Some(h) = capture.take() {
                        h.stop();
                    }
                    attempt = 0;
                }
            }
        }
        if let Some(h) = capture.take() {
            h.stop();
        }
    }))
}

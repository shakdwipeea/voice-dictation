use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::raw::c_int;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sunoto_audio::{AudioEvent, CaptureConfig, start_capture};
use sunoto_context::{AgentProcess, DetectedAgent, FileIndex, detect_agent};
use sunoto_core::{AudioPreRoll, SessionAction, SessionMachine, SessionState};
use sunoto_ipc::{OverlayRequest, SidecarClient, SidecarEvent, SidecarMessage, SidecarRequest};
use sunoto_linux::x11::{
    BubbleKind, HotkeyEvent, HotkeyListener, InsertionOutcome, Shortcut, UiAdapter, X11Error,
};
use sunoto_polish::{FileMatchIndex, polish, resolve_file_references, resolve_style};

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
    /// Focused-window context (class, PID, title) captured at shortcut release;
    /// reported by the UI thread right after CaptureFocus.
    Focus(FocusInfo),
    Fatal(String),
}

/// Focused-window context the main loop needs: the WM_CLASS for styling and the
/// PID/title for detecting a developer tool (e.g. Claude Code) running inside.
pub struct FocusInfo {
    pub class: Option<(String, String)>,
    pub pid: Option<u32>,
    pub title: Option<String>,
}

pub struct UiReport {
    pub session_id: u64,
    pub result: Result<InsertionOutcome, String>,
    pub insert_duration: Duration,
}

pub enum UiCommand {
    CaptureFocus,
    ShowBubble(BubbleKind, String),
    HideBubble,
    Insert { session_id: u64, text: String },
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesktopBackend {
    X11,
    Wayland,
}

fn desktop_backend(settings: &Settings) -> DesktopBackend {
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
    pid: Option<u32>,
    title: Option<String>,
}

enum UiBackend {
    X11(UiAdapter),
    Wayland(WaylandUiAdapter),
}

impl UiBackend {
    fn open(backend: DesktopBackend) -> Result<Self, String> {
        match backend {
            DesktopBackend::X11 => UiAdapter::open()
                .map(Self::X11)
                .map_err(|error| format!("X11 UI unavailable: {error}")),
            DesktopBackend::Wayland => WaylandUiAdapter::open().map(Self::Wayland),
        }
    }

    fn capture_focus(&mut self) -> FocusSnapshot {
        match self {
            Self::X11(adapter) => {
                let focus = adapter.focused_window();
                let context = adapter.window_context(focus);
                FocusSnapshot {
                    token: Some(focus.to_string()),
                    class: context.class,
                    pid: context.pid,
                    title: context.title,
                }
            }
            Self::Wayland(adapter) => adapter.capture_focus(),
        }
    }

    fn show_bubble(&mut self, kind: BubbleKind, text: &str) {
        match self {
            Self::X11(adapter) => adapter.bubble_show(kind, text),
            Self::Wayland(_) => {
                let _ = (kind, text);
            }
        }
    }

    fn hide_bubble(&mut self) {
        match self {
            Self::X11(adapter) => adapter.bubble_hide(),
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
            Self::Wayland(adapter) => adapter.insert(focus_at_release, text),
        }
    }

    fn pump(&mut self) {
        if let Self::X11(adapter) = self {
            adapter.pump();
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
                pid: window.pid,
                title: window.title,
            },
            None => FocusSnapshot {
                token: None,
                class: None,
                pid: None,
                title: None,
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

/// Resolve spoken file references to the focused coding agent's mention syntax
/// (e.g. Claude Code / Gemini CLI `@path`) when the focused window is a terminal
/// running a configured agent. Gated by config and the terminal-class check so
/// the `/proc` scan never runs for ordinary dictation; the per-cwd file index is
/// cached. Any miss leaves the text untouched.
fn resolve_agent_file_references(
    text: String,
    settings: &Settings,
    focused_class: Option<&(String, String)>,
    focused_pid: Option<u32>,
    cache: &mut Option<(PathBuf, FileMatchIndex)>,
    session_id: u64,
) -> String {
    let config = &settings.polish.file_references;
    if !config.enabled || config.agents.is_empty() {
        return text;
    }
    let Some(pid) = focused_pid else {
        return text;
    };
    // Coding agents run inside a terminal; skip the /proc scan for anything else.
    let in_terminal = focused_class
        .is_some_and(|(instance, class)| is_terminal_class(instance) || is_terminal_class(class));
    if !in_terminal {
        return text;
    }
    let specs: Vec<AgentProcess> = config
        .agents
        .iter()
        .map(|agent| AgentProcess {
            name: agent.name.clone(),
            comm: agent.process.clone(),
        })
        .collect();
    let Some(DetectedAgent { name, cwd }) = detect_agent(pid, &specs) else {
        return text;
    };
    let Some(agent) = config.agents.iter().find(|agent| agent.name == name) else {
        return text;
    };
    if cache.as_ref().map(|(root, _)| root.as_path()) != Some(cwd.as_path()) {
        logging::info(&format!(
            "session {session_id}: indexing {} for {name} file references",
            cwd.display()
        ));
        // List the files (sunoto-context), then precompute the matcher's keys
        // once here, off the per-utterance path, and cache both keyed by cwd.
        let listing = FileIndex::build(&cwd, config.max_index_files);
        *cache = Some((cwd.clone(), FileMatchIndex::build(&listing.files)));
    }
    let Some((_, file_index)) = cache.as_ref() else {
        return text;
    };
    let resolved = resolve_file_references(&text, file_index, &agent.ref_template, config);
    if resolved != text {
        logging::info(&format!(
            "session {session_id}: resolved {name} file references"
        ));
    }
    resolved
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
    pid: Option<u32>,
    title: Option<String>,
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
    let pid = payload
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map(|pid| pid as u32)
        .filter(|&pid| pid != 0);
    let title = payload
        .get("title")
        .and_then(serde_json::Value::as_str)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    Some(HyprlandWindow {
        address,
        class,
        pid,
        title,
    })
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
    loop {
        match commands.recv_timeout(Duration::from_millis(25)) {
            Ok(UiCommand::CaptureFocus) => {
                let focus = adapter.capture_focus();
                focus_at_release = focus.token;
                let report = DaemonEvent::Focus(FocusInfo {
                    class: focus.class,
                    pid: focus.pid,
                    title: focus.title,
                });
                if events.send(report).is_err() {
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
            // While recording, the pill's dot and meter say it all; text
            // only appears for transcribing/partial/error states.
            let status = if matches!(kind, BubbleKind::Recording) && text == "recording..." {
                String::new()
            } else {
                text.to_string()
            };
            self.overlay_send(OverlayRequest::Status { text: status });
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
    released_at: Instant,
    final_at: Option<Instant>,
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
    let shortcut = if backend == DesktopBackend::X11 {
        Some(Shortcut::parse(&settings.shortcut)?)
    } else {
        None
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
    logging::info(&format!(
        "Sunoto daemon starting: backend={}, desktop={backend:?}, profile={}ms, shortcut={}. Wait for the ASR sidecar ready message; Ctrl+C exits.",
        settings.backend, settings.profile_ms, settings.shortcut
    ));

    let mut machine = SessionMachine::default();
    let mut preroll = AudioPreRoll::new(settings.preroll_ms as usize * SAMPLES_PER_MS);
    let mut focused_class: Option<(String, String)> = None;
    let mut focused_pid: Option<u32> = None;
    let mut file_index: Option<(PathBuf, FileMatchIndex)> = None;
    let mut timing: Option<SessionTiming> = None;
    let mut audio_stats: Option<SessionAudioStats> = None;
    let mut sidecar_ready = false;
    let mut transcribe_deadline: Option<Instant> = None;
    let mut bubble_hide_at: Option<Instant> = None;
    let mut respawn_at: Option<Instant> = None;
    let mut respawn_backoff = SIDECAR_BACKOFF_START;
    let mut exit_error: Option<String> = None;

    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        match events.recv_timeout(TICK) {
            Ok(DaemonEvent::Hotkey(HotkeyEvent::Pressed)) => {
                if !sidecar_ready {
                    logging::warn("push-to-talk ignored while ASR sidecar is loading");
                    show_error(&ui, "ASR still loading...", &mut bubble_hide_at);
                    continue;
                }
                if let SessionAction::Started { session_id } = machine.press() {
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
                    focused_pid = None;
                    timing = Some(SessionTiming {
                        session_id,
                        released_at: Instant::now(),
                        final_at: None,
                    });
                    transcribe_deadline =
                        Some(Instant::now() + Duration::from_millis(settings.final_timeout_ms));
                    if let Some(stats) = audio_stats.as_ref() {
                        logging::info(&format!(
                            "session {session_id}: sent to ASR: {}",
                            stats.summary()
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
                    logging::info(&format!(
                        "ASR sidecar ready: {backend}. Hold {} to dictate.",
                        settings.shortcut
                    ));
                    respawn_backoff = SIDECAR_BACKOFF_START;
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
                            logging::info(&format!(
                                "session {session_id}: release-to-final {}ms",
                                timing.released_at.elapsed().as_millis()
                            ));
                        }
                        let output = if settings.polish_enabled {
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
                        let output = resolve_agent_file_references(
                            output,
                            &settings,
                            focused_class.as_ref(),
                            focused_pid,
                            &mut file_index,
                            session_id,
                        );
                        let sanitized =
                            sanitize_for_insertion(&output, settings.allow_enter_and_tab);
                        if sanitized.is_empty() {
                            logging::info(&format!("session {session_id}: empty result"));
                            ui.hide();
                        } else {
                            logging::info(&format!(
                                "session {session_id}: inserting {sanitized:?}"
                            ));
                            let _ = ui_tx.send(UiCommand::Insert {
                                session_id,
                                text: sanitized,
                            });
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
            Ok(DaemonEvent::Focus(focus)) => {
                // Logged per session: when text "disappears", the first
                // question is always which window actually received it.
                match focus.class.as_ref() {
                    Some((instance, class_name)) => logging::info(&format!(
                        "insertion target at release: {instance:?} / {class_name:?}{}",
                        focus
                            .title
                            .as_deref()
                            .map(|title| format!(" title {title:?}"))
                            .unwrap_or_default()
                    )),
                    None => logging::warn(
                        "insertion target at release has no WM_CLASS (text may land in a non-text window)",
                    ),
                }
                focused_class = focus.class;
                focused_pid = focus.pid;
            }
            Ok(DaemonEvent::Ui(report)) => {
                match report.result {
                    Ok(outcome) => {
                        let total = timing
                            .as_ref()
                            .filter(|timing| timing.session_id == report.session_id)
                            .map(|timing| timing.released_at.elapsed().as_millis());
                        logging::info(&format!(
                            "session {}: inserted via {:?} in {}ms{}",
                            report.session_id,
                            outcome,
                            report.insert_duration.as_millis(),
                            total
                                .map(|ms| format!(", release-to-insertion {ms}ms"))
                                .unwrap_or_default()
                        ));
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
) {
    if sidecar.is_none() {
        return;
    }
    logging::error(reason);
    *sidecar = None;
    *sidecar_ready = false;
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
    let event = match line.trim() {
        "press" => HotkeyEvent::Pressed,
        "release" => HotkeyEvent::Released,
        other => {
            logging::warn(&format!("ignored unknown control command: {other:?}"));
            return true;
        }
    };
    events.send(DaemonEvent::Hotkey(event)).is_ok()
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
    let capture = start_capture(CaptureConfig {
        device: settings.microphone.clone(),
        ..CaptureConfig::default()
    })?;
    Ok(std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match capture.events().recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if events.send(DaemonEvent::Audio(event)).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        capture.stop();
    }))
}

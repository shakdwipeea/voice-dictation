//! The UI thread: focus capture, text insertion, clipboard, and the native
//! fallback bubble, on its own desktop connection so the event loop never
//! blocks on a window server.
//!
//! Insertion ordering differs per platform for empirical reasons and lives
//! in the platform submodules: X11 types first (XTEST) and pastes on
//! unsupported characters; macOS and Wayland paste first and type as the
//! fallback. Every path parks the text on the clipboard when the focused
//! window changed since the shortcut was released.

mod macos;
mod wayland;
mod x11;

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use sunoto_desktop::{BubbleKind, InsertionOutcome, UiAdapter};

use crate::events::DaemonEvent;
use crate::logging;
use crate::settings::Settings;

use macos::insert_macos;
use wayland::WaylandUiAdapter;
use x11::insert_x11;

pub struct UiReport {
    pub session_id: u64,
    pub result: Result<InsertionOutcome, String>,
    pub insert_duration: Duration,
}

/// Per-session streaming-insertion context owned by the UI thread.
pub(crate) struct StreamSession {
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
    Insert {
        session_id: u64,
        text: String,
    },
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
pub(crate) enum DesktopBackend {
    X11,
    Wayland,
    Macos,
}

pub(crate) fn desktop_backend(settings: &Settings) -> DesktopBackend {
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

pub(crate) struct FocusSnapshot {
    pub(crate) token: Option<String>,
    pub(crate) class: Option<(String, String)>,
}

pub(crate) enum UiBackend {
    X11(UiAdapter),
    Wayland(WaylandUiAdapter),
    // Phase 2-4: replace with a real `sunoto-macos` adapter (CGEventTap,
    // CoreAudio, CGEvent insertion, NSPasteboard, NSWorkspace focus).
    Macos(UiAdapter),
}

impl UiBackend {
    pub(crate) fn open(backend: DesktopBackend) -> Result<Self, String> {
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

    pub(crate) fn capture_focus(&mut self) -> FocusSnapshot {
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

    pub(crate) fn show_bubble(&mut self, kind: BubbleKind, text: &str) {
        match self {
            Self::X11(adapter) => adapter.bubble_show(kind, text),
            Self::Macos(adapter) => adapter.bubble_show(kind, text),
            Self::Wayland(_) => {
                let _ = (kind, text);
            }
        }
    }

    pub(crate) fn hide_bubble(&mut self) {
        match self {
            Self::X11(adapter) => adapter.bubble_hide(),
            Self::Macos(adapter) => adapter.bubble_hide(),
            Self::Wayland(_) => {}
        }
    }

    pub(crate) fn insert(
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
    pub(crate) fn focus_matches(&self, expected: Option<&str>) -> bool {
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
    pub(crate) fn type_chunk(&mut self, text: &str) -> Result<(), String> {
        match self {
            Self::X11(adapter) | Self::Macos(adapter) => adapter
                .insert_direct(text)
                .map_err(|error| error.to_string()),
            Self::Wayland(adapter) => adapter.type_direct(text),
        }
    }

    pub(crate) fn pump(&mut self) {
        match self {
            Self::X11(adapter) | Self::Macos(adapter) => adapter.pump(),
            Self::Wayland(_) => {}
        }
    }
}

/// UI thread: owns insertion, clipboard, and fallback status-bubble operations.
pub(crate) fn ui_thread(
    commands: Receiver<UiCommand>,
    events: Sender<DaemonEvent>,
    backend: DesktopBackend,
) {
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
                if session.focus_ok
                    && session.typed_ok
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

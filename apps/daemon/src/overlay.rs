//! Status UI front-end. Prefers the overlay sidecar (Swift on macOS, GTK on
//! Linux) when it is running and ready, otherwise the native bubble on the UI
//! thread. All overlay writes go through a bounded channel with `try_send`:
//! dropping a UI frame beats blocking the latency path.

use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use sunoto_desktop::BubbleKind;
use sunoto_ipc::{OverlayRequest, OverlaySuggestion, SidecarClient};

use crate::events::DaemonEvent;
use crate::health::DaemonHealth;
use crate::insertion::UiCommand;
use crate::settings::Settings;

pub(crate) const ERROR_BUBBLE_VISIBLE: Duration = Duration::from_millis(2500);

/// Live overlay sidecar: requests go through a bounded channel serviced by a
/// writer thread, so a wedged overlay drops UI frames instead of ever
/// stalling the daemon loop. Dropping the handle ends the writer thread,
/// which drops the client and kills the process.
pub(crate) struct OverlayHandle {
    tx: mpsc::SyncSender<OverlayRequest>,
}

pub(crate) fn spawn_overlay(
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
pub(crate) struct UiFront {
    pub(crate) bubble: Sender<UiCommand>,
    pub(crate) overlay: Option<OverlayHandle>,
    pub(crate) overlay_ready: bool,
    /// Non-ready daemon health currently shown as the idle pill. While set,
    /// `hide()` returns to this state instead of clearing the screen, so a
    /// transient error bubble never erases "hotkey blocked".
    pub(crate) attention: Option<DaemonHealth>,
}

impl UiFront {
    pub(crate) fn overlay_active(&self) -> bool {
        self.overlay.is_some() && self.overlay_ready
    }

    /// Publish daemon health. `Ready` clears the idle pill; anything else
    /// shows it with a neutral dot and the health caption.
    pub(crate) fn health(&mut self, health: DaemonHealth) {
        self.attention = if health.is_ready() {
            None
        } else {
            Some(health)
        };
        self.send_state(health);
    }

    pub(crate) fn send_state(&self, health: DaemonHealth) {
        if self.overlay_active() {
            self.overlay_send(OverlayRequest::State {
                name: health.name().to_string(),
                detail: health.caption().to_string(),
            });
        } else if health.is_ready() {
            let _ = self.bubble.send(UiCommand::HideBubble);
        } else {
            let _ = self.bubble.send(UiCommand::ShowBubble(
                BubbleKind::Transcribing,
                health.caption().to_string(),
            ));
        }
    }

    pub(crate) fn overlay_send(&self, request: OverlayRequest) {
        if let Some(handle) = self.overlay.as_ref() {
            // try_send: dropping a UI frame beats blocking the event loop.
            let _ = handle.tx.try_send(request);
        }
    }

    /// A palette is an interaction boundary, not a cosmetic animation frame.
    /// Report whether it was queued so the daemon never retains executable
    /// actions when there is no UI capable of selecting them.
    pub(crate) fn show_system_palette(
        &self,
        session_id: u64,
        transcript: String,
        suggestions: Vec<OverlaySuggestion>,
    ) -> bool {
        if !self.overlay_active() {
            return false;
        }
        let Some(handle) = self.overlay.as_ref() else {
            return false;
        };
        handle
            .tx
            .try_send(OverlayRequest::SystemPalette {
                session_id,
                transcript,
                suggestions,
            })
            .is_ok()
    }

    pub(crate) fn dismiss_system_palette(&self, session_id: u64) {
        self.overlay_send(OverlayRequest::DismissSystemPalette { session_id });
    }

    pub(crate) fn show(&self, kind: BubbleKind, text: &str) {
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

    pub(crate) fn hide(&self) {
        if let Some(health) = self.attention {
            self.send_state(health);
            return;
        }
        self.overlay_send(OverlayRequest::Hide);
        let _ = self.bubble.send(UiCommand::HideBubble);
    }

    pub(crate) fn level(&self, elapsed_s: f64, peak: f64, rms: f64) {
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

pub(crate) fn show_error(ui: &UiFront, message: &str, bubble_hide_at: &mut Option<Instant>) {
    ui.show(BubbleKind::Error, message);
    *bubble_hide_at = Some(Instant::now() + ERROR_BUBBLE_VISIBLE);
}

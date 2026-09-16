//! Events that reach the daemon loop, and the control-socket command shapes
//! that turn into them. Everything else in the daemon is a producer of these.

use std::os::unix::net::UnixStream;

use serde::Deserialize;
use sunoto_audio::AudioEvent;
use sunoto_core::SessionMode;
use sunoto_desktop::HotkeyEvent;
use sunoto_ipc::SidecarMessage;

use crate::insertion::UiReport;
use crate::system_worker::SystemWorkerEvent;

pub enum DaemonEvent {
    Hotkey(ModeHotkeyEvent),
    Audio(AudioEvent),
    Sidecar(SidecarMessage),
    /// Messages from the GTK overlay UI sidecar (ready handshake, exit).
    Overlay(SidecarMessage),
    /// Results from blocking native discovery and execution work.
    System(SystemWorkerEvent),
    Ui(UiReport),
    /// WM_CLASS (instance, class) of the window focused at shortcut release;
    /// reported by the UI thread right after CaptureFocus.
    FocusClass(Option<(String, String)>),
    ControlPolish {
        text: String,
        response: UnixStream,
    },
    /// Read-only System planning request. Keeping this on the event loop makes
    /// the ownership boundary explicit before live System sessions are added.
    ControlSystemPlan {
        text: String,
        response: UnixStream,
    },
    Fatal(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ControlCommand {
    Polish {
        text: String,
    },
    PlanSystem {
        text: String,
        dry_run: bool,
    },
    Trigger {
        mode: ControlMode,
        edge: ControlEdge,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlMode {
    Dictation,
    System,
}

impl From<ControlMode> for SessionMode {
    fn from(mode: ControlMode) -> Self {
        match mode {
            ControlMode::Dictation => Self::Dictation,
            ControlMode::System => Self::System,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlEdge {
    Press,
    Release,
}

impl From<ControlEdge> for HotkeyEvent {
    fn from(edge: ControlEdge) -> Self {
        match edge {
            ControlEdge::Press => Self::Pressed,
            ControlEdge::Release => Self::Released,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ModeHotkeyEvent {
    pub(crate) mode: SessionMode,
    pub(crate) edge: HotkeyEvent,
}

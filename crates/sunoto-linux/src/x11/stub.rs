//! Compile-only stub for non-Linux targets.
//!
//! The real Linux X11 adapters live in `linux.rs`. This stub exposes the same
//! public API surface so the workspace links on non-Linux targets; every
//! operation returns an error or no-op. The daemon never routes through these
//! on macOS — it uses the `sunoto-desktop` facade, which re-exports
//! `sunoto-macos` there. This exists purely so `cargo build --workspace`
//! succeeds on any host.

use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleKind {
    Recording,
    Transcribing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionOutcome {
    Typed,
    Pasted,
    ClipboardOnly,
}

#[derive(Debug)]
pub enum X11Error {
    DisplayUnavailable,
    XTestUnavailable,
    HotkeyUnavailable(String),
    UnsupportedCharacter(char),
    ClipboardUnavailable,
    SelfTestMismatch { expected: String, actual: String },
    HotkeySelfTestMismatch { actual: Vec<HotkeyEvent> },
}

impl fmt::Display for X11Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DisplayUnavailable => write!(f, "cannot connect to the X11 display"),
            Self::XTestUnavailable => write!(f, "XTEST extension is unavailable"),
            Self::HotkeyUnavailable(shortcut) => {
                write!(f, "cannot resolve the {shortcut} hotkey")
            }
            Self::UnsupportedCharacter(character) => {
                write!(f, "X11 direct insertion does not support {character:?}")
            }
            Self::ClipboardUnavailable => write!(f, "cannot take ownership of the X11 clipboard"),
            Self::SelfTestMismatch { expected, actual } => {
                write!(
                    f,
                    "X11 insertion self-test expected {expected:?}, received {actual:?}"
                )
            }
            Self::HotkeySelfTestMismatch { actual } => {
                write!(
                    f,
                    "X11 push-to-talk self-test expected press/release, received {actual:?}"
                )
            }
        }
    }
}

impl std::error::Error for X11Error {}

#[derive(Debug, Clone)]
pub struct Shortcut {
    pub modifier_mask: u32,
    pub keysym_name: String,
}

impl Shortcut {
    /// Parse "Ctrl+F1" style descriptions (pure-Rust; mirrors the Linux
    /// parser so config validation works on any host).
    pub fn parse(description: &str) -> Result<Self, X11Error> {
        const CONTROL_MASK: u32 = 1 << 2;
        const SHIFT_MASK: u32 = 1;
        const MOD1_MASK: u32 = 1 << 3;
        const MOD4_MASK: u32 = 1 << 6;
        let mut modifier_mask = 0;
        let mut keysym_name = None;
        for part in description.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifier_mask |= CONTROL_MASK,
                "shift" => modifier_mask |= SHIFT_MASK,
                "alt" => modifier_mask |= MOD1_MASK,
                "super" => modifier_mask |= MOD4_MASK,
                "" => return Err(X11Error::HotkeyUnavailable(description.into())),
                _ => {
                    if keysym_name.replace(part.to_string()).is_some() {
                        return Err(X11Error::HotkeyUnavailable(description.into()));
                    }
                }
            }
        }
        match keysym_name {
            Some(keysym_name) if modifier_mask != 0 => Ok(Self {
                modifier_mask,
                keysym_name,
            }),
            _ => Err(X11Error::HotkeyUnavailable(description.into())),
        }
    }
}

impl Default for Shortcut {
    fn default() -> Self {
        Self {
            modifier_mask: 1 << 2, // CONTROL_MASK
            keysym_name: "F1".to_string(),
        }
    }
}

pub struct HotkeyListener;

impl Drop for HotkeyListener {
    fn drop(&mut self) {}
}

impl HotkeyListener {
    pub fn open(_shortcut: &Shortcut) -> Result<Self, X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn wait(&self, _timeout: Duration) -> Option<HotkeyEvent> {
        None
    }

    pub fn selftest_push_to_talk(&self) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }
}

pub struct ProbeWindow;

pub struct UiAdapter;

impl UiAdapter {
    pub fn open() -> Result<Self, X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn focused_window(&self) -> u64 {
        0
    }

    pub fn window_class(&self, _window: u64) -> Option<(String, String)> {
        None
    }

    pub fn insert_direct(&self, _text: &str) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn insert_via_clipboard(&mut self, _text: &str) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn set_clipboard(&mut self, _text: &str) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn read_clipboard(&mut self, _timeout: Duration) -> Option<String> {
        None
    }

    pub fn pump(&mut self) {}

    pub fn bubble_show(&mut self, _kind: BubbleKind, _text: &str) {}

    pub fn bubble_hide(&mut self) {}

    pub fn create_probe_window(&mut self) -> Result<ProbeWindow, X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn type_and_confirm(
        &mut self,
        _probe: &ProbeWindow,
        _text: &str,
    ) -> Result<Duration, X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn destroy_probe_window(&mut self, _probe: ProbeWindow) {}

    pub fn selftest_insert(&mut self, _text: &str) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn selftest_window_class(&mut self) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn selftest_clipboard(&mut self) -> Result<(), X11Error> {
        Err(X11Error::DisplayUnavailable)
    }
}

impl Drop for UiAdapter {
    fn drop(&mut self) {}
}

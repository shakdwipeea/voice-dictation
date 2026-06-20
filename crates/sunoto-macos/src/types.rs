//! Shared types for the macOS desktop adapters.
//!
//! Names mirror the `sunoto_linux::x11` surface so the daemon imports
//! compile unchanged through the `sunoto-desktop` facade. `X11Error` is kept
//! as the error type name for the same reason (a platform-agnostic rename is
//! a later cleanup).

use std::fmt;
use std::os::raw::c_uint;

use crate::ffi;

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
    InputMonitoringPermission,
    EventPostingPermission,
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
            Self::DisplayUnavailable => {
                write!(
                    f,
                    "cannot create the macOS event tap (Accessibility permission?)"
                )
            }
            Self::InputMonitoringPermission => write!(
                f,
                "macOS Input Monitoring permission is missing; grant it to the signed sunoto-daemon binary in System Settings > Privacy & Security > Input Monitoring"
            ),
            Self::EventPostingPermission => write!(
                f,
                "macOS Accessibility permission is missing; grant it to the signed sunoto-daemon binary in System Settings > Privacy & Security > Accessibility"
            ),
            Self::XTestUnavailable => write!(f, "CGEvent posting is unavailable"),
            Self::HotkeyUnavailable(shortcut) => {
                write!(f, "cannot resolve the {shortcut} hotkey")
            }
            Self::UnsupportedCharacter(character) => {
                write!(f, "macOS insertion does not support {character:?}")
            }
            Self::ClipboardUnavailable => {
                write!(f, "cannot use the macOS clipboard (pbcopy/pbpaste)")
            }
            Self::SelfTestMismatch { expected, actual } => {
                write!(
                    f,
                    "macOS insertion self-test expected {expected:?}, received {actual:?}"
                )
            }
            Self::HotkeySelfTestMismatch { actual } => {
                write!(
                    f,
                    "macOS push-to-talk self-test expected press/release, received {actual:?}"
                )
            }
        }
    }
}

impl std::error::Error for X11Error {}

/// A parsed push-to-talk shortcut. The `modifier_mask` stores CGEventFlag
/// bits; `key_name` is the final element (e.g. "F1"). `Cmd` is accepted in
/// addition to Ctrl/Shift/Alt/Super for macOS conventions.
#[derive(Debug, Clone)]
pub struct Shortcut {
    pub modifier_mask: u64,
    pub key_name: String,
}

impl Shortcut {
    /// Parse "Ctrl+F1" / "Cmd+F1" style descriptions. The final element is a
    /// key name; the rest are modifiers.
    pub fn parse(description: &str) -> Result<Self, X11Error> {
        let mut modifier_mask = 0u64;
        let mut key_name = None;
        for part in description.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifier_mask |= ffi::kCGEventFlagMaskControl,
                "shift" => modifier_mask |= ffi::kCGEventFlagMaskShift,
                "alt" | "option" => modifier_mask |= ffi::kCGEventFlagMaskAlternate,
                "cmd" | "super" | "command" => modifier_mask |= ffi::kCGEventFlagMaskCommand,
                "" => return Err(X11Error::HotkeyUnavailable(description.into())),
                _ => {
                    if key_name.replace(part.to_string()).is_some() {
                        return Err(X11Error::HotkeyUnavailable(description.into()));
                    }
                }
            }
        }
        match key_name {
            Some(key_name) if modifier_mask != 0 => Ok(Self {
                modifier_mask,
                key_name,
            }),
            _ => Err(X11Error::HotkeyUnavailable(description.into())),
        }
    }
}

impl Default for Shortcut {
    fn default() -> Self {
        Self {
            modifier_mask: ffi::kCGEventFlagMaskControl,
            key_name: "F1".to_string(),
        }
    }
}

// Re-export the raw mask constants for callers that build masks directly.
#[allow(dead_code)]
pub const _CONTROL_MASK: c_uint = 0;

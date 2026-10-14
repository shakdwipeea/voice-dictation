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
    /// The tap exists but macOS is not delivering events to it. Emitted once
    /// per transition by the delivery probe; see `hotkey_block_reason()`.
    Blocked,
    /// A probe event came back through the tap: real presses will too.
    Available,
    /// Esc was pressed, or another key was typed while a single-key shortcut
    /// was held (the user is typing, not dictating). Throws away a recording.
    Cancelled,
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
    /// The focused element is a password field; nothing was inserted and
    /// nothing was put on the clipboard.
    SecureField,
}

#[derive(Debug)]
pub enum X11Error {
    DisplayUnavailable,
    EventPostingPermission,
    XTestUnavailable,
    HotkeyUnavailable(String),
    /// The tap was created but the delivery probe never came back.
    HotkeyBlocked(String),
    UnsupportedCharacter(char),
    ClipboardUnavailable,
    SelfTestMismatch {
        expected: String,
        actual: String,
    },
    HotkeySelfTestMismatch {
        actual: Vec<HotkeyEvent>,
    },
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
            Self::EventPostingPermission => write!(
                f,
                "macOS Accessibility permission is missing; grant it to the signed sunoto-daemon binary in System Settings > Privacy & Security > Accessibility"
            ),
            Self::XTestUnavailable => write!(f, "CGEvent posting is unavailable"),
            Self::HotkeyUnavailable(shortcut) => {
                write!(f, "cannot resolve the {shortcut} hotkey")
            }
            Self::HotkeyBlocked(reason) => {
                write!(f, "hotkey events are not being delivered: {reason}")
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

/// A single modifier key used on its own as the shortcut, such as Right
/// Option. macOS reports it only through `flagsChanged` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoneModifier {
    /// Virtual key code of the physical key.
    pub key_code: u16,
    /// Device-dependent flag bit that is set while this key is down.
    pub device_mask: u64,
    /// Device-independent flag the key sets (Option, Command, ...).
    pub flag_mask: u64,
}

impl LoneModifier {
    /// "RightOption", "Right Option", "right-option" and so on. Left-hand
    /// modifiers are not offered: they are part of too many shortcuts.
    fn named(name: &str) -> Option<(&'static str, Self)> {
        let squashed: String = name
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '_'))
            .collect::<String>()
            .to_ascii_lowercase();
        let (canonical, key_code, device_mask, flag_mask) = match squashed.as_str() {
            "rightoption" | "rightalt" => (
                "RightOption",
                0x3d,
                ffi::NX_DEVICERALTKEYMASK,
                ffi::kCGEventFlagMaskAlternate,
            ),
            "rightcommand" | "rightcmd" => (
                "RightCommand",
                0x36,
                ffi::NX_DEVICERCMDKEYMASK,
                ffi::kCGEventFlagMaskCommand,
            ),
            "rightcontrol" | "rightctrl" => (
                "RightControl",
                0x3e,
                ffi::NX_DEVICERCTLKEYMASK,
                ffi::kCGEventFlagMaskControl,
            ),
            "rightshift" => (
                "RightShift",
                0x3c,
                ffi::NX_DEVICERSHIFTKEYMASK,
                ffi::kCGEventFlagMaskShift,
            ),
            _ => return None,
        };
        Some((
            canonical,
            Self {
                key_code,
                device_mask,
                flag_mask,
            },
        ))
    }
}

/// A parsed push-to-talk shortcut. The `modifier_mask` stores CGEventFlag
/// bits; `key_name` is the final element (e.g. "F1"). `Cmd` is accepted in
/// addition to Ctrl/Shift/Alt/Super for macOS conventions. A lone right-hand
/// modifier ("RightOption") is also accepted and sets `lone_modifier`.
#[derive(Debug, Clone)]
pub struct Shortcut {
    pub modifier_mask: u64,
    pub key_name: String,
    pub lone_modifier: Option<LoneModifier>,
}

impl Shortcut {
    /// Parse "Ctrl+F1" / "Cmd+F1" style descriptions, or a single right-hand
    /// modifier such as "RightOption". For chords the final element is a key
    /// name; the rest are modifiers.
    pub fn parse(description: &str) -> Result<Self, X11Error> {
        if let Some((canonical, lone)) = LoneModifier::named(description.trim()) {
            return Ok(Self {
                modifier_mask: 0,
                key_name: canonical.to_string(),
                lone_modifier: Some(lone),
            });
        }
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
            // "Ctrl+RightOption" is not a chord macOS can deliver.
            Some(key_name) if LoneModifier::named(&key_name).is_some() => {
                Err(X11Error::HotkeyUnavailable(description.into()))
            }
            Some(key_name) if modifier_mask != 0 => Ok(Self {
                modifier_mask,
                key_name,
                lone_modifier: None,
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
            lone_modifier: None,
        }
    }
}

// Re-export the raw mask constants for callers that build masks directly.
#[allow(dead_code)]
pub const _CONTROL_MASK: c_uint = 0;

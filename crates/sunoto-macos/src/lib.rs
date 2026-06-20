//! macOS desktop adapters (CGEventTap hotkey, CGEvent insertion, CoreGraphics
//! focus, pbcopy clipboard). See `docs/macos-port-plan.md` and the
//! `macos-port` skill.
//!
//! On macOS this crate implements the platform surface the daemon reaches
//! through the `sunoto-desktop` facade. On other targets it is an empty
//! placeholder (the facade re-exports `sunoto-linux` there).

#![cfg(target_os = "macos")]

mod ffi;
mod hotkey;
mod insertion;
mod types;

pub use hotkey::HotkeyListener;
pub use insertion::{ProbeWindow, UiAdapter};
pub use types::{BubbleKind, HotkeyEvent, InsertionOutcome, Shortcut, X11Error};

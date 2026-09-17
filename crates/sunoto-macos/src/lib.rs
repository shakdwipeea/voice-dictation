//! macOS desktop adapters (CGEventTap hotkey, CGEvent insertion, CoreGraphics
//! focus, pbcopy clipboard). See `docs/macos-port-plan.md` and the
//! `macos-port` skill.
//!
//! On macOS this crate implements the platform surface the daemon reaches
//! through the `sunoto-desktop` facade. On other targets it is an empty
//! placeholder (the facade re-exports `sunoto-linux` there).

#![cfg(target_os = "macos")]

mod accessibility;
mod appkit;
mod ffi;
mod hotkey;
mod insertion;
mod system;
mod types;

pub use appkit::{PasteboardSnapshot, keep_process_responsive};
pub use hotkey::{HotkeyListener, PROBE_TIMEOUT, hotkey_block_reason, permission_preflights};
pub use insertion::{ProbeWindow, UiAdapter};
pub use system::SystemPlatform;
pub use types::{BubbleKind, HotkeyEvent, InsertionOutcome, Shortcut, X11Error};

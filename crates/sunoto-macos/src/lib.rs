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
mod permissions;
mod system;
mod types;

pub use accessibility::{FocusSnapshot, SnapshotOptions, app_snapshot, focus_snapshot};
pub use appkit::{EnglishVocabulary, PasteboardSnapshot, contact_names, keep_process_responsive};
pub use hotkey::{HotkeyListener, PROBE_TIMEOUT, hotkey_block_reason};
pub use insertion::{ProbeWindow, UiAdapter};
pub use permissions::{accessibility_granted, microphone_authorized, request_accessibility};
pub use system::SystemPlatform;
pub use types::{BubbleKind, HotkeyEvent, InsertionOutcome, Shortcut, X11Error};

//! Desktop-integration facade for the daemon.
//!
//! The daemon imports all platform adapters (`HotkeyListener`, `Shortcut`,
//! `UiAdapter`, `HotkeyEvent`, `BubbleKind`, `InsertionOutcome`, `X11Error`)
//! from here so it stays platform-agnostic at the call sites.
//!
//! - On Linux: re-exports the real X11 adapters from `sunoto_linux::x11`.
//! - On macOS: re-exports the real CGEventTap / CGEvent / CoreGraphics
//!   adapters from `sunoto_macos`.
//! - On other targets: re-exports `sunoto_linux::x11`, which is a compile
//!   stub there (so the workspace links for cross-compilation hygiene).

#[cfg(target_os = "macos")]
pub use sunoto_macos::*;

#[cfg(not(target_os = "macos"))]
pub use sunoto_linux::x11::*;

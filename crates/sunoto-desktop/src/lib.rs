//! Desktop-integration facade for the daemon.
//!
//! The daemon imports all platform adapters (`HotkeyListener`, `Shortcut`,
//! `UiAdapter`, `HotkeyEvent`, `BubbleKind`, `InsertionOutcome`,
//! `DesktopError`) from here so it stays platform-agnostic at the call sites.
//!
//! - On Linux: re-exports the real X11 adapters from `sunoto_linux::x11`.
//! - On macOS: re-exports the real CGEventTap / CGEvent / CoreGraphics
//!   adapters from `sunoto_macos`.
//! - On other targets: re-exports `sunoto_linux::x11`, which is a compile
//!   stub there (so the workspace links for cross-compilation hygiene).
//! - The Wayland adapter is subprocess glue and is re-exported everywhere.
//!
//! The re-export is by name, so the two platform crates must agree on the
//! surface. [`DesktopAdapter`] and [`HotkeySource`] write that agreement
//! down: each platform's types implement them below, and a platform that
//! drops or renames a method fails to compile here instead of in the daemon.

use std::time::Duration;

#[cfg(target_os = "macos")]
pub use sunoto_macos::*;

#[cfg(not(target_os = "macos"))]
pub use sunoto_linux::{system::*, x11::*};

pub use sunoto_linux::wayland::{WaylandFocus, WaylandOutcome, WaylandUiAdapter};

/// Platform-neutral name for the adapter error type. The concrete type is
/// still called `X11Error` inside both platform crates for historical
/// reasons; new code should spell it `DesktopError`.
pub type DesktopError = X11Error;

/// The focus, insertion, clipboard, and fallback-bubble surface every
/// platform adapter provides.
pub trait DesktopAdapter: Sized {
    fn open() -> Result<Self, DesktopError>;
    fn focused_window(&self) -> u64;
    fn window_class(&self, window: u64) -> Option<(String, String)>;
    /// `Some(true)` when keyboard focus is in a password field; `None` when
    /// the platform cannot tell.
    fn focused_is_secure_field(&self) -> Option<bool> {
        None
    }
    fn insert_direct(&self, text: &str) -> Result<(), DesktopError>;
    fn insert_via_clipboard(&mut self, text: &str) -> Result<(), DesktopError>;
    fn set_clipboard(&mut self, text: &str) -> Result<(), DesktopError>;
    fn read_clipboard(&mut self, timeout: Duration) -> Option<String>;
    fn pump(&mut self);
    fn bubble_show(&mut self, kind: BubbleKind, text: &str);
    fn bubble_hide(&mut self);
}

/// The global push-to-talk listener surface.
pub trait HotkeySource: Sized {
    fn open(shortcut: &Shortcut) -> Result<Self, DesktopError>;
    fn wait(&self, timeout: Duration) -> Option<HotkeyEvent>;
    /// Prove events reach the listener (macOS delivery probe); trivially
    /// `Ok` where a successful `open` already proves it.
    fn verify_delivery(&self, timeout: Duration) -> Result<(), DesktopError>;
    fn selftest_push_to_talk(&self) -> Result<(), DesktopError>;
}

impl DesktopAdapter for UiAdapter {
    fn open() -> Result<Self, DesktopError> {
        UiAdapter::open()
    }
    fn focused_window(&self) -> u64 {
        UiAdapter::focused_window(self)
    }
    fn window_class(&self, window: u64) -> Option<(String, String)> {
        UiAdapter::window_class(self, window)
    }
    #[cfg(target_os = "macos")]
    fn focused_is_secure_field(&self) -> Option<bool> {
        UiAdapter::focused_is_secure_field(self)
    }
    fn insert_direct(&self, text: &str) -> Result<(), DesktopError> {
        UiAdapter::insert_direct(self, text)
    }
    fn insert_via_clipboard(&mut self, text: &str) -> Result<(), DesktopError> {
        UiAdapter::insert_via_clipboard(self, text)
    }
    fn set_clipboard(&mut self, text: &str) -> Result<(), DesktopError> {
        UiAdapter::set_clipboard(self, text)
    }
    fn read_clipboard(&mut self, timeout: Duration) -> Option<String> {
        UiAdapter::read_clipboard(self, timeout)
    }
    fn pump(&mut self) {
        UiAdapter::pump(self)
    }
    fn bubble_show(&mut self, kind: BubbleKind, text: &str) {
        UiAdapter::bubble_show(self, kind, text)
    }
    fn bubble_hide(&mut self) {
        UiAdapter::bubble_hide(self)
    }
}

impl HotkeySource for HotkeyListener {
    fn open(shortcut: &Shortcut) -> Result<Self, DesktopError> {
        HotkeyListener::open(shortcut)
    }
    fn wait(&self, timeout: Duration) -> Option<HotkeyEvent> {
        HotkeyListener::wait(self, timeout)
    }
    fn verify_delivery(&self, timeout: Duration) -> Result<(), DesktopError> {
        HotkeyListener::verify_delivery(self, timeout)
    }
    fn selftest_push_to_talk(&self) -> Result<(), DesktopError> {
        HotkeyListener::selftest_push_to_talk(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_adapter<T: DesktopAdapter>() {}
    fn assert_hotkey<T: HotkeySource>() {}

    #[test]
    fn platform_types_satisfy_the_contracts() {
        assert_adapter::<UiAdapter>();
        assert_hotkey::<HotkeyListener>();
    }

    #[test]
    fn outcome_and_event_enums_have_the_shared_variants() {
        let _ = [
            InsertionOutcome::Typed,
            InsertionOutcome::Pasted,
            InsertionOutcome::ClipboardOnly,
            InsertionOutcome::SecureField,
        ];
        let _ = [
            HotkeyEvent::Pressed,
            HotkeyEvent::Released,
            HotkeyEvent::Blocked,
            HotkeyEvent::Available,
        ];
        let _ = [
            BubbleKind::Recording,
            BubbleKind::Transcribing,
            BubbleKind::Error,
        ];
    }
}

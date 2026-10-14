//! Main-thread macOS TCC permission requests used by onboarding.
//!
//! Sunoto needs two permissions: Accessibility and Microphone. Accessibility
//! covers both posting events (paste, typing) and listening to them (the
//! shortcut's event tap). Apple DTS: "'Accessibility' basically grants event
//! posting and listening, while 'Input Monitoring' only grants listening."
//! Input Monitoring used to be a separate onboarding step; its request API
//! (`CGRequestListenEventAccess`) does not reliably list the app in the pane,
//! and a running process cannot see its own new grant, so it was dropped.
//!
//! The Accessibility prompt is a system modal dialog that macOS shows **at
//! most once per record state**: a dismissed or stale record means the
//! request is silently ignored. Firing requests from background threads at
//! startup raced the system, so the request happens only after the user
//! clicks the onboarding row, on the daemon's main loop.
//!
//! `HotkeyListener::open` and `UiAdapter::open` only *preflight*; they never
//! request. One-shot CLI commands (`check`, `selftest`, `insert`) therefore
//! report permission state instead of prompting under the terminal's own
//! TCC identity, which used to add a confusing second entry to the panes.

use crate::accessibility;
use crate::ffi;

/// Whether Accessibility is granted. Read through `AXIsProcessTrusted`, which
/// reflects a toggle in System Settings without a restart; the daemon polls
/// this while blocked so it can relaunch itself the moment a grant appears.
pub fn accessibility_granted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() || ffi::CGPreflightPostEventAccess() }
}

/// Request Accessibility after its onboarding button is clicked. Besides the
/// system dialog, this adds Sunoto to the corresponding Privacy & Security
/// pane. Must be called on the daemon's main thread.
pub fn request_accessibility() -> bool {
    if unsafe { ffi::AXIsProcessTrusted() } {
        return true;
    }
    accessibility::request_accessibility_with_prompt()
}

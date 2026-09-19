//! Main-thread macOS TCC permission requests used by onboarding.
//!
//! Both Input Monitoring and Accessibility prompts are system modal dialogs
//! that macOS shows **at most once per record state** — a dismissed or stale
//! record means the request is silently ignored. Firing them from several
//! background threads at startup (the old behavior) raced the system:
//! prompts stacked, were coalesced, or never appeared. Requests now happen
//! only after the user clicks the matching onboarding row. The daemon handles
//! that event on its main loop, so prompts attach to the app and only one
//! service is in flight at a time.
//!
//! `HotkeyListener::open` and `UiAdapter::open` only *preflight*; they never
//! request. One-shot CLI commands (`check`, `selftest`, `insert`) therefore
//! report permission state instead of prompting under the terminal's own
//! TCC identity, which used to add a confusing second entry to the panes.

use crate::accessibility;
use crate::ffi;

/// Current TCC answers for (Input Monitoring, Accessibility). Accessibility
/// is read through `AXIsProcessTrusted`, which reflects a toggle in System
/// Settings without a restart; the daemon polls this while blocked so it
/// can relaunch itself the moment a grant appears.
pub fn permission_preflights() -> (bool, bool) {
    unsafe {
        (
            ffi::CGPreflightListenEventAccess(),
            ffi::AXIsProcessTrusted() || ffi::CGPreflightPostEventAccess(),
        )
    }
}

/// Request Input Monitoring after its onboarding button is clicked.
/// Must be called on the daemon's main thread.
pub fn request_input_monitoring() -> bool {
    if unsafe { ffi::CGPreflightListenEventAccess() } {
        return true;
    }
    unsafe { ffi::CGRequestListenEventAccess() }
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

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

use std::os::raw::{c_char, c_void};

use crate::accessibility;
use crate::ffi;

#[link(name = "AVFoundation", kind = "framework")]
#[link(name = "objc")]
unsafe extern "C" {
    static AVMediaTypeAudio: *mut c_void;
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// `AVAuthorizationStatusAuthorized`.
const AV_AUTHORIZED: isize = 3;

/// Whether Microphone access is already granted. Reads the decision only:
/// it never prompts and never opens the device, so the daemon can keep the
/// microphone off until a dictation starts.
pub fn microphone_authorized() -> bool {
    // SAFETY: `+[AVCaptureDevice authorizationStatusForMediaType:]` takes an
    // NSString and returns an NSInteger; class, selector and constant are
    // checked for null before the call.
    unsafe {
        let class = objc_getClass(c"AVCaptureDevice".as_ptr());
        let selector = sel_registerName(c"authorizationStatusForMediaType:".as_ptr());
        if class.is_null() || selector.is_null() || AVMediaTypeAudio.is_null() {
            return false;
        }
        let status: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> isize =
            std::mem::transmute(objc_msgSend as *const ());
        status(class, selector, AVMediaTypeAudio) == AV_AUTHORIZED
    }
}

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

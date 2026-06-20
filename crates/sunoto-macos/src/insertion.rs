//! Text insertion, clipboard, and focus/app identity for macOS (Phase 4).
//!
//! - Insertion: `CGEventCreateKeyboardEvent` + `CGEventKeyboardSetUnicodeString`
//!   per character, posted at the HID event tap. Newlines become Return only
//!   when `allow_enter_and_tab` (the daemon's `sanitize_for_insertion` already
//!   neutralizes control chars before we see them).
//! - Clipboard: `pbcopy`/`pbpaste` subprocesses (no AppKit FFI).
//! - Focus / app identity: `CGWindowListCopyWindowInfo` (CoreGraphics C API)
//!   — the frontmost on-screen, layer-0 window's owner name. On recent macOS
//!   owner names may be redacted without Screen Recording permission; we
//!   degrade to `None` and the daemon's app-aware style simply doesn't apply.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_long};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::ffi;
use crate::types::{BubbleKind, X11Error};

/// CGEvent post location: post at the HID event tap so events enter the
/// normal event stream and reach the focused application.
const POST_TAP: c_int = 0; // kCGHIDEventTap

pub struct UiAdapter {
    source: ffi::CGEventSourceRef,
}

impl UiAdapter {
    pub fn open() -> Result<Self, X11Error> {
        if !unsafe { ffi::CGPreflightPostEventAccess() } {
            unsafe {
                let _ = ffi::CGRequestPostEventAccess();
            }
        }
        // SAFETY: CombinedSessionState source is the standard shared source;
        // null return means the event system is unavailable.
        let source =
            unsafe { ffi::CGEventSourceCreate(ffi::kCGEventSourceStateCombinedSessionState) };
        if source.is_null() {
            return Err(X11Error::DisplayUnavailable);
        }
        Ok(Self { source })
    }

    pub fn focused_window(&self) -> u64 {
        u64::from(frontmost_window_number().unwrap_or(0))
    }

    pub fn window_class(&self, window: u64) -> Option<(String, String)> {
        if window == 0 {
            return None;
        }
        let name = owner_name_for_window(window as u32)?;
        // Return (instance, class) mirroring X11 WM_CLASS; macOS only gives an
        // owner name, so use it for both fields.
        Some((name.clone(), name))
    }

    pub fn insert_direct(&self, text: &str) -> Result<(), X11Error> {
        // SAFETY: source is valid for the adapter's lifetime; each event is
        // created, posted, and released here.
        unsafe {
            for ch in text.chars() {
                if ch == '\n' {
                    post_keyevent(self.source, 0x24, true); // Return
                    post_keyevent(self.source, 0x24, false);
                    continue;
                }
                if ch == '\t' {
                    post_keyevent(self.source, 0x30, true); // Tab
                    post_keyevent(self.source, 0x30, false);
                    continue;
                }
                post_unicode_char(self.source, ch)?;
            }
        }
        Ok(())
    }

    pub fn insert_via_clipboard(&mut self, text: &str) -> Result<(), X11Error> {
        self.set_clipboard(text)?;
        paste_frontmost()
    }

    pub fn set_clipboard(&mut self, text: &str) -> Result<(), X11Error> {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| X11Error::ClipboardUnavailable)?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        drop(child.stdin.take());
        let status = child.wait().map_err(|_| X11Error::ClipboardUnavailable)?;
        if status.success() {
            Ok(())
        } else {
            Err(X11Error::ClipboardUnavailable)
        }
    }

    pub fn read_clipboard(&mut self, _timeout: Duration) -> Option<String> {
        let output = Command::new("pbpaste")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    pub fn pump(&mut self) {
        // No X11-style event queue to pump on macOS; insertion is synchronous.
    }

    pub fn bubble_show(&mut self, _kind: BubbleKind, _text: &str) {
        // The native overlay (Phase 6) lives in a sidecar process; the
        // daemon routes visuals through the overlay sidecar, not here. This
        // is a no-op so the daemon's fallback path compiles.
    }

    pub fn bubble_hide(&mut self) {}

    pub fn create_probe_window(&mut self) -> Result<ProbeWindow, X11Error> {
        // The X11 probe window is used by `bench` to echo typed text back. On
        // macOS, `bench` is not meaningful without a real target; return a
        // stub so the binary compiles. `type_and_confirm` reports unsupported.
        Ok(ProbeWindow { window: 0 })
    }

    pub fn type_and_confirm(
        &mut self,
        _probe: &ProbeWindow,
        _text: &str,
    ) -> Result<Duration, X11Error> {
        Err(X11Error::DisplayUnavailable)
    }

    pub fn destroy_probe_window(&mut self, _probe: ProbeWindow) {}

    pub fn selftest_insert(&mut self, text: &str) -> Result<(), X11Error> {
        // Insert into whatever is focused (caller focuses a disposable target).
        self.insert_direct(text)
    }

    pub fn selftest_window_class(&mut self) -> Result<(), X11Error> {
        match self.window_class(self.focused_window()) {
            Some(_) => Ok(()),
            None => Err(X11Error::SelfTestMismatch {
                expected: "frontmost app name".to_string(),
                actual: "(none / permission denied)".to_string(),
            }),
        }
    }

    pub fn selftest_clipboard(&mut self) -> Result<(), X11Error> {
        let probe = "sunoto macos clipboard probe";
        self.set_clipboard(probe)?;
        let got = self.read_clipboard(Duration::from_secs(1));
        match got {
            Some(text) if text == probe => Ok(()),
            other => Err(X11Error::SelfTestMismatch {
                expected: probe.to_string(),
                actual: other.unwrap_or_default(),
            }),
        }
    }
}

impl Drop for UiAdapter {
    fn drop(&mut self) {
        // SAFETY: release the CGEventSource; no further use after drop.
        unsafe { ffi::CFRelease(self.source) };
    }
}

pub struct ProbeWindow {
    #[allow(dead_code)]
    window: u32,
}

// ----- helpers ----------------------------------------------------------------

/// SAFETY: caller guarantees `source` is a valid CGEventSourceRef.
unsafe fn post_keyevent(source: ffi::CGEventSourceRef, keycode: u16, key_down: bool) {
    unsafe {
        let event = ffi::CGEventCreateKeyboardEvent(source, keycode, key_down as c_int);
        if event.is_null() {
            return;
        }
        ffi::CGEventPost(POST_TAP, event);
        ffi::CFRelease(event);
    }
}

/// SAFETY: caller guarantees `source` is a valid CGEventSourceRef.
unsafe fn post_unicode_char(source: ffi::CGEventSourceRef, ch: char) -> Result<(), X11Error> {
    // Encode the character as UTF-16 (one or two units) and attach it to a
    // synthetic keyDown/keyUp pair. The virtual keycode is a dummy (0 = A);
    // the unicode string overrides the produced character.
    let mut units: [u16; 2] = [0; 2];
    let count = ch.encode_utf16(&mut units).len();
    unsafe {
        let event = ffi::CGEventCreateKeyboardEvent(source, 0, 1);
        if event.is_null() {
            return Err(X11Error::DisplayUnavailable);
        }
        // Clear modifiers so the char isn't chorded with a held shortcut key.
        ffi::CGEventSetFlags(event, 0);
        let mut actual: c_long = 0;
        ffi::CGEventKeyboardSetUnicodeString(
            event,
            count as c_long,
            &mut actual,
            units.as_mut_ptr(),
        );
        ffi::CGEventPost(POST_TAP, event);
        ffi::CFRelease(event);

        // Key up with the same unicode string (some apps need the up event).
        let up = ffi::CGEventCreateKeyboardEvent(source, 0, 0);
        if !up.is_null() {
            ffi::CGEventSetFlags(up, 0);
            let mut actual2: c_long = 0;
            ffi::CGEventKeyboardSetUnicodeString(
                up,
                count as c_long,
                &mut actual2,
                units.as_mut_ptr(),
            );
            ffi::CGEventPost(POST_TAP, up);
            ffi::CFRelease(up);
        }
    }
    Ok(())
}

/// Trigger a paste (Cmd+V) in the frontmost application via CGEvent.
fn paste_frontmost() -> Result<(), X11Error> {
    // SAFETY: a transient source per paste is simplest; release immediately.
    unsafe {
        let source = ffi::CGEventSourceCreate(ffi::kCGEventSourceStateCombinedSessionState);
        if source.is_null() {
            return Err(X11Error::DisplayUnavailable);
        }
        let cmd = ffi::kCGEventFlagMaskCommand;
        // key down V
        let down = ffi::CGEventCreateKeyboardEvent(source, 0x09, 1); // virtual key 9 = V
        if !down.is_null() {
            ffi::CGEventSetFlags(down, cmd);
            ffi::CGEventPost(POST_TAP, down);
            ffi::CFRelease(down);
        }
        let up = ffi::CGEventCreateKeyboardEvent(source, 0x09, 0);
        if !up.is_null() {
            ffi::CGEventSetFlags(up, cmd);
            ffi::CGEventPost(POST_TAP, up);
            ffi::CFRelease(up);
        }
        ffi::CFRelease(source);
    }
    Ok(())
}

/// Owner names that own layer-0 windows but are not user applications —
/// the desktop menubar, Dock, control-center, etc. The frontmost pick must
/// skip these, otherwise dictation targets "Window Server" instead of the
/// actual focused app (TextEdit, Terminal, ...).
const SYSTEM_OWNERS: &[&str] = &["Window Server", "Dock", "SystemUIServer"];

/// Frontmost on-screen, layer-0 window number (z-order: first in the list)
/// owned by a real user application. System-owned layer-0 windows (menubar,
/// Dock) sort above app windows and must be skipped, or focus resolves to
/// "Window Server" and typed text goes nowhere.
fn frontmost_window_number() -> Option<u32> {
    // SAFETY: CGWindowListCopyWindowInfo returns a CFArray we must release.
    let array = unsafe {
        ffi::CGWindowListCopyWindowInfo(ffi::kCGWindowListOptionOnScreenOnly, ffi::kCGNullWindowID)
    };
    if array.is_null() {
        return None;
    }
    let count = unsafe { ffi::CFArrayGetCount(array) };
    let mut result = None;
    for i in 0..count {
        let dict = unsafe { ffi::CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }
        let layer = number_value(dict, ffi::kCGWindowLayer).unwrap_or(0);
        if layer != 0 {
            continue; // skip menus / dock / overlays
        }
        // Skip system-owned layer-0 windows (menubar, Dock, control center).
        let owner = string_value(dict, ffi::kCGWindowOwnerName).unwrap_or_default();
        if SYSTEM_OWNERS.contains(&owner.as_str()) {
            continue;
        }
        if let Some(num) = number_value(dict, ffi::kCGWindowNumber) {
            result = Some(num as u32);
            break;
        }
    }
    unsafe { ffi::CFRelease(array) };
    result
}

/// Owner (application) name for a given window number, by scanning the
/// on-screen window list. Returns the first window whose number matches.
fn owner_name_for_window(window: u32) -> Option<String> {
    let array = unsafe {
        ffi::CGWindowListCopyWindowInfo(ffi::kCGWindowListOptionOnScreenOnly, ffi::kCGNullWindowID)
    };
    if array.is_null() {
        return None;
    }
    let count = unsafe { ffi::CFArrayGetCount(array) };
    let mut result = None;
    for i in 0..count {
        let dict = unsafe { ffi::CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }
        let num = number_value(dict, ffi::kCGWindowNumber).unwrap_or(0) as u32;
        if num != window {
            continue;
        }
        if let Some(name) = string_value(dict, ffi::kCGWindowOwnerName) {
            result = Some(name);
            break;
        }
    }
    unsafe { ffi::CFRelease(array) };
    result
}

/// Read a CFNumber-typed value from a window-info dictionary as i64.
fn number_value(dict: *const std::os::raw::c_void, key: &str) -> Option<i64> {
    let key_cf = cf_string(key)?;
    // SAFETY: dict is a CFDictionary; key_cf is a CFString we release below.
    let value = unsafe { ffi::CFDictionaryGetValue(dict, key_cf as *const _) };
    unsafe { ffi::CFRelease(key_cf) };
    if value.is_null() {
        return None;
    }
    // Try SInt64 then SInt32.
    let mut out64: i64 = 0;
    // SAFETY: CFNumberGetValue writes into out64 if the type is compatible;
    // we probe 64 then 32.
    if unsafe {
        ffi::CFNumberGetValue(
            value as ffi::CFNumberRef,
            ffi::kCFNumberSInt64Type,
            &mut out64 as *mut i64 as *mut std::os::raw::c_void,
        )
    } == 1
    {
        return Some(out64);
    }
    let mut out32: i32 = 0;
    if unsafe {
        ffi::CFNumberGetValue(
            value as ffi::CFNumberRef,
            ffi::kCFNumberSInt32Type,
            &mut out32 as *mut i32 as *mut std::os::raw::c_void,
        )
    } == 1
    {
        return Some(i64::from(out32));
    }
    None
}

/// Read a CFString-typed value from a window-info dictionary as a Rust String.
fn string_value(dict: *const std::os::raw::c_void, key: &str) -> Option<String> {
    let key_cf = cf_string(key)?;
    let value = unsafe { ffi::CFDictionaryGetValue(dict, key_cf as *const _) };
    unsafe { ffi::CFRelease(key_cf) };
    if value.is_null() {
        return None;
    }
    cf_string_to_rust(value as ffi::CFStringRef)
}

/// Create a CFString from a Rust &str (caller must CFRelease the result).
fn cf_string(s: &str) -> Option<ffi::CFStringRef> {
    // SAFETY: bytes are valid for the call; the returned CFString must be
    // released by the caller.
    let cf = unsafe {
        ffi::CFStringCreateWithBytes(
            ffi::NULL_ALLOCATOR,
            s.as_ptr(),
            s.len() as c_long,
            ffi::kCFStringEncodingUTF8,
            0,
        )
    };
    if cf.is_null() { None } else { Some(cf) }
}

/// Copy a CFString into a Rust String.
fn cf_string_to_rust(cf: ffi::CFStringRef) -> Option<String> {
    // SAFETY: try the direct pointer first (common for constant strings), then
    // fall back to a buffer copy.
    unsafe {
        let ptr = ffi::CFStringGetCStringPtr(cf, ffi::kCFStringEncodingUTF8);
        if !ptr.is_null() {
            return std::ffi::CStr::from_ptr(ptr)
                .to_str()
                .ok()
                .map(str::to_string);
        }
        let len = ffi::CFStringGetLength(cf);
        // Worst case: 4 bytes per UTF-16 code unit when transcoded to UTF-8.
        let mut buf = vec![0u8; (len as usize + 1) * 4];
        if ffi::CFStringGetCString(
            cf,
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as c_long,
            ffi::kCFStringEncodingUTF8,
        ) == 1
        {
            let s = std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char);
            s.to_str().ok().map(str::to_string)
        } else {
            None
        }
    }
}

// Re-export for callers that need the CString drop guard pattern.
#[allow(dead_code)]
fn _link_cstring(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

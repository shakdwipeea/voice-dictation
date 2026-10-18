//! Accessibility queries about the focused element.
//!
//! Only the C HIServices API is used, so this needs no Objective-C. Every
//! query returns `None` when Accessibility permission is missing or the app
//! exposes nothing; callers must treat `None` as "unknown", never as "safe".

use std::os::raw::c_long;

use crate::ffi;

const SECURE_ROLE: &str = "AXSecureTextField";

/// Ask macOS for Accessibility access with the system prompt. Besides the
/// dialog, this lists the app in the Accessibility pane with its switch
/// off, so the user flips a switch instead of adding the app by hand.
/// Returns whether access is already granted.
pub fn request_accessibility_with_prompt() -> bool {
    // SAFETY: builds a one-entry CFDictionary {kAXTrustedCheckOptionPrompt:
    // true}, passes it to the documented API, and releases it.
    unsafe {
        let key = cf_string("AXTrustedCheckOptionPrompt");
        let Some(key) = key else {
            return ffi::AXIsProcessTrusted();
        };
        let keys = [key as *const std::os::raw::c_void];
        let values = [ffi::kCFBooleanTrue as *const std::os::raw::c_void];
        let options = ffi::CFDictionaryCreate(
            ffi::NULL_ALLOCATOR,
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &ffi::kCFTypeDictionaryKeyCallBacks,
            &ffi::kCFTypeDictionaryValueCallBacks,
        );
        ffi::CFRelease(key);
        if options.is_null() {
            return ffi::AXIsProcessTrusted();
        }
        let trusted = ffi::AXIsProcessTrustedWithOptions(options);
        ffi::CFRelease(options);
        trusted
    }
}

/// Whether the element that currently has keyboard focus is a secure
/// (password) text field. Checks both the role and the subrole: AppKit
/// reports the role, Chromium-based apps report a plain text field with the
/// secure subrole.
pub fn focused_element_is_secure() -> Option<bool> {
    if !unsafe { ffi::AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: every CF object created here is released before returning;
    // attribute copies are owned by us per the Copy rule.
    unsafe {
        let system_wide = ffi::AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return None;
        }
        let focused = copy_attribute(system_wide, "AXFocusedUIElement");
        ffi::CFRelease(system_wide);
        let focused = focused?;
        let role = copy_string_attribute(focused, "AXRole");
        let subrole = copy_string_attribute(focused, "AXSubrole");
        ffi::CFRelease(focused);
        if role.is_none() && subrole.is_none() {
            return None;
        }
        Some(role.as_deref() == Some(SECURE_ROLE) || subrole.as_deref() == Some(SECURE_ROLE))
    }
}

/// What the focused window shows, for the word list (see the daemon's
/// `vocabulary` module). Every field is `None` when the app does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusSnapshot {
    pub window_title: Option<String>,
    /// `AXDocument` of the focused window: a file URL for document apps and
    /// for Ghostty (its working folder), the page URL for browsers.
    pub document: Option<String>,
    /// Text of the focused element (a text box, a terminal's screen), at
    /// most `max_chars` of it around the cursor. Never read from a password
    /// field.
    pub text: Option<String>,
    /// Text shown elsewhere in the window (a chat's messages, the email
    /// being answered), read only when the focused element has little and
    /// `SnapshotOptions::walk_window` asks for it.
    pub window_text: Option<String>,
}

/// How much `focus_snapshot` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Most characters kept from the focused element, and from the window.
    pub max_chars: usize,
    /// Read the window's visible text when the focused element holds less
    /// than WALK_BELOW_CHARS (an empty chat box, a new email).
    pub walk_window: bool,
    /// Ask an app that shows no focused element to turn its accessibility
    /// on (`AXManualAccessibility`, which Electron apps such as Slack and
    /// VS Code need); it shares its text from the next dictation on.
    pub wake_electron: bool,
}

/// The focused element is "short" below this many characters.
const WALK_BELOW_CHARS: usize = 300;
/// A window walk stops after this many elements or this long.
const WALK_MAX_ELEMENTS: usize = 1_500;
const WALK_BUDGET: std::time::Duration = std::time::Duration::from_millis(150);

/// Seconds an app gets to answer each Accessibility question. A busy app
/// would otherwise hold the reading thread for up to six seconds.
const SNAPSHOT_TIMEOUT_S: f32 = 0.05;

/// Read the focused window's title, document and text. `None` without
/// Accessibility permission or without a focused app.
pub fn focus_snapshot(options: SnapshotOptions) -> Option<FocusSnapshot> {
    if !unsafe { ffi::AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: every CF object created or copied here is released before
    // returning; attribute copies are owned by us per the Copy rule.
    unsafe {
        let system_wide = ffi::AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return None;
        }
        ffi::AXUIElementSetMessagingTimeout(system_wide, SNAPSHOT_TIMEOUT_S);
        let app = copy_attribute(system_wide, "AXFocusedApplication");
        ffi::CFRelease(system_wide);
        // Some apps answer for themselves but not through the system-wide
        // element (it reports "cannot complete"); ask the front app directly.
        let app = app.or_else(|| {
            let pid = crate::appkit::frontmost_pid()?;
            let app = ffi::AXUIElementCreateApplication(pid);
            (!app.is_null()).then_some(app)
        })?;
        Some(snapshot_of(app, options))
    }
}

/// `focus_snapshot` for the app with process id `pid`, focused or not (for
/// `sunoto-daemon words --pid`).
pub fn app_snapshot(pid: i32, options: SnapshotOptions) -> Option<FocusSnapshot> {
    if !unsafe { ffi::AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: the created elements are released here and by snapshot_of.
    unsafe {
        // The system-wide element sets the time limit for every element.
        let system_wide = ffi::AXUIElementCreateSystemWide();
        if !system_wide.is_null() {
            ffi::AXUIElementSetMessagingTimeout(system_wide, SNAPSHOT_TIMEOUT_S);
            ffi::CFRelease(system_wide);
        }
        let app = ffi::AXUIElementCreateApplication(pid);
        (!app.is_null()).then(|| snapshot_of(app, options))
    }
}

/// SAFETY: `app` must be a live AXUIElementRef owned by the caller; it is
/// released here.
unsafe fn snapshot_of(app: ffi::AXUIElementRef, options: SnapshotOptions) -> FocusSnapshot {
    unsafe {
        ffi::AXUIElementSetMessagingTimeout(app, SNAPSHOT_TIMEOUT_S);
        let mut snapshot = FocusSnapshot::default();
        let window = copy_attribute(app, "AXFocusedWindow");
        if let Some(window) = window {
            snapshot.window_title = copy_text_attribute(window, "AXTitle");
            snapshot.document = copy_text_attribute(window, "AXDocument");
        }
        let mut secure = false;
        let mut focused_found = false;
        if let Some(focused) = copy_attribute(app, "AXFocusedUIElement") {
            focused_found = true;
            let role = copy_string_attribute(focused, "AXRole");
            let subrole = copy_string_attribute(focused, "AXSubrole");
            secure = role.as_deref() == Some(SECURE_ROLE) || subrole.as_deref() == Some(SECURE_ROLE);
            if !secure {
                snapshot.text = copy_text_near_cursor(focused, options.max_chars);
            }
            ffi::CFRelease(focused);
        }
        let short = snapshot.text.as_deref().map_or(0, |text| text.chars().count()) < WALK_BELOW_CHARS;
        if let Some(window) = window {
            if options.walk_window && short && !secure {
                snapshot.window_text = window_text(window, options.max_chars);
            }
            ffi::CFRelease(window);
        }
        // After reading: the app rebuilds its tree once it is woken, and a
        // walk during that rebuild finds nothing. Once per app is enough.
        if options.wake_electron
            && !focused_found
            && first_wake(app)
            && let Some(key) = cf_string("AXManualAccessibility")
        {
            ffi::AXUIElementSetAttributeValue(app, key, ffi::kCFBooleanTrue);
            ffi::CFRelease(key);
        }
        ffi::CFRelease(app);
        snapshot
    }
}

/// Whether this is the first time `app` would be woken in this process.
///
/// SAFETY: `app` must be a live AXUIElementRef.
unsafe fn first_wake(app: ffi::AXUIElementRef) -> bool {
    static WOKEN: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());
    let mut pid = 0;
    if unsafe { ffi::AXUIElementGetPid(app, &mut pid) } != ffi::kAXErrorSuccess {
        return false;
    }
    let mut woken = WOKEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if woken.contains(&pid) {
        return false;
    }
    if woken.len() > 512 {
        woken.clear();
    }
    woken.push(pid);
    true
}

/// The visible text of `window`: values, titles and descriptions of its
/// elements, depth first, within WALK_MAX_ELEMENTS, WALK_BUDGET and
/// `max_chars`. Password fields are skipped.
///
/// SAFETY: `window` must be a live AXUIElementRef; it is not released.
unsafe fn window_text(window: ffi::AXUIElementRef, max_chars: usize) -> Option<String> {
    let started = std::time::Instant::now();
    let mut text = String::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![unsafe { ffi::CFRetain(window) }];
    let mut visited = 0;
    while let Some(element) = stack.pop() {
        visited += 1;
        let done = visited > WALK_MAX_ELEMENTS
            || started.elapsed() > WALK_BUDGET
            || text.len() >= max_chars;
        if !done {
            let role = unsafe { copy_string_attribute(element, "AXRole") };
            if role.as_deref() != Some(SECURE_ROLE) {
                for name in ["AXValue", "AXTitle", "AXDescription"] {
                    if let Some(value) = unsafe { copy_text_attribute(element, name) }
                        && value.len() < 4_000
                        && seen.insert(value.clone())
                    {
                        text.push_str(&value);
                        text.push('\n');
                    }
                }
                if let Some(children) = unsafe { copy_attribute(element, "AXChildren") } {
                    let count = unsafe { ffi::CFArrayGetCount(children as ffi::CFArrayRef) };
                    for index in (0..count).rev() {
                        let child = unsafe { ffi::CFArrayGetValueAtIndex(children as ffi::CFArrayRef, index) };
                        if !child.is_null() {
                            stack.push(unsafe { ffi::CFRetain(child) });
                        }
                    }
                    unsafe { ffi::CFRelease(children) };
                }
            }
        }
        unsafe { ffi::CFRelease(element) };
    }
    (!text.is_empty()).then_some(text)
}

/// SAFETY: `element` must be a live AXUIElementRef.
unsafe fn copy_text_attribute(element: ffi::AXUIElementRef, name: &str) -> Option<String> {
    let value = unsafe { copy_attribute(element, name) }?;
    let text = if unsafe { ffi::CFGetTypeID(value) == ffi::CFStringGetTypeID() } {
        cf_string_to_rust(value as ffi::CFStringRef)
    } else {
        None
    };
    unsafe { ffi::CFRelease(value) };
    text.filter(|text| !text.is_empty())
}

/// The element's AXValue, cut to `max_chars` around the selection when it is
/// longer (a long document), from the start when there is no selection.
///
/// SAFETY: `element` must be a live AXUIElementRef.
unsafe fn copy_text_near_cursor(element: ffi::AXUIElementRef, max_chars: usize) -> Option<String> {
    let value = unsafe { copy_attribute(element, "AXValue") }?;
    if unsafe { ffi::CFGetTypeID(value) != ffi::CFStringGetTypeID() } {
        unsafe { ffi::CFRelease(value) };
        return None;
    }
    let length = unsafe { ffi::CFStringGetLength(value as ffi::CFStringRef) };
    let max = max_chars as ffi::CFIndex;
    if length <= max {
        let text = cf_string_to_rust(value as ffi::CFStringRef);
        unsafe { ffi::CFRelease(value) };
        return text.filter(|text| !text.is_empty());
    }
    let mut cursor = 0;
    if let Some(range) = unsafe { copy_attribute(element, "AXSelectedTextRange") } {
        let mut selected = ffi::CFRange::default();
        if unsafe {
            ffi::AXValueGetValue(
                range,
                ffi::kAXValueCFRangeType,
                &mut selected as *mut ffi::CFRange as *mut std::os::raw::c_void,
            )
        } {
            cursor = selected.location.clamp(0, length);
        }
        unsafe { ffi::CFRelease(range) };
    }
    let start = (cursor - max / 2).clamp(0, length - max);
    let part = unsafe {
        ffi::CFStringCreateWithSubstring(
            ffi::NULL_ALLOCATOR,
            value as ffi::CFStringRef,
            ffi::CFRange { location: start, length: max },
        )
    };
    unsafe { ffi::CFRelease(value) };
    if part.is_null() {
        return None;
    }
    let text = cf_string_to_rust(part);
    unsafe { ffi::CFRelease(part) };
    text
}

/// SAFETY: `element` must be a live AXUIElementRef. The returned object is
/// owned by the caller.
unsafe fn copy_attribute(element: ffi::AXUIElementRef, name: &str) -> Option<ffi::CFTypeRef> {
    let key = cf_string(name)?;
    let mut value: ffi::CFTypeRef = std::ptr::null();
    let status = unsafe { ffi::AXUIElementCopyAttributeValue(element, key, &mut value) };
    unsafe { ffi::CFRelease(key) };
    if status != ffi::kAXErrorSuccess || value.is_null() {
        return None;
    }
    Some(value)
}

/// SAFETY: as `copy_attribute`; the copied value is released here.
unsafe fn copy_string_attribute(element: ffi::AXUIElementRef, name: &str) -> Option<String> {
    let value = unsafe { copy_attribute(element, name) }?;
    let text = cf_string_to_rust(value as ffi::CFStringRef);
    unsafe { ffi::CFRelease(value) };
    text
}

fn cf_string(s: &str) -> Option<ffi::CFStringRef> {
    // SAFETY: bytes are valid for the call; the caller releases the result.
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

fn cf_string_to_rust(cf: ffi::CFStringRef) -> Option<String> {
    // SAFETY: the fast path borrows CoreFoundation's buffer only for the
    // copy; the slow path writes into a buffer sized for the worst case.
    unsafe {
        let ptr = ffi::CFStringGetCStringPtr(cf, ffi::kCFStringEncodingUTF8);
        if !ptr.is_null() {
            return std::ffi::CStr::from_ptr(ptr)
                .to_str()
                .ok()
                .map(str::to_string);
        }
        let len = ffi::CFStringGetLength(cf);
        let mut buf = vec![0u8; (len as usize + 1) * 4];
        if ffi::CFStringGetCString(
            cf,
            buf.as_mut_ptr() as *mut std::os::raw::c_char,
            buf.len() as c_long,
            ffi::kCFStringEncodingUTF8,
        ) == 1
        {
            std::ffi::CStr::from_ptr(buf.as_ptr() as *const std::os::raw::c_char)
                .to_str()
                .ok()
                .map(str::to_string)
        } else {
            None
        }
    }
}

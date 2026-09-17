//! The few AppKit calls the insertion path needs, through the raw
//! Objective-C runtime: the general pasteboard (snapshot, transient write,
//! restore) and the frontmost application's identity.
//!
//! Why AppKit here when the rest of the adapter is C-only: `pbcopy` and
//! `pbpaste` round-trip plain text only, so restoring the user's clipboard
//! after a paste would lose images, files, and rich text. `NSPasteboard`
//! gives every item's every type as bytes, which is what a faithful restore
//! needs. Every call is wrapped in an autorelease pool and returns `Option`
//! or `bool`; nothing here panics on a missing class or selector.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

#[link(name = "AppKit", kind = "framework")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// Marks a pasteboard item as transient so clipboard managers (Maccy,
/// Paste, Raycast, Alfred) do not record dictated text.
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";
const PLAIN_TEXT_TYPE: &str = "public.utf8-plain-text";
/// Above this many bytes the snapshot is skipped rather than copied: the
/// user is pasting something huge and a 300 ms round trip of it is worse
/// than losing it.
const SNAPSHOT_BYTE_CAP: usize = 64 * 1024 * 1024;

/// Every item on the general pasteboard with every type it offered, plus the
/// change count observed when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteboardSnapshot {
    items: Vec<Vec<(String, Vec<u8>)>>,
}

impl PasteboardSnapshot {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item_count(&self) -> usize {
        self.items.len()
    }
}

/// Copy the current pasteboard contents. `None` when AppKit is unavailable
/// or the contents exceed the cap.
pub fn snapshot() -> Option<PasteboardSnapshot> {
    let pool = Pool::new()?;
    let pasteboard = general_pasteboard()?;
    // SAFETY: documented NSPasteboard / NSPasteboardItem / NSArray / NSData
    // selectors with matching ABI; all returned objects are autoreleased
    // and live until `pool` drains.
    let items = unsafe {
        let array = send_id(pasteboard, sel("pasteboardItems")?);
        if array.is_null() {
            return Some(PasteboardSnapshot { items: Vec::new() });
        }
        let count = send_usize(array, sel("count")?);
        let mut total = 0usize;
        let mut items = Vec::with_capacity(count);
        for index in 0..count {
            let item = send_id_usize(array, sel("objectAtIndex:")?, index);
            if item.is_null() {
                continue;
            }
            let types = send_id(item, sel("types")?);
            if types.is_null() {
                continue;
            }
            let type_count = send_usize(types, sel("count")?);
            let mut entries = Vec::with_capacity(type_count);
            for type_index in 0..type_count {
                let type_string = send_id_usize(types, sel("objectAtIndex:")?, type_index);
                let Some(type_name) = ns_string_to_rust(type_string) else {
                    continue;
                };
                let data = send_id_id(item, sel("dataForType:")?, type_string);
                if data.is_null() {
                    continue;
                }
                let length = send_usize(data, sel("length")?);
                total += length;
                if total > SNAPSHOT_BYTE_CAP {
                    return None;
                }
                let bytes = send_id(data, sel("bytes")?) as *const u8;
                let payload = if bytes.is_null() || length == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(bytes, length).to_vec()
                };
                entries.push((type_name, payload));
            }
            items.push(entries);
        }
        items
    };
    drop(pool);
    Some(PasteboardSnapshot { items })
}

/// Replace the pasteboard with `text` marked transient. Returns the change
/// count after the write, which `restore` uses to detect a later user copy.
pub fn write_transient_text(text: &str) -> Option<isize> {
    let pool = Pool::new()?;
    let pasteboard = general_pasteboard()?;
    let text_ns = ns_string(text)?;
    let plain = ns_string(PLAIN_TEXT_TYPE)?;
    let transient = ns_string(TRANSIENT_TYPE)?;
    // SAFETY: documented NSPasteboard selectors; `setString:forType:` and
    // `setData:forType:` both target the first item, so the transient marker
    // lands on the same item as the text.
    let change_count = unsafe {
        send_usize(pasteboard, sel("clearContents")?);
        if !send_bool_id_id(pasteboard, sel("setString:forType:")?, text_ns, plain) {
            return None;
        }
        let empty = send_id(objc_class("NSData")?, sel("data")?);
        if !empty.is_null() {
            send_bool_id_id(pasteboard, sel("setData:forType:")?, empty, transient);
        }
        send_usize(pasteboard, sel("changeCount")?) as isize
    };
    drop(pool);
    Some(change_count)
}

/// Put `snapshot` back, but only if the pasteboard still holds what we wrote
/// (`expected_change_count`). Returns false when someone else has written
/// since, in which case their content is left alone.
pub fn restore(snapshot: &PasteboardSnapshot, expected_change_count: isize) -> bool {
    let Some(pool) = Pool::new() else {
        return false;
    };
    let Some(pasteboard) = general_pasteboard() else {
        return false;
    };
    let result = (|| -> Option<bool> {
        // SAFETY: documented NSPasteboard / NSPasteboardItem / NSMutableArray /
        // NSData selectors; items are created with alloc/init and released
        // after the array retains them.
        unsafe {
            let current = send_usize(pasteboard, sel("changeCount")?) as isize;
            if current != expected_change_count {
                return Some(false);
            }
            send_usize(pasteboard, sel("clearContents")?);
            if snapshot.items.is_empty() {
                return Some(true);
            }
            let array = send_id(objc_class("NSMutableArray")?, sel("array")?);
            if array.is_null() {
                return Some(false);
            }
            for entries in &snapshot.items {
                let item = send_id(
                    send_id(objc_class("NSPasteboardItem")?, sel("alloc")?),
                    sel("init")?,
                );
                if item.is_null() {
                    continue;
                }
                for (type_name, bytes) in entries {
                    let Some(type_ns) = ns_string(type_name) else {
                        continue;
                    };
                    let data = send_id_ptr_usize(
                        objc_class("NSData")?,
                        sel("dataWithBytes:length:")?,
                        bytes.as_ptr() as *const c_void,
                        bytes.len(),
                    );
                    if data.is_null() {
                        continue;
                    }
                    send_bool_id_id(item, sel("setData:forType:")?, data, type_ns);
                }
                send_void_id(array, sel("addObject:")?, item);
                send_void(item, sel("release")?);
            }
            Some(send_bool_id(pasteboard, sel("writeObjects:")?, array))
        }
    })();
    drop(pool);
    result.unwrap_or(false)
}

/// `(bundle identifier, localized name)` of the frontmost application, from
/// `NSWorkspace`. Needs no Screen Recording permission, unlike window owner
/// names from `CGWindowList`.
pub fn frontmost_application() -> Option<(String, String)> {
    let pool = Pool::new()?;
    // SAFETY: documented NSWorkspace / NSRunningApplication selectors.
    let result = unsafe {
        let workspace = send_id(objc_class("NSWorkspace")?, sel("sharedWorkspace")?);
        if workspace.is_null() {
            return None;
        }
        let app = send_id(workspace, sel("frontmostApplication")?);
        if app.is_null() {
            return None;
        }
        let bundle = ns_string_to_rust(send_id(app, sel("bundleIdentifier")?));
        let name = ns_string_to_rust(send_id(app, sel("localizedName")?));
        match (bundle, name) {
            (Some(bundle), Some(name)) => Some((bundle, name)),
            (None, Some(name)) => Some((name.clone(), name)),
            (Some(bundle), None) => Some((bundle.clone(), bundle)),
            (None, None) => None,
        }
    };
    drop(pool);
    result
}

/// Opt the process out of App Nap for its whole lifetime. Without this a
/// background app with no window and no audio running gets throttled: the
/// event tap is disabled "by timeout", the delivery probe expires, and the
/// microphone reopens slowly. The activity token is retained forever.
pub fn keep_process_responsive(reason: &str) -> bool {
    let Some(pool) = Pool::new() else {
        return false;
    };
    // NSActivityUserInitiatedAllowingIdleSystemSleep | NSActivityLatencyCritical
    const OPTIONS: u64 = 0x00EF_FFFF | 0xFF_0000_0000;
    let Some(reason_ns) = ns_string(reason) else {
        return false;
    };
    // SAFETY: documented NSProcessInfo selector; the returned token is
    // retained and intentionally leaked so the activity never ends.
    let ok = unsafe {
        let info = send_id(
            objc_class("NSProcessInfo").unwrap_or(std::ptr::null_mut()),
            sel("processInfo").unwrap_or(std::ptr::null_mut()),
        );
        if info.is_null() {
            false
        } else {
            let token = send_id_u64_id(
                info,
                sel("beginActivityWithOptions:reason:").unwrap_or(std::ptr::null_mut()),
                OPTIONS,
                reason_ns,
            );
            if token.is_null() {
                false
            } else {
                send_void(token, sel("retain").unwrap_or(std::ptr::null_mut()));
                true
            }
        }
    };
    drop(pool);
    ok
}

// ----- runtime plumbing --------------------------------------------------------

struct Pool(*mut c_void);

impl Pool {
    fn new() -> Option<Self> {
        // SAFETY: NSAutoreleasePool alloc/init is the documented way to scope
        // autoreleased objects on a thread without a run loop.
        let pool = unsafe {
            send_id(
                send_id(objc_class("NSAutoreleasePool")?, sel("alloc")?),
                sel("init")?,
            )
        };
        if pool.is_null() {
            None
        } else {
            Some(Self(pool))
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: `drain` releases the pool and everything autoreleased into it.
        if let Some(drain) = sel("drain") {
            unsafe { send_void(self.0, drain) };
        }
    }
}

fn general_pasteboard() -> Option<*mut c_void> {
    // SAFETY: class method returning the shared pasteboard, or null.
    let pasteboard = unsafe { send_id(objc_class("NSPasteboard")?, sel("generalPasteboard")?) };
    if pasteboard.is_null() {
        None
    } else {
        Some(pasteboard)
    }
}

fn objc_class(name: &str) -> Option<*mut c_void> {
    let name = CString::new(name).ok()?;
    // SAFETY: reads a terminated class name; null when the class is absent.
    let class = unsafe { objc_getClass(name.as_ptr()) };
    if class.is_null() { None } else { Some(class) }
}

fn sel(name: &str) -> Option<*mut c_void> {
    let name = CString::new(name).ok()?;
    // SAFETY: interns a terminated selector name.
    let selector = unsafe { sel_registerName(name.as_ptr()) };
    if selector.is_null() {
        None
    } else {
        Some(selector)
    }
}

fn ns_string(text: &str) -> Option<*mut c_void> {
    let c = CString::new(text).ok()?;
    // SAFETY: `stringWithUTF8String:` copies the bytes; the result is
    // autoreleased into the caller's pool.
    let ns = unsafe {
        send_id_cstr(
            objc_class("NSString")?,
            sel("stringWithUTF8String:")?,
            c.as_ptr(),
        )
    };
    if ns.is_null() { None } else { Some(ns) }
}

/// SAFETY: `ns` must be an NSString or null.
unsafe fn ns_string_to_rust(ns: *mut c_void) -> Option<String> {
    if ns.is_null() {
        return None;
    }
    let ptr = unsafe { send_cstr(ns, sel("UTF8String")?) };
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(str::to_string)
}

unsafe fn send_id(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_id_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *mut c_void,
) -> *mut c_void {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_id_usize(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: usize,
) -> *mut c_void {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_id_cstr(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *const c_char,
) -> *mut c_void {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_id_u64_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    options: u64,
    argument: *mut c_void,
) -> *mut c_void {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, u64, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, options, argument) }
}

unsafe fn send_id_ptr_usize(
    receiver: *mut c_void,
    selector: *mut c_void,
    pointer: *const c_void,
    length: usize,
) -> *mut c_void {
    let function: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *const c_void,
        usize,
    ) -> *mut c_void = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, pointer, length) }
}

unsafe fn send_bool_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *mut c_void,
) -> bool {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_bool_id_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    first: *mut c_void,
    second: *mut c_void,
) -> bool {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> bool =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, first, second) }
}

unsafe fn send_usize(receiver: *mut c_void, selector: *mut c_void) -> usize {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_cstr(receiver: *mut c_void, selector: *mut c_void) -> *const c_char {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *const c_char =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_void(receiver: *mut c_void, selector: *mut c_void) {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_void_id(receiver: *mut c_void, selector: *mut c_void, argument: *mut c_void) {
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips the real general pasteboard: writes a transient text,
    /// verifies the change count moves, restores the prior contents, and
    /// verifies they came back. Ignored by default because it touches the
    /// user's clipboard; run with `--ignored` on a Mac.
    #[test]
    #[ignore = "touches the real clipboard"]
    fn live_snapshot_write_restore_round_trip() {
        let before = snapshot().expect("snapshot");
        let count = write_transient_text("sunoto pasteboard probe").expect("write");
        let during = snapshot().expect("snapshot during");
        assert_eq!(during.item_count(), 1);
        assert!(during.items[0].iter().any(|(t, _)| t == TRANSIENT_TYPE));
        assert!(restore(&before, count));
        let after = snapshot().expect("snapshot after");
        assert_eq!(after, before);
    }
}

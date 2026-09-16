//! Accessibility queries about the focused element.
//!
//! Only the C HIServices API is used, so this needs no Objective-C. Every
//! query returns `None` when Accessibility permission is missing or the app
//! exposes nothing; callers must treat `None` as "unknown", never as "safe".

use std::os::raw::c_long;

use crate::ffi;

const SECURE_ROLE: &str = "AXSecureTextField";

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

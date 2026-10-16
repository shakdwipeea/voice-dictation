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

/// Select `text` in the focused text element, but only when it is exactly
/// what sits immediately before the caret (no selection, same characters).
/// This is how Improve replaces what Sunoto just pasted without touching
/// anything else: select it, then paste over the selection.
///
/// `Some(true)`: selected, and the selection read back as asked.
/// `Some(false)`: the text before the caret is different, there is a
/// selection, or the app ignored the request.
/// `None`: Accessibility gave no answer (no permission, no focused text).
pub fn select_text_before_caret(text: &str) -> Option<bool> {
    if text.is_empty() || !unsafe { ffi::AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: every CF object created or copied here is released before
    // returning.
    unsafe {
        let system_wide = ffi::AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return None;
        }
        let focused = copy_attribute(system_wide, "AXFocusedUIElement");
        ffi::CFRelease(system_wide);
        let focused = focused?;
        let selected = select_in(focused, text);
        ffi::CFRelease(focused);
        selected
    }
}

/// SAFETY: `element` must be a live AXUIElementRef.
unsafe fn select_in(element: ffi::AXUIElementRef, text: &str) -> Option<bool> {
    let caret = unsafe { selected_range(element) }?;
    let length = text.encode_utf16().count() as ffi::CFIndex;
    if caret.length != 0 || caret.location < length {
        return Some(false);
    }
    let wanted = ffi::CFRange {
        location: caret.location - length,
        length,
    };
    let before_caret = unsafe { string_for_range(element, wanted) }
        .or_else(|| unsafe { value_slice(element, wanted) });
    if before_caret.as_deref() != Some(text) {
        return Some(false);
    }
    let key = cf_string("AXSelectedTextRange")?;
    let value = unsafe {
        ffi::AXValueCreate(
            ffi::kAXValueCFRangeType,
            &wanted as *const ffi::CFRange as *const std::os::raw::c_void,
        )
    };
    if value.is_null() {
        unsafe { ffi::CFRelease(key) };
        return None;
    }
    let status = unsafe { ffi::AXUIElementSetAttributeValue(element, key, value) };
    unsafe {
        ffi::CFRelease(value);
        ffi::CFRelease(key);
    }
    if status != ffi::kAXErrorSuccess {
        return Some(false);
    }
    Some(unsafe { selected_range(element) } == Some(wanted))
}

/// The element's selection (or caret, when its length is 0), in UTF-16
/// units. SAFETY: `element` must be a live AXUIElementRef.
unsafe fn selected_range(element: ffi::AXUIElementRef) -> Option<ffi::CFRange> {
    let value = unsafe { copy_attribute(element, "AXSelectedTextRange") }?;
    let mut range = ffi::CFRange::default();
    let ok = unsafe {
        ffi::AXValueGetValue(
            value,
            ffi::kAXValueCFRangeType,
            &mut range as *mut ffi::CFRange as *mut std::os::raw::c_void,
        )
    };
    unsafe { ffi::CFRelease(value) };
    ok.then_some(range)
}

/// Just the characters in `range`, without copying a long document.
/// SAFETY: `element` must be a live AXUIElementRef.
unsafe fn string_for_range(element: ffi::AXUIElementRef, range: ffi::CFRange) -> Option<String> {
    let key = cf_string("AXStringForRange")?;
    let parameter = unsafe {
        ffi::AXValueCreate(
            ffi::kAXValueCFRangeType,
            &range as *const ffi::CFRange as *const std::os::raw::c_void,
        )
    };
    if parameter.is_null() {
        unsafe { ffi::CFRelease(key) };
        return None;
    }
    let mut result: ffi::CFTypeRef = std::ptr::null();
    let status = unsafe {
        ffi::AXUIElementCopyParameterizedAttributeValue(element, key, parameter, &mut result)
    };
    unsafe {
        ffi::CFRelease(parameter);
        ffi::CFRelease(key);
    }
    if status != ffi::kAXErrorSuccess || result.is_null() {
        return None;
    }
    let text = cf_string_to_rust(result as ffi::CFStringRef);
    unsafe { ffi::CFRelease(result) };
    text
}

/// Fallback for apps without AXStringForRange: slice the whole value.
/// SAFETY: `element` must be a live AXUIElementRef.
unsafe fn value_slice(element: ffi::AXUIElementRef, range: ffi::CFRange) -> Option<String> {
    let value = unsafe { copy_string_attribute(element, "AXValue") }?;
    utf16_slice(&value, range)
}

fn utf16_slice(text: &str, range: ffi::CFRange) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = usize::try_from(range.location).ok()?;
    let end = start.checked_add(usize::try_from(range.length).ok()?)?;
    String::from_utf16(units.get(start..end)?).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slices_follow_accessibility_ranges() {
        let range = |location, length| ffi::CFRange { location, length };
        assert_eq!(
            utf16_slice("Hello world", range(6, 5)).as_deref(),
            Some("world")
        );
        // An emoji is two UTF-16 units, as Accessibility counts them.
        assert_eq!(
            utf16_slice("ok 👍 done", range(3, 2)).as_deref(),
            Some("👍")
        );
        assert_eq!(
            utf16_slice("ok 👍 done", range(6, 4)).as_deref(),
            Some("done")
        );
        assert_eq!(utf16_slice("short", range(2, 9)), None);
        assert_eq!(utf16_slice("short", range(-1, 2)), None);
    }
}

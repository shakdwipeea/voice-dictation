//! Raw CoreGraphics / CoreFoundation FFI for the macOS adapters.
//!
//! Mirrors the hand-written FFI style of `crates/sunoto-linux/src/x11/ffi.rs`:
//! explicit extern declarations, no bindgen, no extra dependencies. Only the C
//! APIs we need are bound; Objective-C (AppKit/Foundation) is deliberately
//! avoided — the clipboard uses `pbcopy`/`pbpaste`, and focus/app identity uses
//! the CoreGraphics `CGWindowList` C API, so no objc_msgSend is required.

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]
#![allow(clippy::duplicated_attributes)]

use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

#[link(name = "CoreGraphics", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {}

// ----- CoreFoundation types ---------------------------------------------------

pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *const c_void;
pub type CFRunLoopMode = CFStringRef;
pub type CFStringEncoding = u32;
pub type CFIndex = c_long;

pub const kCFStringEncodingUTF8: CFStringEncoding = 0x08000100;

pub const NULL_ALLOCATOR: CFAllocatorRef = std::ptr::null();

#[repr(transparent)]
pub struct CFStringConstant(pub CFStringRef);
unsafe impl Sync for CFStringConstant {}

// ----- CoreGraphics event types ----------------------------------------------

pub type CGEventRef = *const c_void;
pub type CGEventTapRef = *const c_void;
pub type CGEventSourceRef = *const c_void;
pub type CGEventTapProxy = *const c_void;
pub type CGEventType = u32;
pub type CGKeyCode = u16;
pub type CGMouseButton = c_uint;

// CGEventTapLocation
pub const kCGHIDEventTap: c_int = 0;
pub const kCGSessionEventTap: c_int = 1;
// CGEventTapPlacement
pub const kCGHeadInsertEventTap: c_int = 0;
pub const kCGTailAppendEventTap: c_int = 1;
// CGEventTapOptions
pub const kCGEventTapOptionListenOnly: c_int = 1;
pub const kCGEventTapOptionDefault: c_int = 0;

// CGEventFlags (modifier bitmasks)
pub const kCGEventFlagMaskShift: u64 = 1 << 17;
pub const kCGEventFlagMaskControl: u64 = 1 << 18;
pub const kCGEventFlagMaskAlternate: u64 = 1 << 19;
pub const kCGEventFlagMaskCommand: u64 = 1 << 20;

// CGEventTypes
pub const kCGEventNull: CGEventType = 0;
pub const kCGEventKeyDown: CGEventType = 10;
pub const kCGEventKeyUp: CGEventType = 11;
pub const kCGEventFlagsChanged: CGEventType = 12;

// CGEventTapDisableTimeout: event tap was disabled by the system (timeout).
pub const kCGSessionEventTapTimeout: CGEventType = 0xfffffffe;

// CGEventField for the unicode string payload.
pub const kCGKeyboardEventKeycode: u32 = 9;

// ----- CGWindowList (frontmost window / app identity) ------------------------

pub const kCGNullWindowID: u32 = 0;
// CGWindowListOption bits
pub const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
pub const kCGWindowListOptionAll: u32 = 1 << 4;

// CGWindowKeys (CFString constants in the per-window dictionaries)
pub const kCGWindowOwnerName: &str = "kCGWindowOwnerName";
pub const kCGWindowOwnerPID: &str = "kCGWindowOwnerPID";
pub const kCGWindowNumber: &str = "kCGWindowNumber";
pub const kCGWindowLayer: &str = "kCGWindowLayer";

// ----- function bindings ------------------------------------------------------

unsafe extern "C" {
    // CoreGraphics: events
    pub fn CGEventTapCreate(
        location: c_int,
        placement: c_int,
        options: c_int,
        eventMask: u64,
        callback: CGEventTapCallBack,
        userInfo: *const c_void,
    ) -> CGEventTapRef;
    pub fn CGEventTapEnable(tap: CGEventTapRef, enable: c_int);
    pub fn CGEventTapIsEnabled(tap: CGEventTapRef) -> c_int;
    pub fn CGEventSourceCreate(sourceStateID: c_int) -> CGEventSourceRef;
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keyCode: CGKeyCode,
        keyDown: c_int,
    ) -> CGEventRef;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        maxStringLength: c_long,
        actualStringLength: *mut c_long,
        unicodeString: *mut u16,
    );
    pub fn CGEventPost(tap: c_int, event: CGEventRef);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> c_long;
    pub fn CGEventSourceSecondsSinceLastEventType(
        source: CGEventSourceRef,
        eventType: CGEventType,
    ) -> f64;
    pub fn CGPreflightListenEventAccess() -> bool;
    pub fn CGRequestListenEventAccess() -> bool;
    pub fn CGPreflightPostEventAccess() -> bool;
    pub fn CGRequestPostEventAccess() -> bool;

    // CoreGraphics: window list
    pub fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
    pub fn CGWindowListCreate(option: u32, relativeToWindow: u32) -> CFArrayRef;
    pub fn CGWindowNumberFromCGPoint(point: CGPoint) -> u32;

    // CoreGraphics: event source state constants are integers (pass directly)
    // kCGEventSourceStateHIDSystemState = 1, kCGEventSourceStateCombinedSessionState = 0

    // CoreFoundation
    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        numBytes: CFIndex,
        encoding: CFStringEncoding,
        isExternalRepresentation: c_int,
    ) -> CFStringRef;
    pub fn CFStringGetCStringPtr(
        theString: CFStringRef,
        encoding: CFStringEncoding,
    ) -> *const c_char;
    pub fn CFStringGetLength(theString: CFStringRef) -> CFIndex;
    pub fn CFStringGetCString(
        theString: CFStringRef,
        buffer: *mut c_char,
        bufferSize: CFIndex,
        encoding: CFStringEncoding,
    ) -> c_int;
    pub fn CFRelease(cf: *const c_void);
    pub fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> *const c_void;
    pub fn CFDictionaryGetValue(theDict: *const c_void, key: *const c_void) -> *const c_void;
    pub fn CFNumberGetValue(
        number: CFNumberRef,
        theType: CFNumberType,
        valuePtr: *mut c_void,
    ) -> c_int;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopRunInMode(
        mode: CFStringRef,
        seconds: f64,
        returnAfterSourceHandled: c_int,
    ) -> c_int;
    pub fn CFRunLoopSourceCreate(
        alloc: CFAllocatorRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CGEventTapRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopTimerCreate(
        alloc: CFAllocatorRef,
        fireDate: f64,
        interval: f64,
        flags: c_int,
        order: CFIndex,
        callout: CFRunLoopTimerCallBack,
        context: *mut CFRunLoopTimerContext,
    ) -> *const c_void;
    pub fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: *const c_void, mode: CFRunLoopMode);
    pub fn CFRunLoopRemoveTimer(rl: CFRunLoopRef, timer: *const c_void, mode: CFRunLoopMode);
    pub fn CFAbsoluteTimeGetCurrent() -> f64;
    pub fn CFNumberGetType(number: CFNumberRef) -> CFNumberType;

    pub static kCFRunLoopDefaultMode: CFStringConstant;
}

// kCGEventSourceStateCombinedSessionState
pub const kCGEventSourceStateCombinedSessionState: c_int = 0;
// CGEventTapLocation values already defined above.

// CGPoint for CGWindowNumberFromCGPoint (unused param sentinel kept simple).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

// CGEventTapCallBack signature. Returns an event (possibly modified/null to
// drop), or the passed event to pass it through.
pub type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    type_: CGEventType,
    event: CGEventRef,
    userInfo: *const c_void,
) -> CGEventRef;

// CFNumberType
pub type CFNumberType = c_int;
pub const kCFNumberSInt32Type: CFNumberType = 3;
pub const kCFNumberSInt64Type: CFNumberType = 4;

// CFRunLoopSourceContext (we only use the `info` pointer + perform; most fields
// are zeroed). Layout matches CoreFoundation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFRunLoopSourceContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copyDescription: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
    pub hash: Option<unsafe extern "C" fn(*const c_void) -> c_ulong>,
    pub schedule: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
    pub perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFRunLoopTimerContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copyDescription: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

pub type CFRunLoopTimerCallBack = unsafe extern "C" fn(timer: *const c_void, info: *mut c_void);

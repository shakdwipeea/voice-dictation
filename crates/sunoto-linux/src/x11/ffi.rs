//! Raw Xlib/XTEST bindings used by the X11 adapters. Layouts mirror Xlib.h.

#![allow(non_snake_case, dead_code)]

use std::os::raw::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong};

#[repr(C)]
pub struct Display {
    _private: [u8; 0],
}

#[repr(C)]
pub struct GCRec {
    _private: [u8; 0],
}

pub type Window = c_ulong;
pub type Atom = c_ulong;
pub type Time = c_ulong;
pub type KeySym = c_ulong;
pub type KeyCode = u8;
pub type Colormap = c_ulong;
pub type GC = *mut GCRec;
pub type Bool = c_int;

pub const KEY_PRESS: c_int = 2;
pub const KEY_RELEASE: c_int = 3;
pub const EXPOSE: c_int = 12;
pub const MAP_NOTIFY: c_int = 19;
pub const SELECTION_CLEAR: c_int = 29;
pub const SELECTION_REQUEST: c_int = 30;
pub const SELECTION_NOTIFY: c_int = 31;

pub const KEY_PRESS_MASK: c_long = 1;
pub const KEY_RELEASE_MASK: c_long = 1 << 1;
pub const EXPOSURE_MASK: c_long = 1 << 15;
pub const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;

pub const SHIFT_MASK: c_uint = 1;
pub const LOCK_MASK: c_uint = 1 << 1;
pub const CONTROL_MASK: c_uint = 1 << 2;
pub const MOD1_MASK: c_uint = 1 << 3;
pub const MOD2_MASK: c_uint = 1 << 4;
pub const MOD4_MASK: c_uint = 1 << 6;

pub const GRAB_MODE_ASYNC: c_int = 1;
pub const CURRENT_TIME: Time = 0;
pub const REVERT_TO_PARENT: c_int = 2;
pub const NONE: c_ulong = 0;
pub const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
pub const PROP_MODE_REPLACE: c_int = 0;
pub const ANY_PROPERTY_TYPE: Atom = 0;
pub const XA_ATOM: Atom = 4;
pub const XA_STRING: Atom = 31;
pub const XA_WM_CLASS: Atom = 67;

pub const XK_F8: KeySym = 0xffc5;
pub const XK_CONTROL_L: KeySym = 0xffe3;
pub const XK_CONTROL_R: KeySym = 0xffe4;
pub const XK_SHIFT_L: KeySym = 0xffe1;
pub const XK_SHIFT_R: KeySym = 0xffe2;
pub const XK_ALT_L: KeySym = 0xffe9;
pub const XK_ALT_R: KeySym = 0xffea;
pub const XK_SUPER_L: KeySym = 0xffeb;
pub const XK_SUPER_R: KeySym = 0xffec;
pub const XK_RETURN: KeySym = 0xff0d;
pub const XK_TAB: KeySym = 0xff09;
pub const XK_V: KeySym = 'v' as KeySym;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XKeyEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub keycode: c_uint,
    pub same_screen: Bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XMapEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub override_redirect: Bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XExposeEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub count: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XSelectionRequestEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub owner: Window,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
    pub time: Time,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XSelectionEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
    pub time: Time,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XSelectionClearEvent {
    pub event_type: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub selection: Atom,
    pub time: Time,
}

#[repr(C)]
pub struct XErrorEvent {
    pub event_type: c_int,
    pub display: *mut Display,
    pub resourceid: c_ulong,
    pub serial: c_ulong,
    pub error_code: c_uchar,
    pub request_code: c_uchar,
    pub minor_code: c_uchar,
}

#[repr(C)]
pub union XEvent {
    pub event_type: c_int,
    pub key: XKeyEvent,
    pub map: XMapEvent,
    pub expose: XExposeEvent,
    pub selection_request: XSelectionRequestEvent,
    pub selection: XSelectionEvent,
    pub selection_clear: XSelectionClearEvent,
    pub pad: [c_long; 24],
}

impl XEvent {
    pub fn zeroed() -> Self {
        XEvent { pad: [0; 24] }
    }
}

#[repr(C)]
pub struct XSetWindowAttributes {
    pub background_pixmap: c_ulong,
    pub background_pixel: c_ulong,
    pub border_pixmap: c_ulong,
    pub border_pixel: c_ulong,
    pub bit_gravity: c_int,
    pub win_gravity: c_int,
    pub backing_store: c_int,
    pub backing_planes: c_ulong,
    pub backing_pixel: c_ulong,
    pub save_under: Bool,
    pub event_mask: c_long,
    pub do_not_propagate_mask: c_long,
    pub override_redirect: Bool,
    pub colormap: Colormap,
    pub cursor: c_ulong,
}

impl XSetWindowAttributes {
    pub fn zeroed() -> Self {
        // SAFETY: all fields are plain integers/pointers where zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct XColor {
    pub pixel: c_ulong,
    pub red: u16,
    pub green: u16,
    pub blue: u16,
    pub flags: c_char,
    pub pad: c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

pub const POLLIN: i16 = 1;

unsafe extern "C" {
    pub fn poll(fds: *mut PollFd, nfds: c_ulong, timeout_ms: c_int) -> c_int;
}

#[link(name = "X11")]
unsafe extern "C" {
    pub fn XOpenDisplay(display_name: *const c_char) -> *mut Display;
    pub fn XCloseDisplay(display: *mut Display) -> c_int;
    pub fn XConnectionNumber(display: *mut Display) -> c_int;
    pub fn XDefaultRootWindow(display: *mut Display) -> Window;
    pub fn XDefaultScreen(display: *mut Display) -> c_int;
    pub fn XDisplayWidth(display: *mut Display, screen: c_int) -> c_int;
    pub fn XDisplayHeight(display: *mut Display, screen: c_int) -> c_int;
    pub fn XBlackPixel(display: *mut Display, screen: c_int) -> c_ulong;
    pub fn XWhitePixel(display: *mut Display, screen: c_int) -> c_ulong;
    pub fn XDefaultColormap(display: *mut Display, screen: c_int) -> Colormap;
    pub fn XAllocColor(display: *mut Display, colormap: Colormap, color: *mut XColor) -> c_int;
    pub fn XCreateSimpleWindow(
        display: *mut Display,
        parent: Window,
        x: c_int,
        y: c_int,
        width: c_uint,
        height: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> Window;
    pub fn XChangeWindowAttributes(
        display: *mut Display,
        window: Window,
        value_mask: c_ulong,
        attributes: *mut XSetWindowAttributes,
    ) -> c_int;
    pub fn XSetWindowBackground(
        display: *mut Display,
        window: Window,
        background_pixel: c_ulong,
    ) -> c_int;
    pub fn XDestroyWindow(display: *mut Display, window: Window) -> c_int;
    pub fn XMapRaised(display: *mut Display, window: Window) -> c_int;
    pub fn XRaiseWindow(display: *mut Display, window: Window) -> c_int;
    pub fn XUnmapWindow(display: *mut Display, window: Window) -> c_int;
    pub fn XClearWindow(display: *mut Display, window: Window) -> c_int;
    pub fn XStoreName(display: *mut Display, window: Window, name: *const c_char) -> c_int;
    pub fn XGetInputFocus(
        display: *mut Display,
        focus_return: *mut Window,
        revert_to_return: *mut c_int,
    ) -> c_int;
    pub fn XSetInputFocus(
        display: *mut Display,
        focus: Window,
        revert_to: c_int,
        time: Time,
    ) -> c_int;
    pub fn XKeysymToKeycode(display: *mut Display, keysym: KeySym) -> KeyCode;
    pub fn XStringToKeysym(string: *const c_char) -> KeySym;
    pub fn XGrabKey(
        display: *mut Display,
        keycode: c_int,
        modifiers: c_uint,
        grab_window: Window,
        owner_events: c_int,
        pointer_mode: c_int,
        keyboard_mode: c_int,
    ) -> c_int;
    pub fn XUngrabKey(
        display: *mut Display,
        keycode: c_int,
        modifiers: c_uint,
        grab_window: Window,
    );
    pub fn XSelectInput(display: *mut Display, window: Window, event_mask: c_long) -> c_int;
    pub fn XNextEvent(display: *mut Display, event: *mut XEvent) -> c_int;
    pub fn XPeekEvent(display: *mut Display, event: *mut XEvent) -> c_int;
    pub fn XPending(display: *mut Display) -> c_int;
    pub fn XLookupString(
        event: *mut XKeyEvent,
        buffer: *mut c_char,
        buffer_size: c_int,
        keysym: *mut KeySym,
        compose_status: *mut std::ffi::c_void,
    ) -> c_int;
    pub fn XSync(display: *mut Display, discard: c_int) -> c_int;
    pub fn XFlush(display: *mut Display) -> c_int;
    pub fn XQueryKeymap(display: *mut Display, keys: *mut [c_char; 32]) -> c_int;
    pub fn XkbSetDetectableAutoRepeat(
        display: *mut Display,
        detectable: c_int,
        supported: *mut c_int,
    ) -> c_int;
    pub fn XSetErrorHandler(
        handler: Option<unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int>,
    ) -> Option<unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int>;
    pub fn XGetErrorText(
        display: *mut Display,
        code: c_int,
        buffer: *mut c_char,
        length: c_int,
    ) -> c_int;
    pub fn XInternAtom(
        display: *mut Display,
        atom_name: *const c_char,
        only_if_exists: Bool,
    ) -> Atom;
    pub fn XSetSelectionOwner(
        display: *mut Display,
        selection: Atom,
        owner: Window,
        time: Time,
    ) -> c_int;
    pub fn XGetSelectionOwner(display: *mut Display, selection: Atom) -> Window;
    pub fn XConvertSelection(
        display: *mut Display,
        selection: Atom,
        target: Atom,
        property: Atom,
        requestor: Window,
        time: Time,
    ) -> c_int;
    pub fn XQueryTree(
        display: *mut Display,
        window: Window,
        root_return: *mut Window,
        parent_return: *mut Window,
        children_return: *mut *mut Window,
        nchildren_return: *mut c_uint,
    ) -> c_int;
    pub fn XGetWindowProperty(
        display: *mut Display,
        window: Window,
        property: Atom,
        long_offset: c_long,
        long_length: c_long,
        delete: Bool,
        req_type: Atom,
        actual_type_return: *mut Atom,
        actual_format_return: *mut c_int,
        nitems_return: *mut c_ulong,
        bytes_after_return: *mut c_ulong,
        prop_return: *mut *mut c_uchar,
    ) -> c_int;
    pub fn XChangeProperty(
        display: *mut Display,
        window: Window,
        property: Atom,
        property_type: Atom,
        format: c_int,
        mode: c_int,
        data: *const c_uchar,
        nelements: c_int,
    ) -> c_int;
    pub fn XSendEvent(
        display: *mut Display,
        window: Window,
        propagate: Bool,
        event_mask: c_long,
        event: *mut XEvent,
    ) -> c_int;
    pub fn XFree(data: *mut std::ffi::c_void) -> c_int;
    pub fn XCreateGC(
        display: *mut Display,
        drawable: c_ulong,
        value_mask: c_ulong,
        values: *mut std::ffi::c_void,
    ) -> GC;
    pub fn XFreeGC(display: *mut Display, gc: GC) -> c_int;
    pub fn XSetForeground(display: *mut Display, gc: GC, foreground: c_ulong) -> c_int;
    pub fn XDrawString(
        display: *mut Display,
        drawable: c_ulong,
        gc: GC,
        x: c_int,
        y: c_int,
        string: *const c_char,
        length: c_int,
    ) -> c_int;
}

#[link(name = "Xtst")]
unsafe extern "C" {
    pub fn XTestQueryExtension(
        display: *mut Display,
        event_base: *mut c_int,
        error_base: *mut c_int,
        major: *mut c_int,
        minor: *mut c_int,
    ) -> c_int;
    pub fn XTestFakeKeyEvent(
        display: *mut Display,
        keycode: c_uint,
        is_press: c_int,
        delay: c_ulong,
    ) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xevent_storage_matches_xlib_contract() {
        assert_eq!(
            std::mem::size_of::<XEvent>(),
            24 * std::mem::size_of::<c_long>()
        );
        assert!(std::mem::size_of::<XKeyEvent>() <= std::mem::size_of::<XEvent>());
        assert!(std::mem::size_of::<XSelectionRequestEvent>() <= std::mem::size_of::<XEvent>());
        assert!(std::mem::size_of::<XSetWindowAttributes>() >= 15 * 4);
    }
}

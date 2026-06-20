use super::ffi::*;

use std::ffi::CString;
use std::fmt;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong};
use std::ptr;
use std::sync::Once;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleKind {
    Recording,
    Transcribing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionOutcome {
    Typed,
    Pasted,
    ClipboardOnly,
}

#[derive(Debug)]
pub enum X11Error {
    DisplayUnavailable,
    XTestUnavailable,
    HotkeyUnavailable(String),
    UnsupportedCharacter(char),
    ClipboardUnavailable,
    SelfTestMismatch { expected: String, actual: String },
    HotkeySelfTestMismatch { actual: Vec<HotkeyEvent> },
}

impl fmt::Display for X11Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DisplayUnavailable => write!(formatter, "cannot connect to the X11 display"),
            Self::XTestUnavailable => write!(formatter, "XTEST extension is unavailable"),
            Self::HotkeyUnavailable(shortcut) => {
                write!(formatter, "cannot resolve the {shortcut} hotkey")
            }
            Self::UnsupportedCharacter(character) => {
                write!(
                    formatter,
                    "X11 direct insertion does not support {character:?}"
                )
            }
            Self::ClipboardUnavailable => {
                write!(formatter, "cannot take ownership of the X11 clipboard")
            }
            Self::SelfTestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "X11 insertion self-test expected {expected:?}, received {actual:?}"
                )
            }
            Self::HotkeySelfTestMismatch { actual } => {
                write!(
                    formatter,
                    "X11 push-to-talk self-test expected press/release, received {actual:?}"
                )
            }
        }
    }
}

impl std::error::Error for X11Error {}

// Xlib's default error handler prints and exits the process; a daemon must
// survive async errors (e.g. a BadWindow from a focus target that vanished).
unsafe extern "C" fn log_x_error(display: *mut Display, event: *mut XErrorEvent) -> c_int {
    let mut buffer = [0 as c_char; 256];
    // SAFETY: Xlib invokes the handler with valid pointers; XGetErrorText is
    // one of the few calls explicitly allowed inside an error handler.
    unsafe {
        XGetErrorText(
            display,
            (*event).error_code as c_int,
            buffer.as_mut_ptr(),
            buffer.len() as c_int,
        );
        let text = std::ffi::CStr::from_ptr(buffer.as_ptr()).to_string_lossy();
        eprintln!(
            "[sunoto] X11 error ignored: {text} (request {}, resource 0x{:x})",
            (*event).request_code,
            (*event).resourceid
        );
    }
    0
}

fn install_error_handler() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // SAFETY: installing a process-global handler before any connection
        // generates errors; the handler only logs.
        unsafe { XSetErrorHandler(Some(log_x_error)) };
    });
}

/// One Xlib connection. Each thread must use its own connection; the struct is
/// deliberately !Send (raw pointer) so it is constructed on its owning thread.
struct Connection {
    display: *mut Display,
    root: Window,
}

impl Connection {
    fn open() -> Result<Self, X11Error> {
        install_error_handler();
        // SAFETY: XOpenDisplay accepts a null pointer to use DISPLAY.
        let display = unsafe { XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(X11Error::DisplayUnavailable);
        }
        let mut event_base = 0;
        let mut error_base = 0;
        let mut major = 0;
        let mut minor = 0;
        // SAFETY: display is valid and all output pointers are writable.
        let has_xtest = unsafe {
            XTestQueryExtension(
                display,
                &mut event_base,
                &mut error_base,
                &mut major,
                &mut minor,
            )
        };
        if has_xtest == 0 {
            // SAFETY: display was opened successfully.
            unsafe { XCloseDisplay(display) };
            return Err(X11Error::XTestUnavailable);
        }
        // SAFETY: display is valid.
        let root = unsafe { XDefaultRootWindow(display) };
        Ok(Self { display, root })
    }

    fn keycode(&self, keysym: KeySym) -> KeyCode {
        // SAFETY: display is valid for the connection's lifetime.
        unsafe { XKeysymToKeycode(self.display, keysym) }
    }

    fn flush(&self) {
        // SAFETY: display is valid.
        unsafe { XFlush(self.display) };
    }

    fn pending(&self) -> c_int {
        // SAFETY: display is valid.
        unsafe { XPending(self.display) }
    }

    fn next_event(&self) -> XEvent {
        let mut event = XEvent::zeroed();
        // SAFETY: event points to enough storage for any XEvent.
        unsafe { XNextEvent(self.display, &mut event) };
        event
    }

    /// Block until the connection has readable data or the timeout elapses.
    fn wait_readable(&self, timeout: Duration) {
        // SAFETY: display is valid; poll only reads the descriptor state.
        unsafe {
            let mut fds = PollFd {
                fd: XConnectionNumber(self.display),
                events: POLLIN,
                revents: 0,
            };
            poll(
                &mut fds,
                1,
                timeout.as_millis().min(i32::MAX as u128) as c_int,
            );
        }
    }

    fn fake_key_event(&self, keycode: KeyCode, press: bool) {
        // SAFETY: display is valid; XTEST accepts any keycode.
        unsafe {
            XTestFakeKeyEvent(self.display, keycode.into(), press as c_int, CURRENT_TIME);
        }
    }

    fn atom(&self, name: &str) -> Atom {
        let name = CString::new(name).expect("atom names contain no NUL");
        // SAFETY: display is valid and the name is NUL-terminated.
        unsafe { XInternAtom(self.display, name.as_ptr(), 0) }
    }

    /// Keycodes of modifier keys that are physically held right now.
    fn held_modifier_keycodes(&self) -> Vec<KeyCode> {
        const MODIFIER_KEYSYMS: [KeySym; 8] = [
            XK_CONTROL_L,
            XK_CONTROL_R,
            XK_SHIFT_L,
            XK_SHIFT_R,
            XK_ALT_L,
            XK_ALT_R,
            XK_SUPER_L,
            XK_SUPER_R,
        ];
        let mut keymap = [0 as c_char; 32];
        // SAFETY: display is valid and keymap is a writable 32-byte buffer.
        unsafe { XQueryKeymap(self.display, &mut keymap) };
        MODIFIER_KEYSYMS
            .into_iter()
            .map(|keysym| self.keycode(keysym))
            .filter(|&keycode| {
                keycode != 0 && keymap[keycode as usize / 8] as u8 & (1 << (keycode % 8)) != 0
            })
            .collect()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: display stays valid until closed here.
        unsafe { XCloseDisplay(self.display) };
    }
}

/// Run `work` with the user's physically held modifier keys logically
/// released, so dictated characters do not become Ctrl/Alt chords when the
/// user is still holding the push-to-talk modifier.
fn with_cleared_modifiers(connection: &Connection, work: impl FnOnce()) {
    let held = connection.held_modifier_keycodes();
    for &keycode in &held {
        connection.fake_key_event(keycode, false);
    }
    connection.flush();
    work();
    // Restore exactly the keycodes that were physically held: the X server
    // resynchronizes its logical state on the user's eventual real release.
    for &keycode in &held {
        connection.fake_key_event(keycode, true);
    }
    connection.flush();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub modifier_mask: c_uint,
    pub keysym_name: String,
}

impl Shortcut {
    /// Parse "Ctrl+F1" style descriptions. The final element is an X11 keysym
    /// name; the rest are Ctrl/Shift/Alt/Super modifiers.
    pub fn parse(description: &str) -> Result<Self, X11Error> {
        let mut modifier_mask = 0;
        let mut keysym_name = None;
        for part in description.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifier_mask |= CONTROL_MASK,
                "shift" => modifier_mask |= SHIFT_MASK,
                "alt" => modifier_mask |= MOD1_MASK,
                "super" => modifier_mask |= MOD4_MASK,
                "" => return Err(X11Error::HotkeyUnavailable(description.into())),
                _ => {
                    if keysym_name.replace(part.to_string()).is_some() {
                        return Err(X11Error::HotkeyUnavailable(description.into()));
                    }
                }
            }
        }
        match keysym_name {
            Some(keysym_name) if modifier_mask != 0 => Ok(Self {
                modifier_mask,
                keysym_name,
            }),
            // A bare key without modifiers would swallow normal typing.
            _ => Err(X11Error::HotkeyUnavailable(description.into())),
        }
    }
}

impl Default for Shortcut {
    fn default() -> Self {
        Self {
            modifier_mask: CONTROL_MASK,
            keysym_name: "F1".to_string(),
        }
    }
}

pub struct HotkeyListener {
    connection: Connection,
    hotkey: KeyCode,
    modifier_mask: c_uint,
    detectable_autorepeat: bool,
}

impl HotkeyListener {
    pub fn open(shortcut: &Shortcut) -> Result<Self, X11Error> {
        let connection = Connection::open()?;
        let keysym_name = CString::new(shortcut.keysym_name.as_str())
            .map_err(|_| X11Error::HotkeyUnavailable(shortcut.keysym_name.clone()))?;
        // SAFETY: the name is NUL-terminated.
        let keysym = unsafe { XStringToKeysym(keysym_name.as_ptr()) };
        let hotkey = if keysym == 0 {
            0
        } else {
            connection.keycode(keysym)
        };
        if hotkey == 0 {
            return Err(X11Error::HotkeyUnavailable(shortcut.keysym_name.clone()));
        }
        let mut detectable_supported = 0;
        // SAFETY: connection display is valid for the adapter's lifetime.
        unsafe {
            XkbSetDetectableAutoRepeat(connection.display, 1, &mut detectable_supported);
            // Grab under every Caps Lock/Num Lock combination, since those
            // locks are part of the modifier state.
            for lock_bits in [0, LOCK_MASK, MOD2_MASK, LOCK_MASK | MOD2_MASK] {
                XGrabKey(
                    connection.display,
                    hotkey.into(),
                    shortcut.modifier_mask | lock_bits,
                    connection.root,
                    0,
                    GRAB_MODE_ASYNC,
                    GRAB_MODE_ASYNC,
                );
            }
            XSelectInput(
                connection.display,
                connection.root,
                KEY_PRESS_MASK | KEY_RELEASE_MASK,
            );
            XSync(connection.display, 0);
        }
        Ok(Self {
            connection,
            hotkey,
            modifier_mask: shortcut.modifier_mask,
            detectable_autorepeat: detectable_supported != 0,
        })
    }

    /// Wait up to `timeout` for the next push-to-talk transition. Returns None
    /// on timeout so the owning thread can check its stop flag.
    pub fn wait(&self, timeout: Duration) -> Option<HotkeyEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            while self.connection.pending() > 0 {
                let event = self.connection.next_event();
                if let Some(hotkey_event) = self.parse(&event) {
                    return Some(hotkey_event);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            self.connection.wait_readable(deadline - now);
            if self.connection.pending() == 0 {
                return None;
            }
        }
    }

    fn parse(&self, event: &XEvent) -> Option<HotkeyEvent> {
        // SAFETY: event_type is the common first field of every XEvent.
        let event_type = unsafe { event.event_type };
        if event_type != KEY_PRESS && event_type != KEY_RELEASE {
            return None;
        }
        // SAFETY: KEY_PRESS and KEY_RELEASE events use the XKeyEvent layout.
        let key = unsafe { event.key };
        if key.keycode != c_uint::from(self.hotkey) {
            return None;
        }
        if event_type == KEY_PRESS {
            // The press must carry the configured modifiers; lock bits vary.
            if key.state & self.modifier_mask == self.modifier_mask {
                return Some(HotkeyEvent::Pressed);
            }
            return None;
        }
        // Release: match by keycode only. The user may release the modifier
        // first, which strips it from the event state; requiring it here
        // would leave the session recording forever.
        if !self.detectable_autorepeat && self.swallow_autorepeat_pair(&key) {
            return None;
        }
        Some(HotkeyEvent::Released)
    }

    /// Without detectable auto-repeat the server emits release+press pairs
    /// with identical timestamps while the key is held; both must be ignored.
    fn swallow_autorepeat_pair(&self, release: &XKeyEvent) -> bool {
        if self.connection.pending() == 0 {
            return false;
        }
        let mut next = XEvent::zeroed();
        // SAFETY: pending() reported a queued event; next is writable storage.
        unsafe { XPeekEvent(self.connection.display, &mut next) };
        // SAFETY: checked via the common event_type field before key access.
        let is_repeat = unsafe {
            next.event_type == KEY_PRESS
                && next.key.keycode == release.keycode
                && next.key.time == release.time
        };
        if is_repeat {
            let _ = self.connection.next_event();
        }
        is_repeat
    }

    /// Synthesize the full press/release sequence with the modifier released
    /// before the key, the ordering that historically dropped the release.
    pub fn selftest_push_to_talk(&self) -> Result<(), X11Error> {
        let control = self.connection.keycode(XK_CONTROL_L);
        self.connection.fake_key_event(control, true);
        self.connection.fake_key_event(self.hotkey, true);
        self.connection.fake_key_event(control, false);
        self.connection.fake_key_event(self.hotkey, false);
        self.connection.flush();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = Vec::new();
        while Instant::now() < deadline && received.len() < 2 {
            if let Some(event) = self.wait(Duration::from_millis(50)) {
                received.push(event);
            }
        }
        if received != [HotkeyEvent::Pressed, HotkeyEvent::Released] {
            return Err(X11Error::HotkeySelfTestMismatch { actual: received });
        }
        Ok(())
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        // SAFETY: display is valid until the connection drops after this.
        unsafe {
            for lock_bits in [0, LOCK_MASK, MOD2_MASK, LOCK_MASK | MOD2_MASK] {
                XUngrabKey(
                    self.connection.display,
                    self.hotkey.into(),
                    self.modifier_mask | lock_bits,
                    self.connection.root,
                );
            }
        }
    }
}

struct Bubble {
    window: Window,
    gc: GC,
    text: String,
    background: c_ulong,
    visible: bool,
}

/// A mapped, focused test window that receives the injected keystrokes.
pub struct ProbeWindow {
    window: Window,
    previous_focus: Window,
    previous_revert: c_int,
}

struct Atoms {
    clipboard: Atom,
    utf8_string: Atom,
    targets: Atom,
    transfer_property: Atom,
}

/// Insertion, clipboard, and status-bubble operations. Owned by the daemon's
/// UI thread; `pump` must be called regularly to serve clipboard requests and
/// bubble repaints.
pub struct UiAdapter {
    connection: Connection,
    atoms: Atoms,
    service_window: Window,
    served_clipboard: Option<String>,
    bubble: Option<Bubble>,
}

impl UiAdapter {
    pub fn open() -> Result<Self, X11Error> {
        let connection = Connection::open()?;
        let atoms = Atoms {
            clipboard: connection.atom("CLIPBOARD"),
            utf8_string: connection.atom("UTF8_STRING"),
            targets: connection.atom("TARGETS"),
            transfer_property: connection.atom("SUNOTO_SELECTION"),
        };
        // SAFETY: display and root are valid; the window stays unmapped and
        // exists only to own selections and receive their events.
        let service_window = unsafe {
            XCreateSimpleWindow(connection.display, connection.root, -1, -1, 1, 1, 0, 0, 0)
        };
        Ok(Self {
            connection,
            atoms,
            service_window,
            served_clipboard: None,
            bubble: None,
        })
    }

    pub fn focused_window(&self) -> u64 {
        let mut focus: Window = 0;
        let mut revert = REVERT_TO_PARENT;
        // SAFETY: display is valid and both out-pointers are writable.
        unsafe { XGetInputFocus(self.connection.display, &mut focus, &mut revert) };
        focus
    }

    /// WM_CLASS (instance, class) for `window`. The input focus usually sits
    /// on a class-less child widget, so the lookup climbs toward the root
    /// until a window carrying WM_CLASS is found.
    pub fn window_class(&self, window: u64) -> Option<(String, String)> {
        let mut current: Window = window;
        // Bounded climb: a class-less chain all the way up means there is
        // genuinely nothing to read (e.g. focus is None or PointerRoot).
        for _ in 0..32 {
            if current <= 1 || current == self.connection.root {
                return None;
            }
            if let Some(class) = self.read_wm_class(current) {
                return Some(class);
            }
            current = self.parent_window(current)?;
        }
        None
    }

    fn read_wm_class(&self, window: Window) -> Option<(String, String)> {
        let mut actual_type: Atom = 0;
        let mut actual_format: c_int = 0;
        let mut item_count: c_ulong = 0;
        let mut bytes_after: c_ulong = 0;
        let mut data: *mut std::os::raw::c_uchar = ptr::null_mut();
        // SAFETY: display is valid, all out-pointers are writable, and the
        // returned buffer is copied out before being XFreed.
        let status = unsafe {
            XGetWindowProperty(
                self.connection.display,
                window,
                XA_WM_CLASS,
                0,
                256, // in 32-bit longs: far beyond any real WM_CLASS
                0,
                XA_STRING,
                &mut actual_type,
                &mut actual_format,
                &mut item_count,
                &mut bytes_after,
                &mut data,
            )
        };
        if status != 0 || data.is_null() {
            return None;
        }
        // SAFETY: Xlib returned `item_count` 8-bit items at `data`.
        let bytes = unsafe { std::slice::from_raw_parts(data, item_count as usize) }.to_vec();
        // SAFETY: data was allocated by Xlib for this property read.
        unsafe { XFree(data.cast()) };
        if actual_type != XA_STRING || actual_format != 8 {
            return None;
        }
        // The property holds two NUL-terminated strings: instance, then class.
        let mut parts = bytes
            .split(|&byte| byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned());
        let instance = parts.next()?;
        let class = parts.next().unwrap_or_else(|| instance.clone());
        Some((instance, class))
    }

    fn parent_window(&self, window: Window) -> Option<Window> {
        let mut root: Window = 0;
        let mut parent: Window = 0;
        let mut children: *mut Window = ptr::null_mut();
        let mut child_count: c_uint = 0;
        // SAFETY: display is valid; the children array is freed immediately.
        let status = unsafe {
            XQueryTree(
                self.connection.display,
                window,
                &mut root,
                &mut parent,
                &mut children,
                &mut child_count,
            )
        };
        if !children.is_null() {
            // SAFETY: children was allocated by Xlib for this query.
            unsafe { XFree(children.cast()) };
        }
        // XQueryTree returns Status: zero means the window is gone.
        if status == 0 || parent == 0 {
            return None;
        }
        Some(parent)
    }

    /// Type `text` into the focused window via XTEST, clearing any physically
    /// held modifiers for the duration. Fails on the first character with no
    /// keysym mapping; callers fall back to the clipboard path.
    pub fn insert_direct(&self, text: &str) -> Result<(), X11Error> {
        // Resolve all keycodes first so unsupported input fails atomically
        // instead of after half the text was typed.
        let mut keystrokes = Vec::with_capacity(text.len());
        for character in text.chars() {
            let (keysym, shift) =
                character_to_keysym(character).ok_or(X11Error::UnsupportedCharacter(character))?;
            let keycode = self.connection.keycode(keysym);
            if keycode == 0 {
                return Err(X11Error::UnsupportedCharacter(character));
            }
            keystrokes.push((keycode, shift));
        }
        let shift_key = self.connection.keycode(XK_SHIFT_L);
        with_cleared_modifiers(&self.connection, || {
            for (keycode, shift) in keystrokes {
                if shift {
                    self.connection.fake_key_event(shift_key, true);
                }
                self.connection.fake_key_event(keycode, true);
                self.connection.fake_key_event(keycode, false);
                if shift {
                    self.connection.fake_key_event(shift_key, false);
                }
            }
        });
        Ok(())
    }

    /// Replace the clipboard with `text`, paste it, then restore the previous
    /// clipboard contents (served by this process until another owner appears).
    pub fn insert_via_clipboard(&mut self, text: &str) -> Result<(), X11Error> {
        let previous = self.read_clipboard(Duration::from_millis(250));
        self.own_clipboard(text)?;
        let control = self.connection.keycode(XK_CONTROL_L);
        let v_key = self.connection.keycode(XK_V);
        with_cleared_modifiers(&self.connection, || {
            self.connection.fake_key_event(control, true);
            self.connection.fake_key_event(v_key, true);
            self.connection.fake_key_event(v_key, false);
            self.connection.fake_key_event(control, false);
        });
        // Serve the paste target's data request before swapping the content
        // back; lazy targets that fetch later receive the restored clipboard.
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            self.pump();
            self.connection.wait_readable(Duration::from_millis(25));
        }
        match previous {
            Some(previous) => self.own_clipboard(&previous)?,
            None => self.release_clipboard(),
        }
        Ok(())
    }

    /// Put `text` on the clipboard without pasting (used when the focused
    /// window changed between release and insertion).
    pub fn set_clipboard(&mut self, text: &str) -> Result<(), X11Error> {
        self.own_clipboard(text)
    }

    fn own_clipboard(&mut self, text: &str) -> Result<(), X11Error> {
        self.served_clipboard = Some(text.to_string());
        // SAFETY: display, selection atom, and service window are valid.
        unsafe {
            XSetSelectionOwner(
                self.connection.display,
                self.atoms.clipboard,
                self.service_window,
                CURRENT_TIME,
            );
            if XGetSelectionOwner(self.connection.display, self.atoms.clipboard)
                != self.service_window
            {
                return Err(X11Error::ClipboardUnavailable);
            }
        }
        self.connection.flush();
        Ok(())
    }

    fn release_clipboard(&mut self) {
        self.served_clipboard = None;
        // SAFETY: display and selection atom are valid.
        unsafe {
            XSetSelectionOwner(
                self.connection.display,
                self.atoms.clipboard,
                NONE,
                CURRENT_TIME,
            );
        }
        self.connection.flush();
    }

    /// Fetch the current clipboard text, pumping our own selection serving
    /// while waiting so a self-owned clipboard also resolves.
    pub fn read_clipboard(&mut self, timeout: Duration) -> Option<String> {
        // SAFETY: display and atoms are valid.
        unsafe {
            if XGetSelectionOwner(self.connection.display, self.atoms.clipboard) == NONE {
                return None;
            }
            XConvertSelection(
                self.connection.display,
                self.atoms.clipboard,
                self.atoms.utf8_string,
                self.atoms.transfer_property,
                self.service_window,
                CURRENT_TIME,
            );
        }
        self.connection.flush();
        let deadline = Instant::now() + timeout;
        loop {
            while self.connection.pending() > 0 {
                let event = self.connection.next_event();
                // SAFETY: event_type is the common first field.
                let event_type = unsafe { event.event_type };
                if event_type == SELECTION_NOTIFY {
                    // SAFETY: SELECTION_NOTIFY uses the XSelectionEvent layout.
                    let notify = unsafe { event.selection };
                    if notify.selection == self.atoms.clipboard {
                        if notify.property == NONE {
                            return None;
                        }
                        return self.read_transfer_property();
                    }
                } else {
                    self.dispatch(&event);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            self.connection.wait_readable(deadline - now);
        }
    }

    fn read_transfer_property(&self) -> Option<String> {
        let mut actual_type: Atom = 0;
        let mut actual_format: c_int = 0;
        let mut item_count: c_ulong = 0;
        let mut bytes_after: c_ulong = 0;
        let mut data: *mut u8 = ptr::null_mut();
        // SAFETY: display/window/property are valid and out-pointers writable.
        let status = unsafe {
            XGetWindowProperty(
                self.connection.display,
                self.service_window,
                self.atoms.transfer_property,
                0,
                (usize::MAX / 4) as c_long,
                1,
                ANY_PROPERTY_TYPE,
                &mut actual_type,
                &mut actual_format,
                &mut item_count,
                &mut bytes_after,
                &mut data,
            )
        };
        if status != 0 || data.is_null() {
            return None;
        }
        // INCR (incremental transfer) means a large clipboard; restoring it is
        // not worth implementing the protocol, so treat it as unreadable.
        let incremental = actual_type == self.connection.atom("INCR");
        let result = if incremental || actual_format != 8 {
            None
        } else {
            // SAFETY: Xlib returned item_count bytes of 8-bit data.
            let bytes = unsafe { std::slice::from_raw_parts(data, item_count as usize) };
            Some(String::from_utf8_lossy(bytes).into_owned())
        };
        // SAFETY: data was allocated by Xlib.
        unsafe { XFree(data.cast()) };
        result
    }

    /// Service pending X events: clipboard requests, ownership loss, and
    /// bubble repaints. Must be called regularly by the owning thread.
    pub fn pump(&mut self) {
        while self.connection.pending() > 0 {
            let event = self.connection.next_event();
            self.dispatch(&event);
        }
    }

    fn dispatch(&mut self, event: &XEvent) {
        // SAFETY: event_type is the common first field of every XEvent.
        let event_type = unsafe { event.event_type };
        match event_type {
            SELECTION_REQUEST => {
                // SAFETY: SELECTION_REQUEST uses XSelectionRequestEvent layout.
                let request = unsafe { event.selection_request };
                self.answer_selection_request(&request);
            }
            SELECTION_CLEAR => {
                // Another application took the clipboard; stop serving ours.
                self.served_clipboard = None;
            }
            EXPOSE => {
                // SAFETY: EXPOSE uses the XExposeEvent layout.
                let expose = unsafe { event.expose };
                if let Some(bubble) = &self.bubble
                    && bubble.visible
                    && bubble.window == expose.window
                {
                    self.draw_bubble();
                }
            }
            _ => {}
        }
    }

    fn answer_selection_request(&mut self, request: &XSelectionRequestEvent) {
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let served = match (&self.served_clipboard, request.target) {
            (Some(_), target) if target == self.atoms.targets => {
                let supported = [self.atoms.targets, self.atoms.utf8_string, XA_STRING];
                // SAFETY: requestor window and atoms come from the request.
                unsafe {
                    XChangeProperty(
                        self.connection.display,
                        request.requestor,
                        property,
                        XA_ATOM,
                        32,
                        PROP_MODE_REPLACE,
                        supported.as_ptr().cast(),
                        supported.len() as c_int,
                    );
                }
                true
            }
            (Some(text), target) if target == self.atoms.utf8_string || target == XA_STRING => {
                let payload_type = if target == XA_STRING {
                    XA_STRING
                } else {
                    self.atoms.utf8_string
                };
                // SAFETY: requestor window and atoms come from the request.
                unsafe {
                    XChangeProperty(
                        self.connection.display,
                        request.requestor,
                        property,
                        payload_type,
                        8,
                        PROP_MODE_REPLACE,
                        text.as_ptr(),
                        text.len() as c_int,
                    );
                }
                true
            }
            _ => false,
        };
        let mut reply = XEvent::zeroed();
        reply.selection = XSelectionEvent {
            event_type: SELECTION_NOTIFY,
            serial: 0,
            send_event: 1,
            display: self.connection.display,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { NONE },
            time: request.time,
        };
        // SAFETY: the reply event is fully initialized for the requestor.
        unsafe {
            XSendEvent(self.connection.display, request.requestor, 0, 0, &mut reply);
        }
        self.connection.flush();
    }

    pub fn bubble_show(&mut self, kind: BubbleKind, text: &str) {
        let background = self.bubble_color(kind);
        if self.bubble.is_none() {
            self.bubble = self.create_bubble();
        }
        let Some(bubble) = &mut self.bubble else {
            return;
        };
        bubble.background = background;
        bubble.text = sanitize_bubble_text(text);
        let window = bubble.window;
        let visible = bubble.visible;
        // SAFETY: the bubble window is valid until destroyed in Drop.
        unsafe {
            XSetWindowBackground(self.connection.display, window, background);
            if visible {
                XClearWindow(self.connection.display, window);
                XRaiseWindow(self.connection.display, window);
            } else {
                XMapRaised(self.connection.display, window);
            }
        }
        if let Some(bubble) = &mut self.bubble {
            bubble.visible = true;
        }
        self.draw_bubble();
        self.connection.flush();
    }

    pub fn bubble_hide(&mut self) {
        if let Some(bubble) = &mut self.bubble
            && bubble.visible
        {
            // SAFETY: the bubble window is valid until destroyed in Drop.
            unsafe { XUnmapWindow(self.connection.display, bubble.window) };
            bubble.visible = false;
            self.connection.flush();
        }
    }

    fn bubble_color(&self, kind: BubbleKind) -> c_ulong {
        let (red, green, blue) = match kind {
            BubbleKind::Recording => (0x2e, 0x7d, 0x32),
            BubbleKind::Transcribing => (0x8d, 0x6e, 0x00),
            BubbleKind::Error => (0xc6, 0x28, 0x28),
        };
        let mut color = XColor {
            red: (red as u16) << 8,
            green: (green as u16) << 8,
            blue: (blue as u16) << 8,
            ..XColor::default()
        };
        // SAFETY: display and default colormap are valid.
        let allocated = unsafe {
            let screen = XDefaultScreen(self.connection.display);
            let colormap = XDefaultColormap(self.connection.display, screen);
            XAllocColor(self.connection.display, colormap, &mut color)
        };
        if allocated != 0 {
            color.pixel
        } else {
            // SAFETY: display is valid.
            unsafe {
                XBlackPixel(
                    self.connection.display,
                    XDefaultScreen(self.connection.display),
                )
            }
        }
    }

    fn create_bubble(&self) -> Option<Bubble> {
        const WIDTH: c_uint = 340;
        const HEIGHT: c_uint = 32;
        // SAFETY: display/root are valid; the window is configured override-
        // redirect before mapping so the window manager never focuses it.
        unsafe {
            let screen = XDefaultScreen(self.connection.display);
            let screen_width = XDisplayWidth(self.connection.display, screen);
            let screen_height = XDisplayHeight(self.connection.display, screen);
            let x = (screen_width - WIDTH as c_int) / 2;
            let y = screen_height - HEIGHT as c_int - 64;
            let window = XCreateSimpleWindow(
                self.connection.display,
                self.connection.root,
                x,
                y,
                WIDTH,
                HEIGHT,
                0,
                0,
                XBlackPixel(self.connection.display, screen),
            );
            let mut attributes = XSetWindowAttributes::zeroed();
            attributes.override_redirect = 1;
            XChangeWindowAttributes(
                self.connection.display,
                window,
                CW_OVERRIDE_REDIRECT,
                &mut attributes,
            );
            let title = b"Sunoto\0";
            XStoreName(self.connection.display, window, title.as_ptr().cast());
            XSelectInput(self.connection.display, window, EXPOSURE_MASK);
            let gc = XCreateGC(self.connection.display, window, 0, ptr::null_mut());
            if gc.is_null() {
                XDestroyWindow(self.connection.display, window);
                return None;
            }
            Some(Bubble {
                window,
                gc,
                text: String::new(),
                background: XBlackPixel(self.connection.display, screen),
                visible: false,
            })
        }
    }

    fn draw_bubble(&self) {
        let Some(bubble) = &self.bubble else {
            return;
        };
        // SAFETY: bubble window/gc are valid; the text is NUL-free ASCII.
        unsafe {
            let screen = XDefaultScreen(self.connection.display);
            XSetForeground(
                self.connection.display,
                bubble.gc,
                XWhitePixel(self.connection.display, screen),
            );
            XDrawString(
                self.connection.display,
                bubble.window,
                bubble.gc,
                12,
                20,
                bubble.text.as_ptr().cast(),
                bubble.text.len() as c_int,
            );
        }
        self.connection.flush();
    }

    /// Create a mapped, focused probe window that echoes typed characters
    /// back. Used by the self-test and the latency bench so insertion can be
    /// measured against a real focused X11 target.
    pub fn create_probe_window(&mut self) -> Result<ProbeWindow, X11Error> {
        let mut previous_focus = 0;
        let mut previous_revert = REVERT_TO_PARENT;
        // SAFETY: display is valid and the output pointers are writable.
        unsafe {
            XGetInputFocus(
                self.connection.display,
                &mut previous_focus,
                &mut previous_revert,
            );
        }
        // SAFETY: display and root are valid.
        let window = unsafe {
            XCreateSimpleWindow(
                self.connection.display,
                self.connection.root,
                0,
                0,
                320,
                80,
                0,
                0,
                0,
            )
        };
        let title = b"Sunoto X11 Probe\0";
        // SAFETY: window is valid and the title is NUL-terminated.
        unsafe {
            XStoreName(self.connection.display, window, title.as_ptr().cast());
            XSelectInput(
                self.connection.display,
                window,
                KEY_PRESS_MASK | STRUCTURE_NOTIFY_MASK,
            );
            XMapRaised(self.connection.display, window);
        }
        self.connection.flush();
        // Focus may only be assigned once the window is viewable; waiting for
        // MapNotify (instead of sleeping) removes the BadMatch race.
        if !self.wait_for_map(window, Duration::from_secs(2)) {
            // SAFETY: window is valid.
            unsafe { XDestroyWindow(self.connection.display, window) };
            return Err(X11Error::SelfTestMismatch {
                expected: "probe window mapped".into(),
                actual: "window never became viewable".into(),
            });
        }
        // SAFETY: the window is mapped and the display is valid.
        unsafe {
            XSetInputFocus(
                self.connection.display,
                window,
                REVERT_TO_PARENT,
                CURRENT_TIME,
            );
            XSync(self.connection.display, 0);
        }
        Ok(ProbeWindow {
            window,
            previous_focus,
            previous_revert,
        })
    }

    /// Type `text` into the focused probe window and wait until every
    /// character has echoed back. Returns the elapsed injection-to-arrival
    /// time, the latency the product gates on.
    pub fn type_and_confirm(
        &mut self,
        probe: &ProbeWindow,
        text: &str,
    ) -> Result<Duration, X11Error> {
        let started = Instant::now();
        self.insert_direct(text)?;
        let deadline = started + Duration::from_secs(2);
        let mut received = Vec::new();
        while Instant::now() < deadline && received.len() < text.len() {
            while self.connection.pending() > 0 {
                let mut event = self.connection.next_event();
                // SAFETY: event_type is the common first field; KEY_PRESS
                // events use the XKeyEvent layout.
                let is_probe_key =
                    unsafe { event.event_type == KEY_PRESS && event.key.window == probe.window };
                if !is_probe_key {
                    self.dispatch(&event);
                    continue;
                }
                let mut buffer = [0 as c_char; 8];
                // SAFETY: KEY_PRESS events use XKeyEvent layout; buffer is
                // writable for up to its length.
                let length = unsafe {
                    XLookupString(
                        (&mut event as *mut XEvent).cast(),
                        buffer.as_mut_ptr(),
                        buffer.len() as c_int,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    )
                };
                received.extend(buffer[..length.max(0) as usize].iter().map(|b| *b as u8));
            }
            if received.len() >= text.len() {
                break;
            }
            self.connection.wait_readable(Duration::from_millis(5));
        }
        let elapsed = started.elapsed();
        let actual = String::from_utf8_lossy(&received).into_owned();
        if actual != text {
            return Err(X11Error::SelfTestMismatch {
                expected: text.into(),
                actual,
            });
        }
        Ok(elapsed)
    }

    pub fn destroy_probe_window(&mut self, probe: ProbeWindow) {
        // SAFETY: both windows and the display stay valid through this call.
        unsafe {
            if probe.previous_focus > 1 {
                XSetInputFocus(
                    self.connection.display,
                    probe.previous_focus,
                    probe.previous_revert,
                    CURRENT_TIME,
                );
            }
            XDestroyWindow(self.connection.display, probe.window);
            XSync(self.connection.display, 0);
        }
    }

    /// Insert text into a freshly created focused window and verify the typed
    /// characters arrive exactly, restoring the previous focus afterwards.
    pub fn selftest_insert(&mut self, text: &str) -> Result<(), X11Error> {
        let probe = self.create_probe_window()?;
        let result = self.type_and_confirm(&probe, text);
        self.destroy_probe_window(probe);
        result.map(|_| ())
    }

    fn wait_for_map(&mut self, window: Window, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            while self.connection.pending() > 0 {
                let event = self.connection.next_event();
                // SAFETY: event_type is the common first field.
                let event_type = unsafe { event.event_type };
                if event_type == MAP_NOTIFY {
                    // SAFETY: MAP_NOTIFY uses the XMapEvent layout.
                    let map = unsafe { event.map };
                    if map.window == window {
                        return true;
                    }
                } else {
                    self.dispatch(&event);
                }
            }
            self.connection.wait_readable(Duration::from_millis(20));
        }
        false
    }

    /// Verify the WM_CLASS lookup against a real window: the probe window
    /// carries a class, and a class-less child must resolve through the
    /// parent climb exactly like a focused application widget does.
    pub fn selftest_window_class(&mut self) -> Result<(), X11Error> {
        let probe = self.create_probe_window()?;
        let class_bytes = b"sunoto-probe\0Sunoto-Probe\0";
        // SAFETY: probe.window is valid and the property bytes outlive the call.
        let child = unsafe {
            XChangeProperty(
                self.connection.display,
                probe.window,
                XA_WM_CLASS,
                XA_STRING,
                8,
                PROP_MODE_REPLACE,
                class_bytes.as_ptr(),
                class_bytes.len() as c_int,
            );
            XCreateSimpleWindow(self.connection.display, probe.window, 0, 0, 1, 1, 0, 0, 0)
        };
        // SAFETY: display is valid; sync makes the new window queryable.
        unsafe { XSync(self.connection.display, 0) };
        let resolved = self.window_class(child);
        // SAFETY: child belongs to this connection.
        unsafe { XDestroyWindow(self.connection.display, child) };
        self.destroy_probe_window(probe);
        match resolved {
            Some((instance, class)) if instance == "sunoto-probe" && class == "Sunoto-Probe" => {
                Ok(())
            }
            other => Err(X11Error::SelfTestMismatch {
                expected: "sunoto-probe / Sunoto-Probe".into(),
                actual: format!("{other:?}"),
            }),
        }
    }

    /// Round-trip the clipboard through the real selection protocol: own it,
    /// read it back via ConvertSelection, then restore the previous content.
    pub fn selftest_clipboard(&mut self) -> Result<(), X11Error> {
        let previous = self.read_clipboard(Duration::from_millis(250));
        let expected = "sunoto clipboard self-test";
        self.own_clipboard(expected)?;
        let actual = self.read_clipboard(Duration::from_secs(1));
        match previous {
            Some(previous) => self.own_clipboard(&previous)?,
            None => self.release_clipboard(),
        }
        if actual.as_deref() != Some(expected) {
            return Err(X11Error::SelfTestMismatch {
                expected: expected.into(),
                actual: actual.unwrap_or_else(|| "<unreadable>".into()),
            });
        }
        Ok(())
    }
}

impl Drop for UiAdapter {
    fn drop(&mut self) {
        // SAFETY: all resources belong to this connection and are destroyed
        // before the connection itself drops.
        unsafe {
            if let Some(bubble) = &self.bubble {
                XFreeGC(self.connection.display, bubble.gc);
                XDestroyWindow(self.connection.display, bubble.window);
            }
            XDestroyWindow(self.connection.display, self.service_window);
        }
    }
}

fn sanitize_bubble_text(text: &str) -> String {
    let mut sanitized: String = text
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() || character == ' ' {
                character
            } else {
                '?'
            }
        })
        .collect();
    const MAX: usize = 44;
    if sanitized.len() > MAX {
        sanitized.truncate(MAX - 3);
        sanitized.push_str("...");
    }
    sanitized
}

fn character_to_keysym(character: char) -> Option<(KeySym, bool)> {
    let direct = match character {
        '\n' => return Some((XK_RETURN, false)),
        '\t' => return Some((XK_TAB, false)),
        'a'..='z'
        | '0'..='9'
        | ' '
        | '-'
        | '='
        | '['
        | ']'
        | '\\'
        | ';'
        | '\''
        | ','
        | '.'
        | '/'
        | '`' => character as KeySym,
        'A'..='Z' => return Some((character.to_ascii_lowercase() as KeySym, true)),
        '!' => '1' as KeySym,
        '@' => '2' as KeySym,
        '#' => '3' as KeySym,
        '$' => '4' as KeySym,
        '%' => '5' as KeySym,
        '^' => '6' as KeySym,
        '&' => '7' as KeySym,
        '*' => '8' as KeySym,
        '(' => '9' as KeySym,
        ')' => '0' as KeySym,
        '_' => '-' as KeySym,
        '+' => '=' as KeySym,
        '{' => '[' as KeySym,
        '}' => ']' as KeySym,
        '|' => '\\' as KeySym,
        ':' => ';' as KeySym,
        '"' => '\'' as KeySym,
        '<' => ',' as KeySym,
        '>' => '.' as KeySym,
        '?' => '/' as KeySym,
        '~' => '`' as KeySym,
        _ => return None,
    };
    Some((
        direct,
        character.is_ascii_punctuation()
            && !matches!(
                character,
                '-' | '=' | '[' | ']' | '\\' | ';' | '\'' | ',' | '.' | '/' | '`'
            ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_ascii_and_shifted_characters() {
        assert_eq!(character_to_keysym('a'), Some(('a' as KeySym, false)));
        assert_eq!(character_to_keysym('A'), Some(('a' as KeySym, true)));
        assert_eq!(character_to_keysym('?'), Some(('/' as KeySym, true)));
        assert_eq!(character_to_keysym('\n'), Some((XK_RETURN, false)));
        assert_eq!(character_to_keysym('é'), None);
    }

    #[test]
    fn shortcut_parsing_requires_modifier_plus_key() {
        assert_eq!(
            Shortcut::parse("Ctrl+F1").unwrap(),
            Shortcut {
                modifier_mask: CONTROL_MASK,
                keysym_name: "F1".into()
            }
        );
        assert_eq!(
            Shortcut::parse("ctrl+shift+space").unwrap().modifier_mask,
            CONTROL_MASK | SHIFT_MASK
        );
        assert!(Shortcut::parse("F8").is_err());
        assert!(Shortcut::parse("Ctrl+").is_err());
        assert!(Shortcut::parse("Ctrl+F8+F9").is_err());
        assert!(Shortcut::parse("").is_err());
        assert_eq!(Shortcut::default(), Shortcut::parse("Ctrl+F1").unwrap());
    }

    #[test]
    fn bubble_text_is_ascii_and_bounded() {
        assert_eq!(sanitize_bubble_text("recording"), "recording");
        assert_eq!(sanitize_bubble_text("héllo\n"), "h?llo?");
        let long = "x".repeat(100);
        let sanitized = sanitize_bubble_text(&long);
        assert_eq!(sanitized.len(), 44);
        assert!(sanitized.ends_with("..."));
    }
}

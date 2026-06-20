//! Global hotkey listener via a system-wide CGEventTap (Phase 3).
//!
//! A dedicated thread runs a CoreGraphics event tap on its own CFRunLoop. The
//! tap callback detects the configured shortcut (modifier flags + key
//! down/up) and pushes [`HotkeyEvent`]s into an mpsc channel; [`wait`] polls
//! that channel with a timeout, matching the Linux `HotkeyListener` contract.
//!
//! Requires Accessibility (and Input Monitoring on recent macOS) TCC
//! permission. If `CGEventTapCreate` returns null the listener reports
//! `DisplayUnavailable`; the daemon logs it and the user is guided to grant
//! permission.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ffi;
use crate::types::{HotkeyEvent, Shortcut, X11Error};

/// macOS virtual key codes for the keys we support as shortcut targets.
fn keycode_for_name(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "f1" => 0x7a,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6d,
        "f11" => 0x67,
        "f12" => 0x6f,
        "f13" => 0x69,
        "f14" => 0x6b,
        "f15" => 0x71,
        "f16" => 0x6a,
        "f17" => 0x40,
        "f18" => 0x4f,
        "f19" => 0x50,
        "space" => 0x31,
        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "esc" | "escape" => 0x35,
        _ => return None,
    })
}

#[cfg(test)]
fn modifier_keycodes(mask: u64) -> Vec<u16> {
    let mut keycodes = Vec::new();
    if mask & ffi::kCGEventFlagMaskControl != 0 {
        keycodes.push(0x3b); // left Control
    }
    if mask & ffi::kCGEventFlagMaskShift != 0 {
        keycodes.push(0x38); // left Shift
    }
    if mask & ffi::kCGEventFlagMaskAlternate != 0 {
        keycodes.push(0x3a); // left Option
    }
    if mask & ffi::kCGEventFlagMaskCommand != 0 {
        keycodes.push(0x37); // left Command
    }
    keycodes
}

/// Bitmask of "interesting" event types for the tap: key down/up + flags
/// changed. (The tap-disabled timeout is not a real bitmask bit.)
const EVENT_MASK: u64 = (1u64 << ffi::kCGEventKeyDown)
    | (1u64 << ffi::kCGEventKeyUp)
    | (1u64 << ffi::kCGEventFlagsChanged);

struct TapState {
    key_code: u16,
    modifier_mask: u64,
    key_down: bool,
    tx: mpsc::Sender<HotkeyEvent>,
    /// The Mach port backing the event tap. Stored so the callback can
    /// re-enable the tap when macOS disables it (kCGSessionEventTapTimeout),
    /// which otherwise leaves the tap inert forever. Cleared (set null) until
    /// the worker thread wires it up after `CGEventTapCreate` succeeds.
    tap: ffi::CGEventTapRef,
}

unsafe extern "C" fn tap_callback(
    _proxy: ffi::CGEventTapProxy,
    type_: ffi::CGEventType,
    event: ffi::CGEventRef,
    user_info: *const c_void,
) -> ffi::CGEventRef {
    if event.is_null() {
        return event;
    }
    if type_ == ffi::kCGSessionEventTapTimeout {
        // macOS disabled the tap (idle timeout or a transient permission
        // hiccup). Re-arm it immediately; otherwise the tap stays inert and
        // no hotkey ever fires again until the daemon restarts.
        let state = unsafe { &*(user_info as *const TapState) };
        if !state.tap.is_null() {
            unsafe { ffi::CGEventTapEnable(state.tap, 1) };
        }
        return event;
    }
    // SAFETY: user_info points at a TapState that lives for the tap's lifetime
    // (kept in TapHandle); the callback only reads/writes POD-ish fields.
    let state = unsafe { &mut *(user_info as *mut TapState) };
    let flags = unsafe { ffi::CGEventGetFlags(event) };
    let keycode =
        unsafe { ffi::CGEventGetIntegerValueField(event, ffi::kCGKeyboardEventKeycode) } as u16;
    // DIAGNOSTIC: log the first few keyboard events the tap sees, so we can
    // tell "tap receives no events at all" (permission / Secure Input) from
    // "events flow but key not matched". Runs only a handful of times.
    static DIAG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let prev = DIAG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if prev < 8 {
        eprintln!(
            "[hotkey-diag] event #{} type={:#x} keycode={} flags={:#x}",
            prev, type_, keycode, flags
        );
    }
    handle_key_event(state, type_, keycode, flags);
    event
}

fn handle_key_event(state: &mut TapState, type_: ffi::CGEventType, keycode: u16, flags: u64) {
    let modifiers_match = (flags & state.modifier_mask) == state.modifier_mask;
    let is_target_key = keycode == state.key_code;

    match type_ {
        ffi::kCGEventKeyDown if is_target_key && modifiers_match => {
            if !state.key_down {
                state.key_down = true;
                let _ = state.tx.send(HotkeyEvent::Pressed);
            }
        }
        ffi::kCGEventKeyUp if is_target_key && state.key_down => {
            state.key_down = false;
            let _ = state.tx.send(HotkeyEvent::Released);
        }
        _ => {}
    }
}

struct TapHandle {
    tap: ffi::CGEventTapRef,
    source: ffi::CFRunLoopSourceRef,
    state_ptr: *mut TapState,
    run_loop: Arc<RunLoopSlot>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Shared slot holding the worker thread's CFRunLoop ref, so `Drop` (on
/// another thread) can call `CFRunLoopWakeUp` on it to promptly break the
/// worker's `CFRunLoopRunInMode` polling loop. Raw pointers are not
/// `Send`/`Sync`, so we wrap them in a marker-typed newtype that is safe to
/// share: the pointer is only read after being stored by the worker and is
/// only used to call the thread-safe `CFRunLoopWakeUp`.
struct RunLoopSlot(Mutex<Option<ffi::CFRunLoopRef>>);
unsafe impl Send for RunLoopSlot {}
unsafe impl Sync for RunLoopSlot {}

pub struct HotkeyListener {
    rx: Receiver<HotkeyEvent>,
    #[allow(dead_code)]
    tap: Arc<TapHandle>,
    key_code: u16,
    modifier_mask: u64,
}

impl HotkeyListener {
    pub fn open(shortcut: &Shortcut) -> Result<Self, X11Error> {
        if !unsafe { ffi::CGPreflightListenEventAccess() } {
            unsafe {
                let _ = ffi::CGRequestListenEventAccess();
            }
        }
        let key_code = keycode_for_name(&shortcut.key_name)
            .ok_or_else(|| X11Error::HotkeyUnavailable(shortcut.key_name.clone()))?;

        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let state = Box::new(TapState {
            key_code,
            modifier_mask: shortcut.modifier_mask,
            key_down: false,
            tx,
            tap: std::ptr::null_mut(),
        });
        let state_ptr = Box::into_raw(state);

        let run_loop_for_thread = Arc::new(RunLoopSlot(Mutex::new(None)));
        let run_loop_clone = Arc::clone(&run_loop_for_thread);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let state_as_usize = state_ptr as usize;
        let (ready_tx, ready_rx) = mpsc::channel::<Option<(usize, usize)>>();

        let thread = std::thread::spawn(move || {
            // SAFETY: create, source, add, enable, and run the tap on this
            // worker thread's CFRunLoop. The callback's TapState pointer is
            // owned by TapHandle after setup succeeds.
            unsafe {
                let state_for_thread = state_as_usize as *const c_void;
                let tap_for_thread = ffi::CGEventTapCreate(
                    ffi::kCGSessionEventTap,
                    ffi::kCGHeadInsertEventTap,
                    ffi::kCGEventTapOptionListenOnly,
                    EVENT_MASK,
                    tap_callback,
                    state_for_thread,
                );
                if tap_for_thread.is_null() {
                    let _ = ready_tx.send(None);
                    return;
                }
                // Wire the tap port into TapState so the callback can
                // re-enable the tap when macOS disables it.
                {
                    let state_ref = &mut *(state_for_thread as *mut TapState);
                    state_ref.tap = tap_for_thread;
                }
                let source_for_thread =
                    ffi::CFMachPortCreateRunLoopSource(ffi::NULL_ALLOCATOR, tap_for_thread, 0);
                if source_for_thread.is_null() {
                    ffi::CFRelease(tap_for_thread);
                    let _ = ready_tx.send(None);
                    return;
                }
                let rl = ffi::CFRunLoopGetCurrent();
                *run_loop_clone.0.lock().unwrap() = Some(rl);
                let mode = ffi::kCFRunLoopDefaultMode.0;
                ffi::CFRunLoopAddSource(rl, source_for_thread, mode);
                ffi::CGEventTapEnable(tap_for_thread, 1);
                let _ = ready_tx.send(Some((tap_for_thread as usize, source_for_thread as usize)));
                let mut ticks_since_check: u32 = 0;
                let mut diag_logged_enabled: bool = false;
                while !stop_clone.load(Ordering::SeqCst) {
                    ffi::CFRunLoopRunInMode(mode, 0.25, 1);
                    // Periodically re-arm the tap. macOS can disable it
                    // after screen lock/sleep *without* delivering a
                    // kCGSessionEventTapTimeout callback, leaving the tap
                    // silently inert. Checking ~4x/s keeps the hotkey alive
                    // across locks without spamming the API.
                    ticks_since_check = ticks_since_check.wrapping_add(1);
                    if ticks_since_check >= 4 {
                        ticks_since_check = 0;
                        let enabled = ffi::CGEventTapIsEnabled(tap_for_thread);
                        if !diag_logged_enabled {
                            eprintln!(
                                "[hotkey-diag] tap is_enabled={} (created ok)",
                                enabled
                            );
                            diag_logged_enabled = true;
                        }
                        if enabled == 0 {
                            eprintln!("[hotkey-diag] tap disabled by system; re-arming");
                            ffi::CGEventTapEnable(tap_for_thread, 1);
                        }
                    }
                }
                ffi::CGEventTapEnable(tap_for_thread, 0);
                ffi::CFRunLoopRemoveSource(rl, source_for_thread, mode);
            }
        });
        let Some((tap_as_usize, source_as_usize)) =
            ready_rx.recv_timeout(Duration::from_secs(2)).ok().flatten()
        else {
            unsafe { drop(Box::from_raw(state_ptr)) };
            let _ = thread.join();
            return Err(X11Error::DisplayUnavailable);
        };
        let tap = tap_as_usize as ffi::CGEventTapRef;
        let source = source_as_usize as ffi::CFRunLoopSourceRef;

        let handle = TapHandle {
            tap,
            source,
            state_ptr,
            run_loop: run_loop_for_thread,
            stop,
            thread: Some(thread),
        };
        // TapHandle holds raw pointers and is used from a single thread after
        // construction; the Arc is a cheap move-only owner here.
        #[allow(clippy::arc_with_non_send_sync)]
        let tap = Arc::new(handle);
        Ok(Self {
            rx,
            tap,
            key_code,
            modifier_mask: shortcut.modifier_mask,
        })
    }

    pub fn wait(&self, timeout: Duration) -> Option<HotkeyEvent> {
        match self.rx.try_recv() {
            Ok(event) => return Some(event),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        let mut remaining = timeout;
        let slice = Duration::from_millis(10);
        while !remaining.is_zero() {
            let step = remaining.min(slice);
            std::thread::sleep(step);
            remaining = remaining.saturating_sub(step);
            match self.rx.try_recv() {
                Ok(event) => return Some(event),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => {}
            }
        }
        None
    }

    pub fn selftest_push_to_talk(&self) -> Result<(), X11Error> {
        let received = self.exercise_matcher_sequence();
        if received != [HotkeyEvent::Pressed, HotkeyEvent::Released] {
            return Err(X11Error::HotkeySelfTestMismatch { actual: received });
        }
        Ok(())
    }

    fn exercise_matcher_sequence(&self) -> Vec<HotkeyEvent> {
        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let mut state = TapState {
            key_code: self.key_code,
            modifier_mask: self.modifier_mask,
            key_down: false,
            tx,
            tap: std::ptr::null_mut(),
        };
        // Exercise the same regression case as the Linux self-test: the
        // target key is released after the modifier state clears.
        handle_key_event(
            &mut state,
            ffi::kCGEventKeyDown,
            self.key_code,
            self.modifier_mask,
        );
        handle_key_event(&mut state, ffi::kCGEventKeyUp, self.key_code, 0);
        rx.try_iter().collect()
    }
}

impl Drop for TapHandle {
    fn drop(&mut self) {
        // Stop the worker thread. The worker runs `CFRunLoopRunInMode` in a
        // polling loop gated on `stop`, so setting the flag + waking the run
        // loop makes it exit within ~0.25s on its own. We deliberately do
        // NOT call `CFRunLoopStop` here: calling it from a different thread
        // triggers a PAC signature trap inside CoreFoundation
        // (`__CFCheckCFInfoPACSignature` -> EXC_BREAKPOINT), which crashed
        // the daemon on every shutdown under launchd. The stop-flag + wake-up
        // is sufficient and crash-free.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(rl) = *self.run_loop.0.lock().unwrap() {
            unsafe { ffi::CFRunLoopWakeUp(rl) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: the worker has joined (no more run-loop / tap callbacks),
        // so releasing the source + tap and reclaiming TapState is safe.
        unsafe {
            ffi::CFRelease(self.source);
            ffi::CFRelease(self.tap);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Shortcut;

    #[test]
    fn maps_configured_hotkey_targets() {
        assert_eq!(keycode_for_name("F1"), Some(0x7a));
        assert_eq!(keycode_for_name("f12"), Some(0x6f));
        assert_eq!(keycode_for_name("space"), Some(0x31));
        assert_eq!(keycode_for_name("nope"), None);
    }

    #[test]
    fn maps_modifier_flags_to_left_modifier_keycodes() {
        let shortcut = Shortcut::parse("Ctrl+Shift+Alt+Cmd+F1").unwrap();
        assert_eq!(
            modifier_keycodes(shortcut.modifier_mask),
            [0x3b, 0x38, 0x3a, 0x37]
        );
    }

    #[test]
    fn matcher_releases_when_modifier_state_clears_first() {
        let shortcut = Shortcut::parse("Ctrl+F1").unwrap();
        let (tx, rx) = mpsc::channel::<HotkeyEvent>();
        let mut state = TapState {
            key_code: keycode_for_name("F1").unwrap(),
            modifier_mask: shortcut.modifier_mask,
            key_down: false,
            tx,
            tap: std::ptr::null_mut(),
        };

        handle_key_event(
            &mut state,
            ffi::kCGEventKeyDown,
            keycode_for_name("F1").unwrap(),
            shortcut.modifier_mask,
        );
        handle_key_event(
            &mut state,
            ffi::kCGEventKeyUp,
            keycode_for_name("F1").unwrap(),
            0,
        );

        let received: Vec<_> = rx.try_iter().collect();
        assert_eq!(received, [HotkeyEvent::Pressed, HotkeyEvent::Released]);
    }
}

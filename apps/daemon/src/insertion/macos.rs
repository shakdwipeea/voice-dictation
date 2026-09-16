//! macOS insertion: paste first, CGEvent typing as the fallback. Per-character
//! unicode typing is ignored by many Cocoa apps, so the clipboard path is the
//! reliable one here (the reverse of X11).
//!
//! Two safety behaviours live here as well:
//! - The focused element is checked through Accessibility before anything is
//!   typed or pasted. A password field gets [`InsertionOutcome::SecureField`]
//!   and nothing else: no keystrokes, no clipboard.
//! - The user's clipboard is snapshotted before the paste and restored by
//!   [`MacosUi::pump`] once [`CLIPBOARD_RESTORE_DELAY`] has passed, unless a
//!   later write (theirs) moved the pasteboard change count. The restore is
//!   deferred so the insertion report is not inflated by the wait.

use std::time::{Duration, Instant};

use sunoto_desktop::{InsertionOutcome, UiAdapter};

use crate::logging;

/// How long the target app gets to consume the paste before the previous
/// clipboard comes back. Apps read the pasteboard synchronously while
/// handling Cmd+V, so this only needs to cover event delivery.
pub(crate) const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(300);

pub(crate) struct MacosUi {
    pub(crate) adapter: UiAdapter,
    clipboard_restore: bool,
    #[cfg(target_os = "macos")]
    pending_restore: Option<(sunoto_desktop::PasteboardSnapshot, Instant)>,
}

impl MacosUi {
    pub(crate) fn new(adapter: UiAdapter, clipboard_restore: bool) -> Self {
        Self {
            adapter,
            clipboard_restore,
            #[cfg(target_os = "macos")]
            pending_restore: None,
        }
    }

    /// True when keyboard focus is known to be in a password field.
    /// Unknown counts as not secure: refusing every insertion whenever
    /// Accessibility is unreadable would make dictation unusable.
    pub(crate) fn focused_is_secure_field(&self) -> bool {
        self.adapter.focused_is_secure_field() == Some(true)
    }

    pub(crate) fn insert(
        &mut self,
        focus_at_release: Option<String>,
        text: &str,
    ) -> Result<InsertionOutcome, String> {
        if self.focused_is_secure_field() {
            return Ok(InsertionOutcome::SecureField);
        }
        let focus_now = self.adapter.focused_window();
        if let Some(expected) = focus_at_release
            && expected != focus_now.to_string()
        {
            // The user moved on; typing into the new window would put text
            // somewhere they did not dictate it. Park it on the clipboard.
            // Overwriting the clipboard is the feature here, so no restore.
            return self
                .adapter
                .set_clipboard(text)
                .map(|_| InsertionOutcome::ClipboardOnly)
                .map_err(|error| error.to_string());
        }
        let snapshot = self.take_snapshot();
        // Paste via clipboard is the reliable macOS insertion mechanism; direct
        // CGEvent typing is the fallback (works in some apps, ignored in others).
        let outcome = match self.adapter.insert_via_clipboard(text) {
            Ok(()) => Ok(InsertionOutcome::Pasted),
            Err(paste_error) => match self.adapter.insert_direct(text) {
                Ok(()) => Ok(InsertionOutcome::Typed),
                Err(type_error) => {
                    logging::warn(&format!(
                        "macOS paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                    ));
                    Ok(InsertionOutcome::ClipboardOnly)
                }
            },
        };
        if matches!(outcome, Ok(InsertionOutcome::ClipboardOnly)) {
            // The text is the result; leave it there.
            self.drop_snapshot();
        } else {
            self.schedule_restore(snapshot);
        }
        outcome
    }

    /// Run the deferred clipboard restore once its delay has elapsed.
    pub(crate) fn pump(&mut self) {
        self.adapter.pump();
        #[cfg(target_os = "macos")]
        if let Some((_, due)) = &self.pending_restore
            && Instant::now() >= *due
        {
            let (snapshot, _) = self.pending_restore.take().expect("checked above");
            if !self.adapter.restore_clipboard(&snapshot) {
                logging::info("clipboard not restored: it changed after the paste");
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn take_snapshot(&mut self) -> Option<sunoto_desktop::PasteboardSnapshot> {
        if !self.clipboard_restore {
            return None;
        }
        // A restore still pending from the previous session would put stale
        // contents back over this paste; run it now instead.
        if let Some((snapshot, _)) = self.pending_restore.take() {
            let _ = self.adapter.restore_clipboard(&snapshot);
        }
        let snapshot = self.adapter.clipboard_snapshot();
        if snapshot.is_none() {
            logging::warn("clipboard snapshot unavailable; the clipboard will not be restored");
        }
        snapshot
    }

    #[cfg(not(target_os = "macos"))]
    fn take_snapshot(&mut self) -> Option<()> {
        let _ = self.clipboard_restore;
        None
    }

    #[cfg(target_os = "macos")]
    fn schedule_restore(&mut self, snapshot: Option<sunoto_desktop::PasteboardSnapshot>) {
        self.pending_restore =
            snapshot.map(|snapshot| (snapshot, Instant::now() + CLIPBOARD_RESTORE_DELAY));
    }

    #[cfg(not(target_os = "macos"))]
    fn schedule_restore(&mut self, _snapshot: Option<()>) {}

    #[cfg(target_os = "macos")]
    fn drop_snapshot(&mut self) {
        self.pending_restore = None;
    }

    #[cfg(not(target_os = "macos"))]
    fn drop_snapshot(&mut self) {}
}

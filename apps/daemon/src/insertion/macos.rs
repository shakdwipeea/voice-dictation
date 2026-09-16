//! macOS insertion: paste first, CGEvent typing as the fallback. Per-character
//! unicode typing is ignored by many Cocoa apps, so the clipboard path is the
//! reliable one here (the reverse of X11).

use sunoto_desktop::{InsertionOutcome, UiAdapter};

use crate::logging;

pub(crate) fn insert_macos(
    adapter: &mut UiAdapter,
    focus_at_release: Option<String>,
    text: &str,
) -> Result<InsertionOutcome, String> {
    let focus_now = adapter.focused_window();
    if let Some(expected) = focus_at_release
        && expected != focus_now.to_string()
    {
        // The user moved on; typing into the new window would put text
        // somewhere they did not dictate it. Park it on the clipboard.
        return adapter
            .set_clipboard(text)
            .map(|_| InsertionOutcome::ClipboardOnly)
            .map_err(|error| error.to_string());
    }
    // Paste via clipboard is the reliable macOS insertion mechanism; direct
    // CGEvent typing is the fallback (works in some apps, ignored in others).
    match adapter.insert_via_clipboard(text) {
        Ok(()) => Ok(InsertionOutcome::Pasted),
        Err(paste_error) => match adapter.insert_direct(text) {
            Ok(()) => Ok(InsertionOutcome::Typed),
            Err(type_error) => {
                logging::warn(&format!(
                    "macOS paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                ));
                Ok(InsertionOutcome::ClipboardOnly)
            }
        },
    }
}

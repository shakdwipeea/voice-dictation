//! X11 insertion: XTEST typing first, clipboard paste for characters XTEST
//! cannot synthesize. The adapter restores the previous clipboard itself.

use sunoto_desktop::{InsertionOutcome, UiAdapter, X11Error};

pub(crate) fn insert_x11(
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
    match adapter.insert_direct(text) {
        Ok(()) => Ok(InsertionOutcome::Typed),
        Err(X11Error::UnsupportedCharacter(_)) => adapter
            .insert_via_clipboard(text)
            .map(|_| InsertionOutcome::Pasted)
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

//! The pill's Improve button (docs/local-history-plan-2026-10-16.md §6.2):
//! the grammar helper process, and the checks every corrected text must pass
//! before it may replace what was pasted.
//!
//! The helper (services/improve/grammar_sidecar.py) is started on first use
//! or when the pointer reaches the pill's actions, unloads its model when
//! idle, and exits after a few idle minutes; it is started again on demand.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::time::Instant;

use serde_json::json;
use sunoto_desktop::InsertionOutcome;
use sunoto_ipc::{GrammarRequest, SidecarClient};
use sunoto_polish::DictionaryEntry;

use crate::events::DaemonEvent;
use crate::history::HistoryRecorder;
use crate::insertion::{UiCommand, UiReport};
use crate::logging;
use crate::overlay::{UiFront, show_error};
use crate::settings::Settings;

/// More changed words than this share of the original is a rewrite, not a
/// correction, and is refused.
const MAX_CHANGED_FRACTION: f64 = 0.4;

pub(crate) struct GrammarHelper {
    client: Option<SidecarClient>,
    events: Sender<DaemonEvent>,
}

impl GrammarHelper {
    pub(crate) fn new(events: Sender<DaemonEvent>) -> Self {
        Self {
            client: None,
            events,
        }
    }

    fn client(&mut self, settings: &Settings) -> Result<&mut SidecarClient, String> {
        if self.client.is_none() {
            let (python, args) = settings.improve_command();
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let events = self.events.clone();
            let client = SidecarClient::spawn(&python, &arg_refs, move |message| {
                events.send(DaemonEvent::Grammar(message)).is_ok()
            })
            .map_err(|error| format!("cannot start the grammar helper: {error}"))?;
            logging::info("grammar helper started");
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("just started"))
    }

    fn send(&mut self, settings: &Settings, request: &GrammarRequest) -> Result<(), String> {
        let result = self
            .client(settings)?
            .send(request)
            .map_err(|error| format!("grammar helper write failed: {error}"));
        if result.is_err() {
            self.client = None;
        }
        result
    }

    /// Load the model ahead of a likely click. Failures only cost the head start.
    pub(crate) fn warm(&mut self, settings: &Settings) {
        if let Err(error) = self.send(settings, &GrammarRequest::Warm) {
            logging::warn(&error);
        }
    }

    pub(crate) fn improve(
        &mut self,
        settings: &Settings,
        session_id: u64,
        text: &str,
    ) -> Result<(), String> {
        self.send(
            settings,
            &GrammarRequest::Improve {
                session_id,
                text: text.to_string(),
            },
        )
    }

    /// The helper exited (idle, or a crash); the next request starts it again.
    pub(crate) fn exited(&mut self) {
        self.client = None;
    }
}

/// The dictation the pill's buttons act on: the last one pasted.
struct LastPaste {
    session_id: u64,
    text: String,
    /// Improve's checked answer, waiting for Replace or Cancel.
    improved: Option<String>,
}

/// A `sunoto-daemon improve` waiting for its answer.
struct ControlRequest {
    session_id: u64,
    accept: bool,
    response: UnixStream,
}

/// The pill's Improve and Fix buttons, driven from the event loop. Every
/// method is cheap and never blocks: the helper and the UI thread answer
/// later through daemon events.
pub(crate) struct PillActions {
    grammar: GrammarHelper,
    last: Option<LastPaste>,
    control: Option<ControlRequest>,
}

impl PillActions {
    pub(crate) fn new(events: Sender<DaemonEvent>) -> Self {
        Self {
            grammar: GrammarHelper::new(events),
            last: None,
            control: None,
        }
    }

    fn improve_available(settings: &Settings) -> bool {
        settings.improve_enabled && cfg!(target_os = "macos")
    }

    fn last(&mut self, session_id: u64) -> Option<&mut LastPaste> {
        self.last
            .as_mut()
            .filter(|last| last.session_id == session_id)
    }

    /// A dictation was pasted: offer the buttons for it.
    pub(crate) fn pasted(
        &mut self,
        session_id: u64,
        text: &str,
        settings: &Settings,
        ui: &UiFront,
        history: &HistoryRecorder,
    ) {
        self.last = Some(LastPaste {
            session_id,
            text: text.to_string(),
            improved: None,
        });
        ui.actions(
            session_id,
            text,
            Self::improve_available(settings),
            settings.fix_enabled && history.can_note(session_id),
        );
    }

    /// A new dictation started: the old one's buttons go away.
    pub(crate) fn new_dictation(&mut self, ui: &UiFront) {
        if self.last.take().is_some() {
            ui.dismiss_actions();
        }
    }

    pub(crate) fn hovered(&mut self, session_id: u64, settings: &Settings) {
        if Self::improve_available(settings) && self.last(session_id).is_some() {
            self.grammar.warm(settings);
        }
    }

    pub(crate) fn requested(&mut self, session_id: u64, settings: &Settings, ui: &UiFront) {
        if !Self::improve_available(settings) {
            ui.improve_message(session_id, "Improve is turned off");
            self.answer_control(
                session_id,
                json!({"ok": false, "error": "Improve is turned off (improve_enabled in the config)"}),
            );
            return;
        }
        let Some(text) = self.last(session_id).map(|last| last.text.clone()) else {
            ui.improve_message(session_id, "Nothing to improve");
            self.answer_control(
                session_id,
                json!({"ok": false, "error": "nothing to improve"}),
            );
            return;
        };
        logging::info(&format!("session {session_id}: improve requested"));
        if let Err(error) = self.grammar.improve(settings, session_id, &text) {
            logging::warn(&error);
            ui.improve_message(session_id, "Improve isn't available");
            self.answer_control(session_id, json!({"ok": false, "error": error}));
        }
    }

    /// The helper answered: check it, then preview it or say why not.
    pub(crate) fn improved(
        &mut self,
        session_id: u64,
        text: &str,
        ms: u64,
        settings: &Settings,
        ui: &UiFront,
        ui_tx: &Sender<UiCommand>,
    ) {
        let Some(last) = self.last(session_id) else {
            return;
        };
        let original = last.text.clone();
        let verdict = check(&original, text, &settings.polish.dictionary);
        logging::info(&format!(
            "session {session_id}: improve answered in {ms}ms: {verdict:?}"
        ));
        match &verdict {
            Verdict::Replace(improved) => {
                last.improved = Some(improved.clone());
                let accept = self
                    .control
                    .as_ref()
                    .is_some_and(|control| control.session_id == session_id && control.accept);
                if accept {
                    self.accepted(session_id, ui_tx);
                    return;
                }
                ui.improve_preview(session_id, &original, improved);
            }
            Verdict::Unchanged => ui.improve_message(session_id, "Looks right already"),
            Verdict::Refused(_) => ui.improve_message(session_id, "Couldn't improve this safely"),
        }
        let (name, improved, reason) = match &verdict {
            Verdict::Replace(text) => ("preview", Some(text.as_str()), None),
            Verdict::Unchanged => ("unchanged", None, None),
            Verdict::Refused(reason) => ("refused", None, Some(*reason)),
        };
        self.answer_control(
            session_id,
            json!({"ok": true, "verdict": name, "original": original, "improved": improved, "reason": reason, "ms": ms}),
        );
    }

    /// Replace was clicked: hand the swap to the UI thread.
    pub(crate) fn accepted(&mut self, session_id: u64, ui_tx: &Sender<UiCommand>) {
        let Some(last) = self.last(session_id) else {
            return;
        };
        let Some(new) = last.improved.clone() else {
            return;
        };
        let _ = ui_tx.send(UiCommand::Replace {
            session_id,
            old: last.text.clone(),
            new,
        });
    }

    pub(crate) fn cancelled(&mut self, session_id: u64, history: &mut HistoryRecorder) {
        if let Some(improved) = self.last(session_id).and_then(|last| last.improved.take()) {
            history.improve(session_id, &improved, false);
        }
    }

    pub(crate) fn fix_saved(&mut self, session_id: u64, text: &str, history: &mut HistoryRecorder) {
        let saved = history.correction(session_id, text);
        logging::info(&format!(
            "session {session_id}: fix {}",
            if saved {
                "saved to history"
            } else {
                "not saved (history off or text withheld)"
            }
        ));
    }

    /// The UI thread finished a Replace.
    pub(crate) fn replaced(
        &mut self,
        report: UiReport,
        ui: &UiFront,
        history: &mut HistoryRecorder,
        bubble_hide_at: &mut Option<Instant>,
    ) {
        let session_id = report.session_id;
        let Some(last) = self.last(session_id) else {
            return;
        };
        let Some(new) = last.improved.take() else {
            return;
        };
        let outcome = match &report.result {
            Ok(InsertionOutcome::Pasted | InsertionOutcome::Typed) => {
                // The next Improve or Fix works on what is on screen now.
                last.text = new.clone();
                history.improve(session_id, &new, true);
                ui.pasted();
                "replaced"
            }
            Ok(InsertionOutcome::ClipboardOnly) => {
                history.improve(session_id, &new, true);
                show_error(ui, "Improved text copied: press ⌘V", bubble_hide_at);
                "clipboard_only"
            }
            Ok(InsertionOutcome::SecureField) => {
                show_error(ui, "password field: nothing replaced", bubble_hide_at);
                "secure_field"
            }
            Err(error) => {
                logging::warn(&format!("session {session_id}: replace failed: {error}"));
                show_error(ui, "Couldn't replace the text", bubble_hide_at);
                "failed"
            }
        };
        logging::info(&format!(
            "session {session_id}: improve {outcome} in {}ms",
            report.insert_duration.as_millis()
        ));
        self.answer_control(
            session_id,
            json!({"ok": true, "verdict": "preview", "replaced": outcome, "text": new}),
        );
    }

    pub(crate) fn helper_failed(&mut self, session_id: Option<u64>, message: &str, ui: &UiFront) {
        logging::warn(&format!("grammar helper error: {message}"));
        if let Some(session_id) = session_id.or(self.last.as_ref().map(|last| last.session_id)) {
            ui.improve_message(session_id, "Improve failed");
            self.answer_control(session_id, json!({"ok": false, "error": message}));
        }
    }

    pub(crate) fn helper_exited(&mut self) {
        self.grammar.exited();
    }

    /// `sunoto-daemon improve [--accept]`.
    pub(crate) fn control(
        &mut self,
        accept: bool,
        mut response: UnixStream,
        settings: &Settings,
        ui: &UiFront,
    ) {
        let Some(session_id) = self.last.as_ref().map(|last| last.session_id) else {
            reply(
                &mut response,
                json!({"ok": false, "error": "nothing was pasted recently"}),
            );
            return;
        };
        if let Some(mut stale) = self.control.take() {
            reply(
                &mut stale.response,
                json!({"ok": false, "error": "superseded"}),
            );
        }
        self.control = Some(ControlRequest {
            session_id,
            accept,
            response,
        });
        self.requested(session_id, settings, ui);
    }

    fn answer_control(&mut self, session_id: u64, payload: serde_json::Value) {
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.session_id == session_id)
            && let Some(mut control) = self.control.take()
        {
            reply(&mut control.response, payload);
        }
    }
}

fn reply(response: &mut UnixStream, payload: serde_json::Value) {
    if serde_json::to_writer(&mut *response, &payload).is_ok() {
        let _ = response.write_all(b"\n");
    }
}

/// What to do with the helper's answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Offer this text in the preview.
    Replace(String),
    /// Nothing to change.
    Unchanged,
    /// The answer failed a check; nothing is offered.
    Refused(&'static str),
}

/// Check the helper's answer against what was pasted: restore the user's
/// word-list terms the model altered, then refuse anything that changes
/// numbers, code, names or links, or rewrites too much.
pub(crate) fn check(before: &str, after: &str, dictionary: &[DictionaryEntry]) -> Verdict {
    let Some(after) = restore_word_list(before, after, dictionary) else {
        return Verdict::Refused("a word-list term was dropped");
    };
    if after.trim() == before.trim() {
        return Verdict::Unchanged;
    }
    if digit_runs(before) != digit_runs(&after) {
        return Verdict::Refused("numbers changed");
    }
    if protected_tokens(before) != protected_tokens(&after) {
        return Verdict::Refused("code, a name or a link changed");
    }
    if changed_fraction(before, &after) > MAX_CHANGED_FRACTION {
        return Verdict::Refused("too much rewritten");
    }
    Verdict::Replace(after)
}

fn digit_runs(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .collect()
}

/// Words a grammar model must not touch: anything with a digit, a symbol,
/// a leading dash, or a capital after the first letter (agents.md,
/// --workspace, macOS, PyTorch, https://...). Same rule as
/// tools/improve/grammar_probe.py, which was tested on the model.
fn protected_tokens(text: &str) -> Vec<&str> {
    const SYMBOLS: &str = "./@:_\\#$%&*=+<>[]{}|~";
    text.split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| ".,!?;:\"'()".contains(c)))
        .filter(|token| {
            !token.is_empty()
                && (token
                    .chars()
                    .any(|c| c.is_ascii_digit() || SYMBOLS.contains(c))
                    || token.starts_with('-')
                    || token.chars().skip(1).any(char::is_uppercase))
        })
        .collect()
}

/// Share of the original words that did not survive, by longest common
/// subsequence on lower-cased words.
fn changed_fraction(before: &str, after: &str) -> f64 {
    let a: Vec<String> = before.split_whitespace().map(str::to_lowercase).collect();
    let b: Vec<String> = after.split_whitespace().map(str::to_lowercase).collect();
    if a.is_empty() {
        return 0.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for word in &a {
        let mut diagonal = 0;
        for (j, other) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if word == other {
                diagonal + 1
            } else {
                row[j + 1].max(row[j])
            };
            diagonal = above;
        }
    }
    1.0 - row[b.len()] as f64 / a.len() as f64
}

/// Put back the exact spelling of word-list terms ("PyTorch", "Priya") that
/// were in the pasted text but came back with other capitals. `None` when
/// such a term disappeared altogether.
fn restore_word_list(before: &str, after: &str, dictionary: &[DictionaryEntry]) -> Option<String> {
    let mut after = after.to_string();
    for entry in dictionary {
        let term = entry.written.trim();
        if term.is_empty() || find_word(before, term, false).is_none() {
            continue;
        }
        let mut start = 0;
        let mut found = false;
        while let Some(at) = find_word(&after[start..], term, true).map(|at| at + start) {
            found = true;
            after.replace_range(at..at + term.len(), term);
            start = at + term.len();
        }
        if !found {
            return None;
        }
    }
    Some(after)
}

/// Byte offset of `term` as a whole word in `text`, optionally ignoring
/// ASCII case (which keeps byte offsets valid).
fn find_word(text: &str, term: &str, ignore_case: bool) -> Option<usize> {
    let (haystack, needle) = if ignore_case {
        (text.to_ascii_lowercase(), term.to_ascii_lowercase())
    } else {
        (text.to_string(), term.to_string())
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '\'';
    let mut from = 0;
    while let Some(found) = haystack[from..].find(&needle) {
        let at = from + found;
        let end = at + needle.len();
        let starts = haystack[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word(c));
        let ends = haystack[end..].chars().next().is_none_or(|c| !is_word(c));
        if starts && ends {
            return Some(at);
        }
        from = at + needle.len().max(1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(spoken: &str, written: &str) -> DictionaryEntry {
        DictionaryEntry {
            spoken: spoken.into(),
            written: written.into(),
        }
    }

    #[test]
    fn grammar_fixes_are_offered() {
        for (before, after) in [
            (
                "She dont like the new design.",
                "She doesn't like the new design.",
            ),
            ("I has finished the report.", "I have finished the report."),
            ("Please send me a email.", "Please send me an email."),
        ] {
            assert_eq!(check(before, after, &[]), Verdict::Replace(after.into()));
        }
        assert_eq!(check("All good.", "All good.", &[]), Verdict::Unchanged);
    }

    #[test]
    fn the_tested_model_failures_are_refused() {
        // Real outputs of gec-t5_small on the test set (tools/improve).
        for (before, after) in [
            (
                "Update agents.md and push the fix to GitHub.",
                "Update agents. md and push the fix to GitHub.",
            ),
            (
                "Run cargo test --workspace before you push.",
                "Run a cargo test -- workspace before you push.",
            ),
            (
                "Version 0.1.12 fixes the overlay on macOS 26.",
                "Version 0.1.12 fixes the overlay on MacOS 26.",
            ),
            (
                "Open https://example.com/docs/setup in the browser.",
                "Open https:// example.com / docs / setup in the browser.",
            ),
            (
                "Call me at 555-0142 after 6 pm.",
                "Call me at 555-0124 after 6 pm.",
            ),
        ] {
            assert!(
                matches!(check(before, after, &[]), Verdict::Refused(_)),
                "{after}"
            );
        }
    }

    #[test]
    fn big_rewrites_are_refused() {
        assert_eq!(
            check(
                "Meet me at the cafe.",
                "Let us all gather somewhere nice together.",
                &[]
            ),
            Verdict::Refused("too much rewritten")
        );
        assert!(changed_fraction("a b c d e", "a b c d x") <= MAX_CHANGED_FRACTION);
    }

    #[test]
    fn word_list_terms_keep_their_spelling() {
        let words = [word("pie torch", "PyTorch"), word("priya", "Priya")];
        assert_eq!(
            check(
                "I trained it in PyTorch with Priya yesterday",
                "I trained it in pytorch with priya yesterday.",
                &words
            ),
            Verdict::Replace("I trained it in PyTorch with Priya yesterday.".into())
        );
        assert_eq!(
            check("Ask Priya about it.", "Ask about it.", &words),
            Verdict::Refused("a word-list term was dropped")
        );
        // Terms the dictation did not contain are left alone.
        assert_eq!(
            check("She dont know.", "She doesn't know.", &words),
            Verdict::Replace("She doesn't know.".into())
        );
    }

    #[test]
    fn whole_words_only() {
        assert_eq!(find_word("Priyanka called", "Priya", true), None);
        assert_eq!(find_word("Call priya now", "Priya", true), Some(5));
        assert_eq!(digit_runs("$4,250 on 3/6"), ["4", "250", "3", "6"]);
        assert_eq!(
            protected_tokens("Push agents.md to GitHub, then rest."),
            ["agents.md", "GitHub"]
        );
    }
}

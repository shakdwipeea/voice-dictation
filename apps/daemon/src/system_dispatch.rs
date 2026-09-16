//! System-mode glue owned by the loop: the one-time selection set awaiting
//! a palette choice, and the navigation confirmation that precedes a URL or
//! web-search open. Raw transcripts never reach an executor from here; only
//! a typed, revalidated action does.

use std::time::Instant;

use sunoto_ipc::OverlaySuggestion;
use sunoto_system::{CapabilityInput, PendingSuggestionSet, SuggestionAction, ValidatedHttpUrl};

use crate::logging;
use crate::overlay::{UiFront, show_error};

pub(crate) enum PendingSystemSelection {
    Targets(PendingSuggestionSet),
    BrowserNavigation {
        pending: PendingSuggestionSet,
        url: ValidatedHttpUrl,
    },
    Navigation {
        session_id: u64,
        input: CapabilityInput,
        title: String,
    },
}

pub(crate) enum SelectedSystemAction {
    Target(sunoto_system::ResolvedSystemAction),
    Navigation {
        input: CapabilityInput,
        title: String,
    },
    BrowserNavigation {
        action: sunoto_system::ResolvedSystemAction,
        url: ValidatedHttpUrl,
    },
}

impl PendingSystemSelection {
    pub(crate) fn session_id(&self) -> u64 {
        match self {
            Self::Targets(pending) => pending.session_id(),
            Self::BrowserNavigation { pending, .. } => pending.session_id(),
            Self::Navigation { session_id, .. } => *session_id,
        }
    }

    pub(crate) fn select(
        &mut self,
        session_id: u64,
        suggestion_id: &str,
    ) -> Result<SelectedSystemAction, String> {
        match self {
            Self::Targets(pending) => pending
                .select(session_id, suggestion_id)
                .map(SelectedSystemAction::Target)
                .map_err(|error| error.to_string()),
            Self::BrowserNavigation { pending, url } => pending
                .select(session_id, suggestion_id)
                .map(|action| SelectedSystemAction::BrowserNavigation {
                    action,
                    url: url.clone(),
                })
                .map_err(|error| error.to_string()),
            Self::Navigation {
                session_id: active_session,
                input,
                title,
            } if *active_session == session_id && suggestion_id == "system-navigation-confirm" => {
                Ok(SelectedSystemAction::Navigation {
                    input: input.clone(),
                    title: title.clone(),
                })
            }
            Self::Navigation { .. } => Err("stale or invented System navigation selection".into()),
        }
    }
}

pub(crate) fn present_navigation_confirmation(
    navigation: Result<(CapabilityInput, String), String>,
    session_id: u64,
    transcript: &str,
    ui: &UiFront,
    pending_system: &mut Option<PendingSystemSelection>,
    bubble_hide_at: &mut Option<Instant>,
) {
    let (input, title) = match navigation {
        Ok(navigation) => navigation,
        Err(error) => {
            logging::warn(&format!(
                "session {session_id}: invalid System URL: {error}"
            ));
            show_error(
                ui,
                "That is not a supported HTTP or HTTPS URL",
                bubble_hide_at,
            );
            return;
        }
    };
    let suggestions = vec![OverlaySuggestion {
        suggestion_id: "system-navigation-confirm".into(),
        title: title.clone(),
        subtitle: Some("Opens in the default browser after selection".into()),
        action_label: "Open".into(),
    }];
    ui.hide();
    if ui.show_system_palette(session_id, transcript.into(), suggestions) {
        *pending_system = Some(PendingSystemSelection::Navigation {
            session_id,
            input,
            title,
        });
    } else {
        show_error(ui, "System suggestions UI is unavailable", bubble_hide_at);
    }
}

pub(crate) fn suggestion_action_label(action: SuggestionAction) -> &'static str {
    match action {
        SuggestionAction::OpenApplication
        | SuggestionAction::OpenFile
        | SuggestionAction::OpenFolder
        | SuggestionAction::OpenProject
        | SuggestionAction::OpenUrl => "Open",
        SuggestionAction::RevealFile => "Reveal",
        SuggestionAction::SearchWeb => "Search",
    }
}

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    ActionCandidate, ResolvedSystemAction, SuggestionAction, SystemIntent, rank_suggestions,
};

/// A command-palette row safe to send to an untrusted UI process.
///
/// `suggestion_id` is scoped to one System session. Platform identifiers and
/// paths remain in `PendingSuggestionSet` inside the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectableSuggestion {
    pub suggestion_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub action: SuggestionAction,
    pub score: u16,
}

/// One outstanding suggestion set with replay protection.
pub struct PendingSuggestionSet {
    session_id: u64,
    suggestions: Vec<SelectableSuggestion>,
    actions: HashMap<String, ResolvedSystemAction>,
    consumed: bool,
}

impl PendingSuggestionSet {
    pub fn build(
        session_id: u64,
        intent: &SystemIntent,
        candidates: Vec<ActionCandidate>,
        limit: usize,
    ) -> Self {
        let actions_by_target = candidates
            .iter()
            .map(|record| (record.candidate.stable_id.clone(), record.action.clone()))
            .collect::<HashMap<_, _>>();
        let ranked = rank_suggestions(
            intent,
            candidates.into_iter().map(|record| record.candidate),
            limit,
        );

        let mut actions = HashMap::new();
        let suggestions = ranked
            .into_iter()
            .enumerate()
            .filter_map(|(index, suggestion)| {
                let action = actions_by_target.get(&suggestion.stable_id)?.clone();
                let suggestion_id = format!("session-{session_id}:suggestion-{}", index + 1);
                actions.insert(suggestion_id.clone(), action);
                Some(SelectableSuggestion {
                    suggestion_id,
                    title: suggestion.title,
                    subtitle: suggestion.subtitle,
                    action: suggestion.action,
                    score: suggestion.score,
                })
            })
            .collect();

        Self {
            session_id,
            suggestions,
            actions,
            consumed: false,
        }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn suggestions(&self) -> &[SelectableSuggestion] {
        &self.suggestions
    }

    pub fn select(
        &mut self,
        session_id: u64,
        suggestion_id: &str,
    ) -> Result<ResolvedSystemAction, SelectionError> {
        if session_id != self.session_id {
            return Err(SelectionError::StaleSession);
        }
        if self.consumed {
            return Err(SelectionError::AlreadyConsumed);
        }
        let action = self
            .actions
            .remove(suggestion_id)
            .ok_or(SelectionError::UnknownSuggestion)?;
        self.consumed = true;
        self.actions.clear();
        Ok(action)
    }

    pub fn cancel(&mut self, session_id: u64) -> Result<(), SelectionError> {
        if session_id != self.session_id {
            return Err(SelectionError::StaleSession);
        }
        if self.consumed {
            return Err(SelectionError::AlreadyConsumed);
        }
        self.consumed = true;
        self.actions.clear();
        Ok(())
    }

    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionError {
    StaleSession,
    UnknownSuggestion,
    AlreadyConsumed,
}

impl fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StaleSession => "selection belongs to a stale System session",
            Self::UnknownSuggestion => "selection is not in the current suggestion set",
            Self::AlreadyConsumed => "suggestion set has already been consumed",
        })
    }
}

impl std::error::Error for SelectionError {}

#[cfg(test)]
mod tests {
    use crate::{ApplicationId, CandidateKind, ResolvedCandidate, SystemIntent, TargetHint};

    use super::*;

    fn candidate(id: &str, name: &str, alias: &str) -> ActionCandidate {
        ActionCandidate {
            candidate: ResolvedCandidate {
                stable_id: id.into(),
                display_name: name.into(),
                subtitle: Some("Application".into()),
                kind: CandidateKind::Application,
                aliases: vec![alias.into()],
                recency_score: 0,
            },
            action: ResolvedSystemAction::LaunchApplication {
                application: ApplicationId::new(id).unwrap(),
                display_name: name.into(),
            },
        }
    }

    fn pending() -> PendingSuggestionSet {
        PendingSuggestionSet::build(
            7,
            &SystemIntent::OpenTarget {
                query: "chrome".into(),
                hint: TargetHint::Application,
            },
            vec![candidate("chrome", "Google Chrome", "chrome")],
            5,
        )
    }

    #[test]
    fn explicit_selection_yields_one_typed_action() {
        let mut pending = pending();
        let suggestion = pending.suggestions()[0].suggestion_id.clone();
        assert!(matches!(
            pending.select(7, &suggestion),
            Ok(ResolvedSystemAction::LaunchApplication { .. })
        ));
        assert_eq!(
            pending.select(7, &suggestion),
            Err(SelectionError::AlreadyConsumed)
        );
    }

    #[test]
    fn stale_and_invented_selections_never_produce_an_action() {
        let mut pending = pending();
        assert_eq!(
            pending.select(6, "session-7:suggestion-1"),
            Err(SelectionError::StaleSession)
        );
        assert_eq!(
            pending.select(7, "session-7:suggestion-99"),
            Err(SelectionError::UnknownSuggestion)
        );
        assert!(!pending.is_consumed());
    }

    #[test]
    fn serialized_rows_never_expose_native_target_ids() {
        let pending = PendingSuggestionSet::build(
            9,
            &SystemIntent::OpenTarget {
                query: "browser".into(),
                hint: TargetHint::Application,
            },
            vec![candidate(
                "native-secret:/Applications/Browser.app",
                "Browser",
                "browser",
            )],
            5,
        );
        let json = serde_json::to_string(pending.suggestions()).unwrap();
        assert!(!json.contains("native-secret"));
        assert!(!json.contains("/Applications"));
        assert!(json.contains("session-9:suggestion-1"));
    }
}

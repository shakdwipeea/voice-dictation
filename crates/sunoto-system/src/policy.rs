use serde::{Deserialize, Serialize};

use crate::SystemIntent;

/// Coarse product risk classes. Version 1 exposes only A/B actions as
/// suggestions; C/D actions do not have intent or executor variants yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    LaunchOrNavigation,
    LocalContentOpen,
}

/// Policy decides whether resolution is allowed to produce selectable rows.
/// It never authorizes execution; a separate current selection event is still
/// required after the target has been resolved and revalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    PresentSuggestions,
}

pub fn policy_for_intent(intent: &SystemIntent) -> (RiskClass, PolicyDecision) {
    let risk = match intent {
        SystemIntent::OpenTarget {
            hint: crate::TargetHint::Auto | crate::TargetHint::File | crate::TargetHint::Folder,
            ..
        }
        | SystemIntent::OpenTargetWithApplication { .. }
        | SystemIntent::OpenFolder { .. }
        | SystemIntent::FindFile { .. }
        | SystemIntent::OpenFileByQuery { .. }
        | SystemIntent::RevealFileByQuery { .. } => RiskClass::LocalContentOpen,
        SystemIntent::OpenTarget { .. }
        | SystemIntent::WebSearch { .. }
        | SystemIntent::OpenUrl { .. }
        | SystemIntent::OpenBrowserAndUrl { .. } => RiskClass::LaunchOrNavigation,
    };
    (risk, PolicyDecision::PresentSuggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_current_intent_is_suggestion_only() {
        let intents = [
            SystemIntent::OpenTarget {
                query: "Chrome".into(),
                hint: crate::TargetHint::Auto,
            },
            SystemIntent::FindFile {
                query: "report".into(),
            },
            SystemIntent::WebSearch {
                query: "Rust".into(),
            },
        ];
        for intent in intents {
            assert_eq!(
                policy_for_intent(&intent).1,
                PolicyDecision::PresentSuggestions
            );
        }
        assert_eq!(
            policy_for_intent(&SystemIntent::OpenTargetWithApplication {
                query: "project".into(),
                application_query: "editor".into(),
            })
            .0,
            RiskClass::LocalContentOpen
        );
    }
}

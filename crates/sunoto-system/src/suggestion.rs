use serde::{Deserialize, Serialize};

use crate::{SystemIntent, TargetHint};

/// A platform-resolved target that is safe to rank but not yet execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCandidate {
    pub stable_id: String,
    pub display_name: String,
    pub subtitle: Option<String>,
    pub kind: CandidateKind,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Small provider-supplied freshness signal (0..=50). Name/alias match
    /// remains dominant; recency only orders otherwise comparable results.
    #[serde(default)]
    pub recency_score: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Application,
    File,
    Folder,
    Project,
    Url,
    WebSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionAction {
    OpenApplication,
    OpenFile,
    RevealFile,
    OpenFolder,
    OpenProject,
    OpenUrl,
    SearchWeb,
}

/// One selectable command-palette row.
///
/// `stable_id` is resolved by a platform provider. The daemon will later wrap
/// it in a per-session, single-use selection token before execution exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSuggestion {
    pub stable_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub action: SuggestionAction,
    pub score: u16,
}

/// Rank already-resolved candidates for a typed intent.
///
/// This function is deliberately deterministic. It may highlight the first
/// row, but it cannot execute it. Candidates of an incompatible type are
/// discarded before ranking.
pub fn rank_suggestions(
    intent: &SystemIntent,
    candidates: impl IntoIterator<Item = ResolvedCandidate>,
    limit: usize,
) -> Vec<SystemSuggestion> {
    let query = normalize(intent.query());
    let mut ranked = candidates
        .into_iter()
        .filter_map(|candidate| suggestion_for(intent, &query, candidate))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.stable_id.cmp(&right.stable_id))
    });
    ranked.truncate(limit);
    ranked
}

fn suggestion_for(
    intent: &SystemIntent,
    query: &str,
    candidate: ResolvedCandidate,
) -> Option<SystemSuggestion> {
    let action = compatible_action(intent, candidate.kind)?;
    let mut score = match_score(query, &candidate.display_name);
    for alias in &candidate.aliases {
        score = score.max(match_score(query, alias));
    }
    score = score.saturating_add(candidate.recency_score.min(50));
    (score > 0).then(|| SystemSuggestion {
        stable_id: candidate.stable_id,
        title: format!("{} {}", action_label(action), candidate.display_name),
        subtitle: candidate.subtitle,
        action,
        score,
    })
}

fn compatible_action(intent: &SystemIntent, kind: CandidateKind) -> Option<SuggestionAction> {
    match (intent, kind) {
        (
            SystemIntent::OpenTarget {
                hint: TargetHint::Application | TargetHint::Auto,
                ..
            },
            CandidateKind::Application,
        ) => Some(SuggestionAction::OpenApplication),
        (
            SystemIntent::OpenTarget {
                hint: TargetHint::File | TargetHint::Auto,
                ..
            }
            | SystemIntent::OpenFileByQuery { .. },
            CandidateKind::File,
        ) => Some(SuggestionAction::OpenFile),
        (
            SystemIntent::FindFile { .. } | SystemIntent::RevealFileByQuery { .. },
            CandidateKind::File,
        ) => Some(SuggestionAction::RevealFile),
        (
            SystemIntent::OpenTarget {
                hint: TargetHint::Folder | TargetHint::Auto,
                ..
            }
            | SystemIntent::OpenFolder { .. },
            CandidateKind::Folder,
        ) => Some(SuggestionAction::OpenFolder),
        (
            SystemIntent::OpenTarget {
                hint: TargetHint::Auto,
                ..
            },
            CandidateKind::Project,
        ) => Some(SuggestionAction::OpenProject),
        (SystemIntent::OpenTargetWithApplication { .. }, CandidateKind::File) => {
            Some(SuggestionAction::OpenFile)
        }
        (SystemIntent::OpenTargetWithApplication { .. }, CandidateKind::Folder) => {
            Some(SuggestionAction::OpenFolder)
        }
        (SystemIntent::OpenTargetWithApplication { .. }, CandidateKind::Project) => {
            Some(SuggestionAction::OpenProject)
        }
        (
            SystemIntent::OpenTarget {
                hint: TargetHint::Url,
                ..
            }
            | SystemIntent::OpenUrl { .. },
            CandidateKind::Url,
        ) => Some(SuggestionAction::OpenUrl),
        (SystemIntent::WebSearch { .. }, CandidateKind::WebSearch) => {
            Some(SuggestionAction::SearchWeb)
        }
        _ => None,
    }
}

fn match_score(query: &str, candidate: &str) -> u16 {
    let candidate = normalize(candidate);
    if query == candidate {
        return 1_000;
    }
    if candidate.starts_with(query) {
        return 800;
    }
    let query_tokens = query.split_whitespace().collect::<Vec<_>>();
    if !query_tokens.is_empty()
        && query_tokens
            .iter()
            .all(|token| candidate.split_whitespace().any(|part| part == *token))
    {
        return 600;
    }
    if candidate.contains(query) {
        return 400;
    }
    0
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn action_label(action: SuggestionAction) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, aliases: &[&str]) -> ResolvedCandidate {
        ResolvedCandidate {
            stable_id: id.into(),
            display_name: name.into(),
            subtitle: Some("Application".into()),
            kind: CandidateKind::Application,
            aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
            recency_score: 0,
        }
    }

    #[test]
    fn exact_alias_ranks_before_prefix_match() {
        let intent = SystemIntent::OpenTarget {
            query: "chrome".into(),
            hint: TargetHint::Application,
        };
        let suggestions = rank_suggestions(
            &intent,
            [
                app("canary", "Chrome Canary", &[]),
                app("chrome", "Google Chrome", &["chrome"]),
            ],
            5,
        );
        assert_eq!(suggestions[0].stable_id, "chrome");
        assert_eq!(suggestions[0].title, "Open Google Chrome");
        assert_eq!(suggestions.len(), 2);
    }

    #[test]
    fn incompatible_candidate_types_are_removed() {
        let intent = SystemIntent::OpenTarget {
            query: "chrome".into(),
            hint: TargetHint::Application,
        };
        let file = ResolvedCandidate {
            stable_id: "file".into(),
            display_name: "Chrome notes.txt".into(),
            subtitle: None,
            kind: CandidateKind::File,
            aliases: vec![],
            recency_score: 0,
        };
        assert!(rank_suggestions(&intent, [file], 5).is_empty());
    }

    #[test]
    fn ranking_never_discards_ambiguous_exact_candidates() {
        let intent = SystemIntent::OpenTarget {
            query: "notes".into(),
            hint: TargetHint::Auto,
        };
        let suggestions = rank_suggestions(
            &intent,
            [app("notes-a", "Notes", &[]), app("notes-b", "Notes", &[])],
            5,
        );
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].score, 1_000);
        assert_eq!(suggestions[1].score, 1_000);
    }

    #[test]
    fn exact_cross_kind_matches_remain_visible_and_recency_breaks_only_ties() {
        let intent = SystemIntent::OpenTarget {
            query: "atlas".into(),
            hint: TargetHint::Auto,
        };
        let mut older_project = ResolvedCandidate {
            stable_id: "project-old".into(),
            display_name: "atlas".into(),
            subtitle: Some("Project · workspace".into()),
            kind: CandidateKind::Project,
            aliases: Vec::new(),
            recency_score: 5,
        };
        let newer_folder = ResolvedCandidate {
            stable_id: "folder-new".into(),
            display_name: "atlas".into(),
            subtitle: Some("Folder · Documents".into()),
            kind: CandidateKind::Folder,
            aliases: Vec::new(),
            recency_score: 50,
        };
        let ranked = rank_suggestions(&intent, [older_project.clone(), newer_folder], 5);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].stable_id, "folder-new");
        older_project.display_name = "atlas archive".into();
        older_project.recency_score = 50;
        let exact = app("app-atlas", "atlas", &[]);
        let ranked = rank_suggestions(&intent, [older_project, exact], 5);
        assert_eq!(ranked[0].stable_id, "app-atlas");
    }

    #[test]
    fn unrelated_query_returns_no_match() {
        let intent = SystemIntent::OpenTarget {
            query: "definitely missing".into(),
            hint: TargetHint::Auto,
        };
        assert!(rank_suggestions(&intent, [app("notes", "Notes", &[])], 5).is_empty());
    }

    #[test]
    fn every_voice_spotlight_target_kind_maps_to_a_palette_action() {
        let candidate = |kind| ResolvedCandidate {
            stable_id: format!("{kind:?}"),
            display_name: "target".into(),
            subtitle: Some(format!("{kind:?} evidence")),
            kind,
            aliases: Vec::new(),
            recency_score: 0,
        };
        let cases = [
            (
                SystemIntent::OpenTarget {
                    query: "target".into(),
                    hint: TargetHint::Auto,
                },
                CandidateKind::Application,
                SuggestionAction::OpenApplication,
            ),
            (
                SystemIntent::OpenFileByQuery {
                    query: "target".into(),
                },
                CandidateKind::File,
                SuggestionAction::OpenFile,
            ),
            (
                SystemIntent::OpenFolder {
                    query: "target".into(),
                },
                CandidateKind::Folder,
                SuggestionAction::OpenFolder,
            ),
            (
                SystemIntent::OpenTarget {
                    query: "target".into(),
                    hint: TargetHint::Auto,
                },
                CandidateKind::Project,
                SuggestionAction::OpenProject,
            ),
            (
                SystemIntent::OpenUrl {
                    spoken_url: "target".into(),
                },
                CandidateKind::Url,
                SuggestionAction::OpenUrl,
            ),
            (
                SystemIntent::WebSearch {
                    query: "target".into(),
                },
                CandidateKind::WebSearch,
                SuggestionAction::SearchWeb,
            ),
        ];
        for (intent, kind, expected) in cases {
            let ranked = rank_suggestions(&intent, [candidate(kind)], 1);
            assert_eq!(ranked[0].action, expected);
        }
    }

    #[test]
    fn json_contains_no_executable_command_field() {
        let suggestion = SystemSuggestion {
            stable_id: "app:chrome".into(),
            title: "Open Google Chrome".into(),
            subtitle: None,
            action: SuggestionAction::OpenApplication,
            score: 1_000,
        };
        let json = serde_json::to_string(&suggestion).unwrap();
        assert!(!json.contains("command"));
        assert!(!json.contains("args"));
    }
}

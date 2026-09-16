//! Harmless Phase 1 fixtures for exercising the full selection contract.

use crate::{
    ActionCandidate, ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationId,
    ApplicationResolver, CandidateKind, LocalTargetId, LocalTargetKind, ResolvedCandidate,
    ResolvedSystemAction, SystemIntent, SystemOperationError, application_aliases,
};

#[derive(Default)]
pub struct FixtureApplicationResolver;

impl ApplicationResolver for FixtureApplicationResolver {
    fn application_candidates(&mut self) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        [
            ("fixture:chrome", "Google Chrome"),
            ("fixture:music", "Music"),
            ("fixture:vscode", "Visual Studio Code"),
        ]
        .into_iter()
        .map(|(id, name)| {
            Ok(ActionCandidate {
                candidate: ResolvedCandidate {
                    stable_id: id.into(),
                    display_name: name.into(),
                    subtitle: Some("Fixture application".into()),
                    kind: CandidateKind::Application,
                    aliases: application_aliases(name),
                    recency_score: 0,
                },
                action: ResolvedSystemAction::LaunchApplication {
                    application: ApplicationId::new(id)?,
                    display_name: name.into(),
                },
            })
        })
        .collect()
    }

    fn target_candidates(
        &mut self,
        intent: &SystemIntent,
    ) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        let mut candidates = self.application_candidates()?;
        let selected_editor = if let SystemIntent::OpenTargetWithApplication {
            application_query,
            ..
        } = intent
        {
            self.application_candidates()?
                .into_iter()
                .find_map(|candidate| {
                    candidate
                        .candidate
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(application_query))
                        .then_some(candidate.action)
                })
        } else {
            None
        };
        let (application, application_display_name) = match selected_editor {
            Some(ResolvedSystemAction::LaunchApplication {
                application,
                display_name,
            }) => (Some(application), Some(display_name)),
            _ => (None, None),
        };
        let reveal = matches!(
            intent,
            SystemIntent::FindFile { .. } | SystemIntent::RevealFileByQuery { .. }
        );
        for (id, name, kind) in [
            (
                "fixture:project",
                "who-else-is-free",
                LocalTargetKind::Project,
            ),
            (
                "fixture:folder",
                "who-else-is-free notes",
                LocalTargetKind::Folder,
            ),
            ("fixture:file", "who-else-is-free.md", LocalTargetKind::File),
        ] {
            let target = LocalTargetId::new(id)?;
            candidates.push(ActionCandidate {
                candidate: ResolvedCandidate {
                    stable_id: id.into(),
                    display_name: name.into(),
                    subtitle: Some(format!("Fixture {:?}; approved root", kind)),
                    kind: match kind {
                        LocalTargetKind::File => CandidateKind::File,
                        LocalTargetKind::Folder => CandidateKind::Folder,
                        LocalTargetKind::Project => CandidateKind::Project,
                    },
                    aliases: Vec::new(),
                    recency_score: 25,
                },
                action: ResolvedSystemAction::OpenLocalTarget {
                    target,
                    display_name: name.into(),
                    kind,
                    reveal,
                    application: application.clone(),
                    application_display_name: application_display_name.clone(),
                },
            });
        }
        Ok(candidates)
    }
}

#[derive(Default)]
pub struct RecordingExecutor {
    actions: Vec<ResolvedSystemAction>,
}

impl RecordingExecutor {
    pub fn actions(&self) -> &[ResolvedSystemAction] {
        &self.actions
    }
}

impl ActionExecutor for RecordingExecutor {
    fn execute(
        &mut self,
        action: &ResolvedSystemAction,
    ) -> Result<ActionResult, SystemOperationError> {
        self.actions.push(action.clone());
        let evidence = match action {
            ResolvedSystemAction::LaunchApplication { .. } => {
                ActionSuccessEvidence::ApplicationRunning
            }
            ResolvedSystemAction::OpenLocalTarget { reveal: true, .. } => {
                ActionSuccessEvidence::LocalTargetRevealed
            }
            ResolvedSystemAction::OpenLocalTarget { .. } => {
                ActionSuccessEvidence::LocalTargetOpened
            }
        };
        Ok(ActionResult {
            display_name: action.display_name().into(),
            detail: "recorded fixture action".into(),
            // A recording executor is the test double for the executor's
            // verified-success observation; it never touches the desktop.
            evidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{PendingSuggestionSet, SystemIntent, TargetHint};

    use super::*;

    #[test]
    fn open_chrome_requires_selection_before_the_fixture_executor_runs() {
        let mut resolver = FixtureApplicationResolver;
        let candidates = resolver.application_candidates().unwrap();
        let intent = SystemIntent::OpenTarget {
            query: "Chrome".into(),
            hint: TargetHint::Application,
        };
        let mut pending = PendingSuggestionSet::build(12, &intent, candidates, 5);
        let mut executor = RecordingExecutor::default();

        assert!(executor.actions().is_empty());
        assert_eq!(pending.suggestions()[0].title, "Open Google Chrome");
        let suggestion_id = pending.suggestions()[0].suggestion_id.clone();
        let action = pending.select(12, &suggestion_id).unwrap();
        executor.execute(&action).unwrap();
        assert_eq!(executor.actions(), &[action]);
    }
}

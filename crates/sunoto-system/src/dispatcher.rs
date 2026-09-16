//! Native capability dispatcher. It is the only bridge from a validated
//! capability call to a platform resolver/executor.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{
    ActionExecutor, ActionSuccessEvidence, ApplicationResolver, CapabilityCatalog, CapabilityId,
    CapabilityInput, CapabilityOutput, NativeCapabilityCall, ObservationEvidence,
    ResolvedSystemAction, ResolvedTargetId, SystemIntent, TargetHint, TypedObservation,
    rank_suggestions,
};

/// Cancellation and deadline checks used around every native boundary.
/// Native dispatchers must check this again immediately before a side effect.
pub trait DispatchGuard {
    fn is_cancelled(&self) -> bool;
    fn deadline_exceeded(&self) -> bool;
}

struct UnboundedDispatch;

impl DispatchGuard for UnboundedDispatch {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn deadline_exceeded(&self) -> bool {
        false
    }
}

pub trait CapabilityDispatcher {
    fn catalog(&self) -> CapabilityCatalog;
    fn dispatch(&mut self, input: &CapabilityInput) -> CapabilityOutput;

    fn dispatch_guarded(
        &mut self,
        input: &CapabilityInput,
        guard: &dyn DispatchGuard,
    ) -> CapabilityOutput {
        if guard.is_cancelled() {
            return TypedObservation::cancelled(input.capability_id());
        }
        if guard.deadline_exceeded() {
            return TypedObservation::failed(input.capability_id(), "capability deadline exceeded");
        }
        self.dispatch(input)
    }
}

pub struct NativeCapabilityDispatcher<P> {
    platform: P,
    last_candidates: Vec<crate::ActionCandidate>,
    resolved_targets: HashMap<ResolvedTargetId, ResolvedSystemAction>,
    selected_targets: HashMap<ResolvedTargetId, ResolvedSystemAction>,
    next_target_id: u64,
    target_ttl: Duration,
    target_store_expires_at: Option<Instant>,
}

impl<P> NativeCapabilityDispatcher<P> {
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            last_candidates: Vec::new(),
            resolved_targets: HashMap::new(),
            selected_targets: HashMap::new(),
            next_target_id: 0,
            target_ttl: Duration::from_secs(60),
            target_store_expires_at: None,
        }
    }

    pub fn with_target_ttl(platform: P, target_ttl: Duration) -> Self {
        let mut dispatcher = Self::new(platform);
        dispatcher.target_ttl = target_ttl;
        dispatcher
    }

    pub fn into_inner(self) -> P {
        self.platform
    }

    /// Candidate actions remain inside the trusted native worker for palette
    /// selection. The planner/UI receive opaque target IDs, never these
    /// platform action records.
    pub fn take_last_candidates(&mut self) -> Vec<crate::ActionCandidate> {
        std::mem::take(&mut self.last_candidates)
    }

    /// Converts an explicit, current palette selection into one one-time
    /// capability input. This is deliberately not exposed through planner
    /// observations: discovery alone never authorizes a launch.
    pub fn authorize_selected_action(
        &mut self,
        action: &ResolvedSystemAction,
    ) -> Result<CapabilityInput, String> {
        if self.target_store_expired() {
            self.clear_target_store();
            return Err("selected target expired before authorization".into());
        }
        let target = self
            .resolved_targets
            .iter()
            .find_map(|(target, resolved)| (resolved == action).then(|| target.clone()))
            .ok_or_else(|| {
                "selected application is no longer in the active target store".to_string()
            })?;
        self.selected_targets.insert(target.clone(), action.clone());
        Ok(CapabilityInput::Native(match action {
            ResolvedSystemAction::LaunchApplication { .. } => {
                NativeCapabilityCall::OpenApplication { target }
            }
            ResolvedSystemAction::OpenLocalTarget { .. } => {
                NativeCapabilityCall::OpenTarget { target }
            }
        }))
    }

    fn next_target(&mut self) -> ResolvedTargetId {
        self.next_target_id += 1;
        ResolvedTargetId::new(format!("target-{}", self.next_target_id))
    }

    fn start_target_store(&mut self) {
        self.last_candidates.clear();
        self.resolved_targets.clear();
        self.selected_targets.clear();
        self.target_store_expires_at = Some(Instant::now() + self.target_ttl);
    }

    fn clear_target_store(&mut self) {
        self.last_candidates.clear();
        self.resolved_targets.clear();
        self.selected_targets.clear();
        self.target_store_expires_at = None;
    }

    fn target_store_expired(&self) -> bool {
        self.target_store_expires_at
            .is_none_or(|expires_at| Instant::now() >= expires_at)
    }

    fn guard_observation(
        capability: CapabilityId,
        guard: &dyn DispatchGuard,
    ) -> Option<TypedObservation> {
        if guard.is_cancelled() {
            Some(TypedObservation::cancelled(capability))
        } else if guard.deadline_exceeded() {
            Some(TypedObservation::failed(
                capability,
                "capability deadline exceeded",
            ))
        } else {
            None
        }
    }
}

impl<P: ApplicationResolver + ActionExecutor> NativeCapabilityDispatcher<P> {
    fn dispatch_inner(
        &mut self,
        input: &CapabilityInput,
        guard: &dyn DispatchGuard,
    ) -> CapabilityOutput {
        let capability = input.capability_id();
        if !self.catalog().contains(capability) {
            return TypedObservation::failed(capability, "capability is unavailable");
        }
        if let Err(error) = input.validate() {
            return TypedObservation::failed(capability, error);
        }
        if let Some(observation) = Self::guard_observation(capability, guard) {
            return observation;
        }

        match input {
            CapabilityInput::Native(NativeCapabilityCall::FindApplication { query }) => {
                // A new discovery starts a fresh target store. In particular,
                // an ID from a prior session cannot be replayed.
                self.start_target_store();
                match self.platform.application_candidates() {
                    Ok(candidates) => {
                        if let Some(observation) = Self::guard_observation(capability, guard) {
                            return observation;
                        }
                        let ranked = rank_suggestions(
                            &SystemIntent::OpenTarget {
                                query: query.clone(),
                                hint: TargetHint::Application,
                            },
                            candidates
                                .iter()
                                .map(|candidate| candidate.candidate.clone()),
                            5,
                        );
                        let summaries = ranked
                            .into_iter()
                            .filter_map(|suggestion| {
                                let candidate = candidates.iter().find(|candidate| {
                                    candidate.candidate.stable_id == suggestion.stable_id
                                })?;
                                let target = self.next_target();
                                self.resolved_targets
                                    .insert(target.clone(), candidate.action.clone());
                                Some(crate::ActionCandidateSummary {
                                    target,
                                    display_name: candidate.candidate.display_name.clone(),
                                    kind: candidate.candidate.kind,
                                    evidence: candidate.candidate.subtitle.clone(),
                                })
                            })
                            .collect();
                        self.last_candidates = candidates;
                        TypedObservation::succeeded(
                            capability,
                            ObservationEvidence::ApplicationsResolved {
                                candidates: summaries,
                            },
                        )
                    }
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
            CapabilityInput::Native(NativeCapabilityCall::FindTarget { intent }) => {
                self.start_target_store();
                match self.platform.target_candidates(intent) {
                    Ok(candidates) => {
                        let ranked = rank_suggestions(
                            intent,
                            candidates
                                .iter()
                                .map(|candidate| candidate.candidate.clone()),
                            10,
                        );
                        let summaries = ranked
                            .iter()
                            .filter_map(|suggestion| {
                                let candidate = candidates.iter().find(|candidate| {
                                    candidate.candidate.stable_id == suggestion.stable_id
                                })?;
                                let target = self.next_target();
                                self.resolved_targets
                                    .insert(target.clone(), candidate.action.clone());
                                Some(crate::ActionCandidateSummary {
                                    target,
                                    display_name: candidate.candidate.display_name.clone(),
                                    kind: candidate.candidate.kind,
                                    evidence: candidate.candidate.subtitle.clone(),
                                })
                            })
                            .collect();
                        self.last_candidates = candidates;
                        TypedObservation::succeeded(
                            capability,
                            ObservationEvidence::TargetsResolved {
                                candidates: summaries,
                            },
                        )
                    }
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
            CapabilityInput::Native(NativeCapabilityCall::OpenApplication { target }) => {
                if self.target_store_expired() {
                    self.clear_target_store();
                    return TypedObservation::failed(capability, "selected target expired");
                }
                // A target must be both resolved in this store and explicitly
                // selected by the current palette. Consume it before the
                // revalidation/side-effect boundary to make it one-time.
                let Some(action) = self.selected_targets.remove(target) else {
                    return TypedObservation::failed(
                        capability,
                        "target was not selected for the active System session",
                    );
                };
                if let Some(observation) = Self::guard_observation(capability, guard) {
                    return observation;
                }
                let is_current = match self.platform.application_candidates() {
                    Ok(candidates) => {
                        if let Some(observation) = Self::guard_observation(capability, guard) {
                            return observation;
                        }
                        candidates
                            .into_iter()
                            .any(|candidate| candidate.action == action)
                    }
                    Err(error) => return TypedObservation::failed(capability, error.to_string()),
                };
                if !is_current {
                    return TypedObservation::failed(
                        capability,
                        "application changed after it was selected",
                    );
                }
                if let Some(observation) = Self::guard_observation(capability, guard) {
                    return observation;
                }
                match self.platform.execute(&action) {
                    Ok(result) => {
                        if let Some(observation) = Self::guard_observation(capability, guard) {
                            return observation;
                        }
                        match result.evidence {
                            ActionSuccessEvidence::ApplicationRunning => {
                                TypedObservation::succeeded(
                                    capability,
                                    ObservationEvidence::ApplicationLaunched {
                                        display_name: result.display_name,
                                        native_result: result.detail,
                                    },
                                )
                            }
                            ActionSuccessEvidence::LaunchRequestedOnly => TypedObservation::failed(
                                capability,
                                "native launcher accepted the request but did not verify the application launched",
                            ),
                            ActionSuccessEvidence::LocalTargetOpened
                            | ActionSuccessEvidence::LocalTargetRevealed => {
                                TypedObservation::failed(
                                    capability,
                                    "application executor returned local-target evidence",
                                )
                            }
                        }
                    }
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
            CapabilityInput::Native(NativeCapabilityCall::OpenTarget { target }) => {
                if self.target_store_expired() {
                    self.clear_target_store();
                    return TypedObservation::failed(capability, "selected target expired");
                }
                let Some(action) = self.selected_targets.remove(target) else {
                    return TypedObservation::failed(
                        capability,
                        "target was not selected for the active System session",
                    );
                };
                if !matches!(action, ResolvedSystemAction::OpenLocalTarget { .. }) {
                    return TypedObservation::failed(
                        capability,
                        "selected target has the wrong kind",
                    );
                }
                if let Some(observation) = Self::guard_observation(capability, guard) {
                    return observation;
                }
                match self.platform.execute(&action) {
                    Ok(result) => match result.evidence {
                        ActionSuccessEvidence::LocalTargetOpened => {
                            let kind = match action {
                                ResolvedSystemAction::OpenLocalTarget { kind, .. } => kind,
                                _ => unreachable!(),
                            };
                            TypedObservation::succeeded(
                                capability,
                                ObservationEvidence::LocalTargetOpened {
                                    display_name: result.display_name,
                                    kind: match kind {
                                        crate::LocalTargetKind::File => crate::CandidateKind::File,
                                        crate::LocalTargetKind::Folder => {
                                            crate::CandidateKind::Folder
                                        }
                                        crate::LocalTargetKind::Project => {
                                            crate::CandidateKind::Project
                                        }
                                    },
                                    native_result: result.detail,
                                },
                            )
                        }
                        ActionSuccessEvidence::LocalTargetRevealed => TypedObservation::succeeded(
                            capability,
                            ObservationEvidence::LocalTargetRevealed {
                                display_name: result.display_name,
                                native_result: result.detail,
                            },
                        ),
                        _ => TypedObservation::failed(
                            capability,
                            "native executor did not verify the local target action",
                        ),
                    },
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
            CapabilityInput::Native(NativeCapabilityCall::OpenUrl { url, browser }) => {
                if browser.is_some() && self.target_store_expired() {
                    self.clear_target_store();
                    return TypedObservation::failed(capability, "selected browser target expired");
                }
                // A supplied browser target must have been selected by the
                // active palette.  Default-browser navigation intentionally
                // has no target ID to invent or replay.
                let selected_browser = match browser {
                    Some(target) => match self.selected_targets.remove(target) {
                        Some(action) => {
                            let current = match self.platform.application_candidates() {
                                Ok(candidates) => candidates
                                    .into_iter()
                                    .any(|candidate| candidate.action == action),
                                Err(error) => {
                                    return TypedObservation::failed(capability, error.to_string());
                                }
                            };
                            if !current {
                                return TypedObservation::failed(
                                    capability,
                                    "selected browser changed after it was selected",
                                );
                            }
                            match self.platform.can_open_http_urls(&action) {
                                Ok(true) => Some(action),
                                Ok(false) => {
                                    return TypedObservation::failed(
                                        capability,
                                        "selected application does not handle HTTP(S) URLs",
                                    );
                                }
                                Err(error) => {
                                    return TypedObservation::failed(capability, error.to_string());
                                }
                            }
                        }
                        None => {
                            return TypedObservation::failed(
                                capability,
                                "browser target was not selected for the active System session",
                            );
                        }
                    },
                    None => None,
                };
                if let Some(observation) = Self::guard_observation(capability, guard) {
                    return observation;
                }
                match self.platform.open_url(url, selected_browser.as_ref()) {
                    Ok(result) => TypedObservation::succeeded(
                        capability,
                        ObservationEvidence::UrlOpened {
                            url: url.as_str().into(),
                            native_result: result.detail,
                        },
                    ),
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
            CapabilityInput::Native(NativeCapabilityCall::WebSearch { query, browser }) => {
                if browser.is_some() && self.target_store_expired() {
                    self.clear_target_store();
                    return TypedObservation::failed(capability, "selected browser target expired");
                }
                let url = match crate::ValidatedHttpUrl::web_search(query) {
                    Ok(url) => url,
                    Err(error) => return TypedObservation::failed(capability, error),
                };
                // Keep the same selection boundary as URL open; use the
                // native executor only with validated constructed data.
                let selected_browser = match browser {
                    Some(target) => match self.selected_targets.remove(target) {
                        Some(action) => {
                            let current = match self.platform.application_candidates() {
                                Ok(candidates) => candidates
                                    .into_iter()
                                    .any(|candidate| candidate.action == action),
                                Err(error) => {
                                    return TypedObservation::failed(capability, error.to_string());
                                }
                            };
                            if !current {
                                return TypedObservation::failed(
                                    capability,
                                    "selected browser changed after it was selected",
                                );
                            }
                            match self.platform.can_open_http_urls(&action) {
                                Ok(true) => Some(action),
                                Ok(false) => {
                                    return TypedObservation::failed(
                                        capability,
                                        "selected application does not handle HTTP(S) URLs",
                                    );
                                }
                                Err(error) => {
                                    return TypedObservation::failed(capability, error.to_string());
                                }
                            }
                        }
                        None => {
                            return TypedObservation::failed(
                                capability,
                                "browser target was not selected for the active System session",
                            );
                        }
                    },
                    None => None,
                };
                match self.platform.open_url(&url, selected_browser.as_ref()) {
                    Ok(result) => TypedObservation::succeeded(
                        capability,
                        ObservationEvidence::UrlOpened {
                            url: url.as_str().into(),
                            native_result: result.detail,
                        },
                    ),
                    Err(error) => TypedObservation::failed(capability, error.to_string()),
                }
            }
        }
    }
}

impl<P: ApplicationResolver + ActionExecutor> CapabilityDispatcher
    for NativeCapabilityDispatcher<P>
{
    fn catalog(&self) -> CapabilityCatalog {
        CapabilityCatalog
    }

    fn dispatch(&mut self, input: &CapabilityInput) -> CapabilityOutput {
        self.dispatch_inner(input, &UnboundedDispatch)
    }

    fn dispatch_guarded(
        &mut self,
        input: &CapabilityInput,
        guard: &dyn DispatchGuard,
    ) -> CapabilityOutput {
        self.dispatch_inner(input, guard)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationResolver, FakePlanner,
        FixtureApplicationResolver, NativeCapabilityCall, ObservationEvidence, ObservationStatus,
        PlanLimits, PlanTerminalState, PlannedCapabilityCall, PlannedCapabilityInput,
        RecordingExecutor, ResolvedTargetId, SystemOperationError, SystemPlan, SystemPlanRunner,
    };

    use super::*;

    #[derive(Default)]
    struct FixturePlatform {
        resolver: FixtureApplicationResolver,
        executor: RecordingExecutor,
        urls: Vec<(String, Option<String>)>,
    }

    impl ApplicationResolver for FixturePlatform {
        fn application_candidates(
            &mut self,
        ) -> Result<Vec<crate::ActionCandidate>, SystemOperationError> {
            self.resolver.application_candidates()
        }

        fn target_candidates(
            &mut self,
            intent: &crate::SystemIntent,
        ) -> Result<Vec<crate::ActionCandidate>, SystemOperationError> {
            self.resolver.target_candidates(intent)
        }
    }

    impl ActionExecutor for FixturePlatform {
        fn execute(
            &mut self,
            action: &crate::ResolvedSystemAction,
        ) -> Result<crate::ActionResult, SystemOperationError> {
            self.executor.execute(action)
        }

        fn open_url(
            &mut self,
            url: &crate::ValidatedHttpUrl,
            browser: Option<&crate::ResolvedSystemAction>,
        ) -> Result<crate::ActionResult, SystemOperationError> {
            self.urls.push((
                url.as_str().into(),
                browser.map(|action| action.display_name().into()),
            ));
            Ok(ActionResult {
                display_name: browser
                    .map(|action| action.display_name().into())
                    .unwrap_or_else(|| "Default browser".into()),
                detail: "recorded URL navigation".into(),
                evidence: ActionSuccessEvidence::ApplicationRunning,
            })
        }

        fn can_open_http_urls(
            &mut self,
            action: &crate::ResolvedSystemAction,
        ) -> Result<bool, SystemOperationError> {
            Ok(action.display_name() == "Google Chrome")
        }
    }

    struct RequestOnlyPlatform(FixtureApplicationResolver);

    impl Default for RequestOnlyPlatform {
        fn default() -> Self {
            Self(FixtureApplicationResolver)
        }
    }

    impl ApplicationResolver for RequestOnlyPlatform {
        fn application_candidates(
            &mut self,
        ) -> Result<Vec<crate::ActionCandidate>, SystemOperationError> {
            self.0.application_candidates()
        }
    }

    impl ActionExecutor for RequestOnlyPlatform {
        fn execute(
            &mut self,
            action: &crate::ResolvedSystemAction,
        ) -> Result<ActionResult, SystemOperationError> {
            Ok(ActionResult {
                display_name: action.display_name().into(),
                detail: "request accepted".into(),
                evidence: ActionSuccessEvidence::LaunchRequestedOnly,
            })
        }
    }

    struct Cancelled;
    impl DispatchGuard for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
        fn deadline_exceeded(&self) -> bool {
            false
        }
    }

    fn find_chrome(
        dispatcher: &mut NativeCapabilityDispatcher<FixturePlatform>,
    ) -> ResolvedTargetId {
        let found = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::FindApplication {
                query: "Chrome".into(),
            },
        ));
        match found.evidence {
            ObservationEvidence::ApplicationsResolved { candidates } => {
                candidates[0].target.clone()
            }
            other => panic!("unexpected observation: {other:?}"),
        }
    }

    #[test]
    fn discovery_observations_do_not_expose_the_platform_application_id() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let found = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::FindApplication {
                query: "Chrome".into(),
            },
        ));
        let json = serde_json::to_string(&found).unwrap();
        assert!(!json.contains("target-1"));
        assert!(!json.contains("fixture:chrome"));
    }

    #[test]
    fn discovery_observation_is_ranked_and_bounded_to_the_query() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let found = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::FindApplication {
                query: "Chrome".into(),
            },
        ));
        let ObservationEvidence::ApplicationsResolved { candidates } = found.evidence else {
            panic!("expected application candidates");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display_name, "Google Chrome");
    }

    #[test]
    fn discovery_without_explicit_selection_never_executes() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let target = find_chrome(&mut dispatcher);
        let opened = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::OpenApplication { target },
        ));
        assert_eq!(opened.status, ObservationStatus::Failed);
        assert!(dispatcher.into_inner().executor.actions().is_empty());
    }

    #[test]
    fn selected_target_runs_through_the_bounded_runner_once() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        find_chrome(&mut dispatcher);
        let action = dispatcher.take_last_candidates()[0].action.clone();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let plan = SystemPlan {
            goal: "Open Google Chrome".into(),
            steps: vec![PlannedCapabilityCall {
                id: "open".into(),
                input: PlannedCapabilityInput::Literal(input.clone()),
                depends_on: Vec::new(),
            }],
            limits: PlanLimits {
                max_steps: 1,
                whole_plan_timeout: std::time::Duration::from_secs(1),
                per_step_timeout: std::time::Duration::from_secs(1),
            },
        };
        let mut planner = FakePlanner::returning(plan);
        let run = SystemPlanRunner::run("Open Google Chrome", &mut planner, &mut dispatcher, &());
        assert_eq!(run.terminal, PlanTerminalState::Completed);
        let opened = &run.observations[0].1;
        assert!(matches!(
            opened.evidence,
            ObservationEvidence::ApplicationLaunched { .. }
        ));
        assert_eq!(
            dispatcher.dispatch(&input).status,
            ObservationStatus::Failed
        );
        assert_eq!(dispatcher.into_inner().executor.actions().len(), 1);
    }

    #[test]
    fn a_new_discovery_invalidates_old_selected_targets() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        find_chrome(&mut dispatcher);
        let action = dispatcher.take_last_candidates()[0].action.clone();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        find_chrome(&mut dispatcher);
        assert_eq!(
            dispatcher.dispatch(&input).status,
            ObservationStatus::Failed
        );
        assert!(dispatcher.into_inner().executor.actions().is_empty());
    }

    #[test]
    fn cancellation_is_checked_before_the_executor_receives_an_action() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        find_chrome(&mut dispatcher);
        let action = dispatcher.take_last_candidates()[0].action.clone();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let opened = dispatcher.dispatch_guarded(&input, &Cancelled);
        assert_eq!(opened.status, ObservationStatus::Cancelled);
        assert!(dispatcher.into_inner().executor.actions().is_empty());
    }

    #[test]
    fn launcher_request_without_running_application_evidence_is_not_success() {
        let mut dispatcher = NativeCapabilityDispatcher::new(RequestOnlyPlatform::default());
        let found = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::FindApplication {
                query: "Chrome".into(),
            },
        ));
        assert!(found.is_success());
        let action = dispatcher.take_last_candidates()[0].action.clone();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let opened = dispatcher.dispatch(&input);
        assert_eq!(opened.status, ObservationStatus::Failed);
    }

    #[test]
    fn selected_browser_id_is_consumed_and_passed_to_native_url_execution() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let target = find_chrome(&mut dispatcher);
        let action = dispatcher.take_last_candidates()[0].action.clone();
        dispatcher.authorize_selected_action(&action).unwrap();
        let url = crate::ValidatedHttpUrl::parse_spoken("example.com").unwrap();
        let opened = dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
            url: url.clone(),
            browser: Some(target.clone()),
        }));
        assert!(opened.is_success());
        assert_eq!(
            dispatcher.into_inner().urls,
            vec![(url.as_str().into(), Some("Google Chrome".into()))]
        );
        // The same selected target cannot be replayed for a second URL.
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let target = find_chrome(&mut dispatcher);
        let action = dispatcher.take_last_candidates()[0].action.clone();
        dispatcher.authorize_selected_action(&action).unwrap();
        let input = CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
            url,
            browser: Some(target),
        });
        assert!(dispatcher.dispatch(&input).is_success());
        assert_eq!(
            dispatcher.dispatch(&input).status,
            ObservationStatus::Failed
        );
    }

    #[test]
    fn unified_target_find_selects_and_opens_project_once() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let intent = SystemIntent::OpenTarget {
            query: "who-else-is-free".into(),
            hint: TargetHint::Auto,
        };
        let found =
            dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                intent: intent.clone(),
            }));
        let ObservationEvidence::TargetsResolved { candidates } = found.evidence else {
            panic!("expected unified target candidates");
        };
        assert_eq!(candidates[0].kind, crate::CandidateKind::Project);
        assert_eq!(candidates[0].display_name, "who-else-is-free");
        let all = dispatcher.take_last_candidates();
        let project = all
            .iter()
            .find(|candidate| candidate.candidate.kind == crate::CandidateKind::Project)
            .unwrap()
            .action
            .clone();
        let input = dispatcher.authorize_selected_action(&project).unwrap();
        let opened = dispatcher.dispatch(&input);
        assert!(matches!(
            opened.evidence,
            ObservationEvidence::LocalTargetOpened {
                kind: crate::CandidateKind::Project,
                ..
            }
        ));
        assert_eq!(
            dispatcher.dispatch(&input).status,
            ObservationStatus::Failed
        );
    }

    #[test]
    fn combined_project_and_editor_is_one_explicit_typed_selection() {
        let mut dispatcher = NativeCapabilityDispatcher::new(FixturePlatform::default());
        let intent = SystemIntent::OpenTargetWithApplication {
            query: "who-else-is-free".into(),
            application_query: "VS Code".into(),
        };
        let found =
            dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                intent: intent.clone(),
            }));
        assert!(found.is_success());
        let project = dispatcher
            .take_last_candidates()
            .into_iter()
            .find(|candidate| candidate.candidate.kind == crate::CandidateKind::Project)
            .unwrap()
            .action;
        assert!(matches!(
            &project,
            ResolvedSystemAction::OpenLocalTarget {
                application_display_name: Some(name),
                ..
            } if name == "Visual Studio Code"
        ));
        assert!(dispatcher.authorize_selected_action(&project).is_ok());
    }

    #[test]
    fn expired_target_store_rejects_selection_without_execution() {
        let mut dispatcher =
            NativeCapabilityDispatcher::with_target_ttl(FixturePlatform::default(), Duration::ZERO);
        let intent = SystemIntent::OpenTarget {
            query: "who-else-is-free".into(),
            hint: TargetHint::Auto,
        };
        dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::FindTarget {
            intent,
        }));
        let action = dispatcher.take_last_candidates()[0].action.clone();
        assert!(dispatcher.authorize_selected_action(&action).is_err());
        assert!(dispatcher.into_inner().executor.actions().is_empty());
    }
}

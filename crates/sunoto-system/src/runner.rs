//! A bounded, synchronous observe/validate/act/observe runner.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    CapabilityCatalog, CapabilityDispatcher, CapabilityId, CapabilityInput, DispatchGuard,
    NativeCapabilityCall, ObservationEvidence, ObservationStatus, TypedObservation,
};

pub const MAX_NATIVE_PLAN_STEPS: usize = 5;
pub const MAX_NATIVE_WHOLE_PLAN_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_NATIVE_PER_STEP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLimits {
    pub max_steps: usize,
    pub whole_plan_timeout: Duration,
    pub per_step_timeout: Duration,
}
impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_steps: MAX_NATIVE_PLAN_STEPS,
            whole_plan_timeout: MAX_NATIVE_WHOLE_PLAN_TIMEOUT,
            per_step_timeout: MAX_NATIVE_PER_STEP_TIMEOUT,
        }
    }
}

/// A capability input in a plan can either be complete when planning starts or
/// bind an opaque target returned by an earlier discovery step. The binding is
/// resolved inside the runner; planners never manufacture native target IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
pub enum PlannedCapabilityInput {
    Literal(CapabilityInput),
    /// An opaque result from a prior discovery step bound into a specific,
    /// typed argument.  The target is never serialized as a native ID.
    TargetFromStep {
        step_id: String,
        candidate_index: usize,
        argument: TargetArgument,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "argument", rename_all = "snake_case")]
pub enum TargetArgument {
    ApplicationTarget,
    LocalTarget,
    UrlBrowser { url: crate::ValidatedHttpUrl },
    WebSearchBrowser { query: String },
}

impl TargetArgument {
    fn capability_id(&self) -> CapabilityId {
        match self {
            Self::ApplicationTarget => CapabilityId::ApplicationOpen,
            Self::LocalTarget => CapabilityId::TargetOpen,
            Self::UrlBrowser { .. } => CapabilityId::UrlOpen,
            Self::WebSearchBrowser { .. } => CapabilityId::WebSearch,
        }
    }

    fn with_target(&self, target: crate::ResolvedTargetId) -> CapabilityInput {
        CapabilityInput::Native(match self {
            Self::ApplicationTarget => NativeCapabilityCall::OpenApplication { target },
            Self::LocalTarget => NativeCapabilityCall::OpenTarget { target },
            Self::UrlBrowser { url } => NativeCapabilityCall::OpenUrl {
                url: url.clone(),
                browser: Some(target),
            },
            Self::WebSearchBrowser { query } => NativeCapabilityCall::WebSearch {
                query: query.clone(),
                browser: Some(target),
            },
        })
    }
}

impl PlannedCapabilityInput {
    fn capability_id(&self) -> CapabilityId {
        match self {
            Self::Literal(input) => input.capability_id(),
            Self::TargetFromStep { argument, .. } => argument.capability_id(),
        }
    }

    fn source_step_id(&self) -> Option<&str> {
        match self {
            Self::Literal(_) => None,
            Self::TargetFromStep { step_id, .. } => Some(step_id),
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Literal(input) => input.validate(),
            Self::TargetFromStep {
                step_id,
                candidate_index,
                ..
            } => {
                if step_id.trim().is_empty() {
                    return Err("target binding source step must not be empty".into());
                }
                if *candidate_index >= 100 {
                    return Err("target binding candidate index is out of range".into());
                }
                Ok(())
            }
        }
    }

    fn resolve(
        &self,
        observations: &HashMap<String, TypedObservation>,
    ) -> Result<CapabilityInput, String> {
        match self {
            Self::Literal(input) => Ok(input.clone()),
            Self::TargetFromStep {
                step_id,
                candidate_index,
                argument,
            } => {
                let observation = observations.get(step_id).ok_or_else(|| {
                    format!("target binding source step {step_id} has no observation")
                })?;
                if !observation.is_success() {
                    return Err(format!(
                        "target binding source step {step_id} did not succeed"
                    ));
                }
                let candidates = match (&observation.evidence, argument) {
                    (
                        ObservationEvidence::ApplicationsResolved { candidates },
                        TargetArgument::ApplicationTarget
                        | TargetArgument::UrlBrowser { .. }
                        | TargetArgument::WebSearchBrowser { .. },
                    ) => candidates,
                    (
                        ObservationEvidence::TargetsResolved { candidates },
                        TargetArgument::LocalTarget,
                    ) => candidates,
                    _ => {
                        return Err(format!(
                            "target binding source step {step_id} returned the wrong target kind"
                        ));
                    }
                };
                let candidate = candidates
                    .get(*candidate_index)
                    .ok_or_else(|| {
                        format!(
                            "target binding candidate {candidate_index} is unavailable from step {step_id}"
                        )
                    })?;
                if matches!(argument, TargetArgument::LocalTarget)
                    && !matches!(
                        candidate.kind,
                        crate::CandidateKind::File
                            | crate::CandidateKind::Folder
                            | crate::CandidateKind::Project
                    )
                {
                    return Err(format!(
                        "target binding candidate {candidate_index} has the wrong kind"
                    ));
                }
                let target = candidate.target.clone();
                Ok(argument.with_target(target))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedCapabilityCall {
    pub id: String,
    pub input: PlannedCapabilityInput,
    pub depends_on: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPlan {
    pub goal: String,
    pub steps: Vec<PlannedCapabilityCall>,
    pub limits: PlanLimits,
}

pub trait Planner {
    fn plan(&mut self, goal: &str, catalog: &CapabilityCatalog) -> Result<SystemPlan, String>;
}
pub struct FakePlanner {
    result: Result<SystemPlan, String>,
}
impl FakePlanner {
    pub fn returning(plan: SystemPlan) -> Self {
        Self { result: Ok(plan) }
    }
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            result: Err(message.into()),
        }
    }
}
impl Planner for FakePlanner {
    fn plan(&mut self, _goal: &str, _catalog: &CapabilityCatalog) -> Result<SystemPlan, String> {
        self.result.clone()
    }
}

pub trait RunnerCancellation {
    fn is_cancelled(&self) -> bool;
}

struct RunnerDispatchGuard<'a> {
    cancellation: &'a dyn RunnerCancellation,
    deadline: Instant,
}

impl DispatchGuard for RunnerDispatchGuard<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn deadline_exceeded(&self) -> bool {
        Instant::now() >= self.deadline
    }
}
impl RunnerCancellation for () {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanTerminalState {
    Completed,
    Cancelled,
    Rejected { reason: String },
    Failed { step_id: String, reason: String },
    TimedOut { step_id: Option<String> },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRunResult {
    pub terminal: PlanTerminalState,
    pub observations: Vec<(String, TypedObservation)>,
}
pub struct SystemPlanRunner;

impl SystemPlanRunner {
    pub fn run<P: Planner, D: CapabilityDispatcher, C: RunnerCancellation>(
        goal: &str,
        planner: &mut P,
        dispatcher: &mut D,
        cancellation: &C,
    ) -> PlanRunResult {
        let plan = match planner.plan(goal, &dispatcher.catalog()) {
            Ok(plan) => plan,
            Err(reason) => {
                return PlanRunResult {
                    terminal: PlanTerminalState::Rejected { reason },
                    observations: vec![],
                };
            }
        };
        if let Err(reason) = validate_plan(&plan, &dispatcher.catalog()) {
            return PlanRunResult {
                terminal: PlanTerminalState::Rejected { reason },
                observations: vec![],
            };
        }
        let started = Instant::now();
        let mut complete = HashSet::new();
        let mut observation_by_step = HashMap::new();
        let mut observations = Vec::new();
        for step in &plan.steps {
            if cancellation.is_cancelled() {
                return PlanRunResult {
                    terminal: PlanTerminalState::Cancelled,
                    observations,
                };
            }
            if started.elapsed() >= plan.limits.whole_plan_timeout {
                return PlanRunResult {
                    terminal: PlanTerminalState::TimedOut { step_id: None },
                    observations,
                };
            }
            if !step
                .depends_on
                .iter()
                .all(|dependency| complete.contains(dependency))
            {
                return PlanRunResult {
                    terminal: PlanTerminalState::Rejected {
                        reason: format!("step {} has an unresolved dependency", step.id),
                    },
                    observations,
                };
            }
            let input = match step.input.resolve(&observation_by_step) {
                Ok(input) => input,
                Err(reason) => {
                    let observation = TypedObservation::failed(step.input.capability_id(), &reason);
                    observations.push((step.id.clone(), observation));
                    return PlanRunResult {
                        terminal: PlanTerminalState::Failed {
                            step_id: step.id.clone(),
                            reason,
                        },
                        observations,
                    };
                }
            };
            let step_started = Instant::now();
            let capability_timeout = dispatcher
                .catalog()
                .contract(input.capability_id())
                .map(|contract| Duration::from_millis(contract.timeout_ms))
                .unwrap_or(MAX_NATIVE_PER_STEP_TIMEOUT)
                .min(MAX_NATIVE_PER_STEP_TIMEOUT);
            let step_deadline = (step_started + plan.limits.per_step_timeout)
                .min(step_started + capability_timeout)
                .min(started + plan.limits.whole_plan_timeout)
                .min(started + MAX_NATIVE_WHOLE_PLAN_TIMEOUT);
            let guard = RunnerDispatchGuard {
                cancellation,
                deadline: step_deadline,
            };
            let observation = dispatcher.dispatch_guarded(&input, &guard);
            let timed_out = Instant::now() >= step_deadline;
            let success = observation.is_success();
            observation_by_step.insert(step.id.clone(), observation.clone());
            observations.push((step.id.clone(), observation));
            if cancellation.is_cancelled()
                || observations.last().is_some_and(|(_, observation)| {
                    observation.status == ObservationStatus::Cancelled
                })
            {
                return PlanRunResult {
                    terminal: PlanTerminalState::Cancelled,
                    observations,
                };
            }
            if timed_out {
                return PlanRunResult {
                    terminal: PlanTerminalState::TimedOut {
                        step_id: Some(step.id.clone()),
                    },
                    observations,
                };
            }
            if !success {
                let reason = match &observations.last().unwrap().1.evidence {
                    crate::ObservationEvidence::Failure { reason } => reason.clone(),
                    _ => "missing success evidence".into(),
                };
                return PlanRunResult {
                    terminal: PlanTerminalState::Failed {
                        step_id: step.id.clone(),
                        reason,
                    },
                    observations,
                };
            }
            complete.insert(step.id.clone());
        }
        PlanRunResult {
            terminal: PlanTerminalState::Completed,
            observations,
        }
    }
}

fn validate_plan(plan: &SystemPlan, catalog: &CapabilityCatalog) -> Result<(), String> {
    if plan.steps.is_empty() {
        return Err("plan has no steps".into());
    }
    if plan.limits.max_steps == 0
        || plan.limits.max_steps > MAX_NATIVE_PLAN_STEPS
        || plan.steps.len() > plan.limits.max_steps
        || plan.steps.len() > MAX_NATIVE_PLAN_STEPS
    {
        return Err("plan exceeds the native step limit".into());
    }
    if plan.limits.per_step_timeout.is_zero()
        || plan.limits.per_step_timeout > MAX_NATIVE_PER_STEP_TIMEOUT
    {
        return Err("per-step timeout exceeds the native limit".into());
    }
    if plan.limits.whole_plan_timeout.is_zero()
        || plan.limits.whole_plan_timeout > MAX_NATIVE_WHOLE_PLAN_TIMEOUT
    {
        return Err("whole-plan timeout exceeds the native limit".into());
    }
    let mut ids = HashSet::new();
    for step in &plan.steps {
        if step.id.trim().is_empty() || !ids.insert(step.id.as_str()) {
            return Err("plan has an empty or duplicate step ID".into());
        }
        if !catalog.contains(step.input.capability_id()) {
            return Err(format!("unknown capability in step {}", step.id));
        }
        step.input.validate()?;
        if step
            .depends_on
            .iter()
            .any(|dependency| !ids.contains(dependency.as_str()))
        {
            return Err(format!(
                "step {} depends on a future or unknown step",
                step.id
            ));
        }
        if let Some(source_step_id) = step.input.source_step_id()
            && !step
                .depends_on
                .iter()
                .any(|dependency| dependency == source_step_id)
        {
            return Err(format!(
                "step {} must depend on target source step {source_step_id}",
                step.id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionCandidateSummary, CapabilityId, ObservationEvidence, ResolvedTargetId};
    #[derive(Default)]
    struct RecordingDispatcher {
        calls: Vec<CapabilityInput>,
        fail_open: bool,
    }
    impl CapabilityDispatcher for RecordingDispatcher {
        fn catalog(&self) -> CapabilityCatalog {
            CapabilityCatalog
        }
        fn dispatch(&mut self, input: &CapabilityInput) -> TypedObservation {
            self.calls.push(input.clone());
            match input {
                CapabilityInput::Native(crate::NativeCapabilityCall::FindApplication {
                    ..
                }) => TypedObservation::succeeded(
                    CapabilityId::ApplicationFind,
                    ObservationEvidence::ApplicationsResolved {
                        candidates: vec![ActionCandidateSummary {
                            target: ResolvedTargetId::new("target-1"),
                            display_name: "Google Chrome".into(),
                            kind: crate::CandidateKind::Application,
                            evidence: Some("fixture application".into()),
                        }],
                    },
                ),
                CapabilityInput::Native(crate::NativeCapabilityCall::FindTarget { .. }) => {
                    TypedObservation::succeeded(
                        CapabilityId::TargetFind,
                        ObservationEvidence::TargetsResolved {
                            candidates: vec![ActionCandidateSummary {
                                target: ResolvedTargetId::new("target-project"),
                                display_name: "who-else-is-free".into(),
                                kind: crate::CandidateKind::Project,
                                evidence: Some("fixture project".into()),
                            }],
                        },
                    )
                }
                _ if self.fail_open => {
                    TypedObservation::failed(CapabilityId::ApplicationOpen, "native launch failed")
                }
                CapabilityInput::Native(crate::NativeCapabilityCall::OpenUrl { url, .. }) => {
                    TypedObservation::succeeded(
                        CapabilityId::UrlOpen,
                        ObservationEvidence::UrlOpened {
                            url: url.as_str().into(),
                            native_result: "recorded".into(),
                        },
                    )
                }
                CapabilityInput::Native(crate::NativeCapabilityCall::OpenTarget { .. }) => {
                    TypedObservation::succeeded(
                        CapabilityId::TargetOpen,
                        ObservationEvidence::LocalTargetOpened {
                            display_name: "who-else-is-free".into(),
                            kind: crate::CandidateKind::Project,
                            native_result: "recorded".into(),
                        },
                    )
                }
                _ => TypedObservation::succeeded(
                    CapabilityId::ApplicationOpen,
                    ObservationEvidence::ApplicationLaunched {
                        display_name: "Google Chrome".into(),
                        native_result: "recorded".into(),
                    },
                ),
            }
        }
    }
    fn plan() -> SystemPlan {
        SystemPlan {
            goal: "Open Chrome".into(),
            limits: PlanLimits::default(),
            steps: vec![
                PlannedCapabilityCall {
                    id: "find".into(),
                    input: PlannedCapabilityInput::Literal(CapabilityInput::Native(
                        crate::NativeCapabilityCall::FindApplication {
                            query: "Chrome".into(),
                        },
                    )),
                    depends_on: vec![],
                },
                PlannedCapabilityCall {
                    id: "open".into(),
                    input: PlannedCapabilityInput::TargetFromStep {
                        step_id: "find".into(),
                        candidate_index: 0,
                        argument: TargetArgument::ApplicationTarget,
                    },
                    depends_on: vec!["find".into()],
                },
            ],
        }
    }
    #[test]
    fn recording_plan_observes_before_and_after_execution() {
        let mut planner = FakePlanner::returning(plan());
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert_eq!(dispatcher.calls.len(), 2);
        assert!(matches!(
            result.observations[1].1.evidence,
            ObservationEvidence::ApplicationLaunched { .. }
        ));
    }

    #[test]
    fn target_binding_uses_the_resolved_id_from_the_source_observation() {
        let mut planner = FakePlanner::returning(plan());
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert!(matches!(
            &dispatcher.calls[1],
            CapabilityInput::Native(crate::NativeCapabilityCall::OpenApplication { target })
                if target == &ResolvedTargetId::new("target-1")
        ));
    }

    #[test]
    fn browser_result_binds_only_to_the_url_browser_argument() {
        let url = crate::ValidatedHttpUrl::parse_spoken("google.com").unwrap();
        let plan = SystemPlan {
            goal: "Open Chrome and go to google.com".into(),
            limits: PlanLimits::default(),
            steps: vec![
                PlannedCapabilityCall {
                    id: "find-browser".into(),
                    input: PlannedCapabilityInput::Literal(CapabilityInput::Native(
                        crate::NativeCapabilityCall::FindApplication {
                            query: "Chrome".into(),
                        },
                    )),
                    depends_on: vec![],
                },
                PlannedCapabilityCall {
                    id: "open-url".into(),
                    input: PlannedCapabilityInput::TargetFromStep {
                        step_id: "find-browser".into(),
                        candidate_index: 0,
                        argument: TargetArgument::UrlBrowser { url: url.clone() },
                    },
                    depends_on: vec!["find-browser".into()],
                },
            ],
        };
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run(
            "Open Chrome and go to google.com",
            &mut planner,
            &mut dispatcher,
            &(),
        );
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert!(matches!(
            &dispatcher.calls[1],
            CapabilityInput::Native(crate::NativeCapabilityCall::OpenUrl { url: actual, browser: Some(target) })
                if actual == &url && target == &ResolvedTargetId::new("target-1")
        ));
    }

    #[test]
    fn local_target_result_binds_only_to_target_open() {
        let intent = crate::SystemIntent::OpenTarget {
            query: "who-else-is-free".into(),
            hint: crate::TargetHint::Auto,
        };
        let plan = SystemPlan {
            goal: "Open project".into(),
            limits: PlanLimits::default(),
            steps: vec![
                PlannedCapabilityCall {
                    id: "find-target".into(),
                    input: PlannedCapabilityInput::Literal(CapabilityInput::Native(
                        crate::NativeCapabilityCall::FindTarget { intent },
                    )),
                    depends_on: vec![],
                },
                PlannedCapabilityCall {
                    id: "open-target".into(),
                    input: PlannedCapabilityInput::TargetFromStep {
                        step_id: "find-target".into(),
                        candidate_index: 0,
                        argument: TargetArgument::LocalTarget,
                    },
                    depends_on: vec!["find-target".into()],
                },
            ],
        };
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open project", &mut planner, &mut dispatcher, &());
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert!(matches!(
            &dispatcher.calls[1],
            CapabilityInput::Native(crate::NativeCapabilityCall::OpenTarget { target })
                if target == &ResolvedTargetId::new("target-project")
        ));
    }

    #[test]
    fn serialized_plan_references_a_prior_step_not_a_native_target() {
        let json = serde_json::to_string(&plan()).unwrap();
        assert!(json.contains("target_from_step"));
        assert!(json.contains("find"));
        assert!(!json.contains("target-1"));
    }
    #[test]
    fn failed_observation_stops_the_plan() {
        let mut planner = FakePlanner::returning(plan());
        let mut dispatcher = RecordingDispatcher {
            fail_open: true,
            ..Default::default()
        };
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(result.terminal, PlanTerminalState::Failed { .. }));
        assert_eq!(result.observations.len(), 2);
    }
    struct Cancel;
    impl RunnerCancellation for Cancel {
        fn is_cancelled(&self) -> bool {
            true
        }
    }
    #[test]
    fn cancellation_runs_no_action() {
        let mut planner = FakePlanner::returning(plan());
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &Cancel);
        assert_eq!(result.terminal, PlanTerminalState::Cancelled);
        assert!(dispatcher.calls.is_empty());
    }
    #[test]
    fn future_dependency_is_rejected() {
        let mut plan = plan();
        plan.steps[0].depends_on.push("open".into());
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::Rejected { .. }
        ));
    }

    #[test]
    fn target_binding_requires_an_explicit_dependency() {
        let mut plan = plan();
        plan.steps[1].depends_on.clear();
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::Rejected { .. }
        ));
        assert!(dispatcher.calls.is_empty());
    }

    #[test]
    fn missing_bound_candidate_fails_without_dispatching_the_later_step() {
        let mut plan = plan();
        plan.steps[1].input = PlannedCapabilityInput::TargetFromStep {
            step_id: "find".into(),
            candidate_index: 1,
            argument: TargetArgument::ApplicationTarget,
        };
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(result.terminal, PlanTerminalState::Failed { .. }));
        assert_eq!(dispatcher.calls.len(), 1);
    }

    #[test]
    fn planner_failure_is_rejected_without_dispatch() {
        let mut planner = FakePlanner::failing("planner unavailable");
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::Rejected { .. }
        ));
        assert!(dispatcher.calls.is_empty());
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let mut plan = plan();
        plan.steps[1].id = "find".into();
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::Rejected { .. }
        ));
        assert!(dispatcher.calls.is_empty());
    }

    #[test]
    fn planner_cannot_raise_system_owned_limits() {
        let mut plan = plan();
        plan.limits.max_steps = MAX_NATIVE_PLAN_STEPS + 1;
        plan.limits.per_step_timeout = MAX_NATIVE_PER_STEP_TIMEOUT + Duration::from_millis(1);
        plan.limits.whole_plan_timeout = MAX_NATIVE_WHOLE_PLAN_TIMEOUT + Duration::from_millis(1);
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = RecordingDispatcher::default();
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::Rejected { .. }
        ));
        assert!(dispatcher.calls.is_empty());
    }

    struct SlowDispatcher(RecordingDispatcher);

    impl CapabilityDispatcher for SlowDispatcher {
        fn catalog(&self) -> CapabilityCatalog {
            CapabilityCatalog
        }

        fn dispatch(&mut self, input: &CapabilityInput) -> TypedObservation {
            std::thread::sleep(Duration::from_millis(2));
            self.0.dispatch(input)
        }
    }

    #[test]
    fn per_step_deadline_marks_a_slow_dispatch_as_timed_out() {
        let mut plan = plan();
        plan.limits.per_step_timeout = Duration::from_millis(1);
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = SlowDispatcher(RecordingDispatcher::default());
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::TimedOut { step_id: Some(_) }
        ));
        assert_eq!(dispatcher.0.calls.len(), 1);
    }

    #[test]
    fn whole_plan_deadline_caps_the_current_step() {
        let mut plan = plan();
        plan.limits.per_step_timeout = Duration::from_millis(10);
        plan.limits.whole_plan_timeout = Duration::from_millis(1);
        let mut planner = FakePlanner::returning(plan);
        let mut dispatcher = SlowDispatcher(RecordingDispatcher::default());
        let result = SystemPlanRunner::run("Open Chrome", &mut planner, &mut dispatcher, &());
        assert!(matches!(
            result.terminal,
            PlanTerminalState::TimedOut { step_id: Some(_) }
        ));
        assert_eq!(dispatcher.0.calls.len(), 1);
    }
}

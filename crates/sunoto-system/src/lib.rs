//! Pure planning primitives for Sunoto's voice-powered System command palette.
//!
//! This crate deliberately contains no platform APIs and no command execution.
//! It turns a transcript into a typed intent, ranks already-resolved candidates,
//! and decides whether those candidates may be displayed. Platform discovery,
//! UI selection, and execution remain separate layers so raw speech can never
//! become a shell command by accident.

mod action;
mod capability;
mod dispatcher;
mod fixtures;
mod intent;
mod parser;
mod policy;
mod runner;
mod selection;
mod suggestion;
mod targets;
mod url;

pub use action::{
    ActionCandidate, ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationId,
    ApplicationResolver, LocalTargetId, ResolvedSystemAction, SystemOperationError,
    application_aliases,
};
pub use capability::{
    ActionCandidateSummary, CapabilityCatalog, CapabilityContract, CapabilityId, CapabilityInput,
    CapabilityOutput, ConfirmationRule, NativeCapabilityCall, ObservationEvidence,
    ObservationStatus, ResolvedTargetId, TypedObservation,
};
pub use dispatcher::{CapabilityDispatcher, DispatchGuard, NativeCapabilityDispatcher};
pub use fixtures::{FixtureApplicationResolver, RecordingExecutor};
pub use intent::{RouteOutcome, RouteRejection, SystemIntent, TargetHint};
pub use parser::route_deterministically;
pub use policy::{PolicyDecision, RiskClass, policy_for_intent};
pub use runner::{
    FakePlanner, MAX_NATIVE_PER_STEP_TIMEOUT, MAX_NATIVE_PLAN_STEPS, MAX_NATIVE_WHOLE_PLAN_TIMEOUT,
    PlanLimits, PlanRunResult, PlanTerminalState, PlannedCapabilityCall, PlannedCapabilityInput,
    Planner, RunnerCancellation, SystemPlan, SystemPlanRunner, TargetArgument,
};
pub use selection::{PendingSuggestionSet, SelectableSuggestion, SelectionError};
pub use suggestion::{
    CandidateKind, ResolvedCandidate, SuggestionAction, SystemSuggestion, rank_suggestions,
};
pub use targets::{FilesystemTargetProvider, LocalTarget, LocalTargetKind};
pub use url::ValidatedHttpUrl;

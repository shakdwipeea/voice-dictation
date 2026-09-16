//! Stable, typed capability contracts shared by planners and native executors.
//!
//! This deliberately advertises only the application capabilities implemented
//! by the native foundation. There is no command-string capability.

use serde::{Deserialize, Serialize};

use crate::{CandidateKind, RiskClass, ValidatedHttpUrl};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    ApplicationFind,
    ApplicationOpen,
    TargetFind,
    TargetOpen,
    UrlOpen,
    WebSearch,
}

impl CapabilityId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApplicationFind => "application.find",
            Self::ApplicationOpen => "application.open",
            Self::TargetFind => "target.find",
            Self::TargetOpen => "target.open",
            Self::UrlOpen => "url.open",
            Self::WebSearch => "web.search",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationRule {
    ExplicitSelection,
}

/// A compact JSON-schema-like contract. It is deliberately data, so a future
/// model adapter can receive it without gaining an executable interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityContract {
    pub id: CapabilityId,
    pub stable_id: &'static str,
    pub version: u8,
    pub provider: &'static str,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub risk: RiskClass,
    pub confirmation: ConfirmationRule,
    pub timeout_ms: u64,
    pub observations_may_be_untrusted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityCatalog;

impl CapabilityCatalog {
    pub fn contracts(&self) -> Vec<CapabilityContract> {
        vec![
            CapabilityContract {
                id: CapabilityId::ApplicationFind,
                stable_id: CapabilityId::ApplicationFind.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","minLength":1,"maxLength":200}}}),
                output_schema: serde_json::json!({"type":"object","required":["candidates"]}),
                risk: RiskClass::LaunchOrNavigation,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 5_000,
                // Display names originate in application bundles/Desktop
                // Entries. They are environment data, not planner
                // instructions, and must be treated as untrusted if a future
                // model sees them.
                observations_may_be_untrusted: true,
            },
            CapabilityContract {
                id: CapabilityId::UrlOpen,
                stable_id: CapabilityId::UrlOpen.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["url"],"properties":{"url":{"type":"validated_http_url"},"browser_target":{"type":"opaque_id"}}}),
                output_schema: serde_json::json!({"type":"object","required":["evidence"]}),
                risk: RiskClass::LaunchOrNavigation,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 10_000,
                observations_may_be_untrusted: true,
            },
            CapabilityContract {
                id: CapabilityId::WebSearch,
                stable_id: CapabilityId::WebSearch.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","minLength":1,"maxLength":200}}}),
                output_schema: serde_json::json!({"type":"object","required":["evidence"]}),
                risk: RiskClass::LaunchOrNavigation,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 10_000,
                observations_may_be_untrusted: true,
            },
            CapabilityContract {
                id: CapabilityId::TargetFind,
                stable_id: CapabilityId::TargetFind.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","minLength":1,"maxLength":200}}}),
                output_schema: serde_json::json!({"type":"object","required":["candidates"]}),
                risk: RiskClass::LocalContentOpen,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 5_000,
                observations_may_be_untrusted: true,
            },
            CapabilityContract {
                id: CapabilityId::TargetOpen,
                stable_id: CapabilityId::TargetOpen.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["target_id"],"properties":{"target_id":{"type":"opaque_id"},"application_target":{"type":"opaque_id"},"reveal":{"type":"boolean"}}}),
                output_schema: serde_json::json!({"type":"object","required":["evidence"]}),
                risk: RiskClass::LocalContentOpen,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 10_000,
                observations_may_be_untrusted: true,
            },
            CapabilityContract {
                id: CapabilityId::ApplicationOpen,
                stable_id: CapabilityId::ApplicationOpen.as_str(),
                version: 1,
                provider: "native",
                input_schema: serde_json::json!({"type":"object","required":["target_id"],"properties":{"target_id":{"type":"opaque_id"}}}),
                output_schema: serde_json::json!({"type":"object","required":["evidence"]}),
                risk: RiskClass::LaunchOrNavigation,
                confirmation: ConfirmationRule::ExplicitSelection,
                timeout_ms: 10_000,
                observations_may_be_untrusted: true,
            },
        ]
    }

    pub fn contains(&self, id: CapabilityId) -> bool {
        self.contracts().iter().any(|contract| contract.id == id)
    }

    pub fn contract(&self, id: CapabilityId) -> Option<CapabilityContract> {
        self.contracts()
            .into_iter()
            .find(|contract| contract.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeCapabilityCall {
    FindApplication {
        query: String,
    },
    OpenApplication {
        target: ResolvedTargetId,
    },
    FindTarget {
        intent: crate::SystemIntent,
    },
    OpenTarget {
        target: ResolvedTargetId,
    },
    OpenUrl {
        url: ValidatedHttpUrl,
        browser: Option<ResolvedTargetId>,
    },
    WebSearch {
        query: String,
        browser: Option<ResolvedTargetId>,
    },
}

impl NativeCapabilityCall {
    pub const fn capability_id(&self) -> CapabilityId {
        match self {
            Self::FindApplication { .. } => CapabilityId::ApplicationFind,
            Self::OpenApplication { .. } => CapabilityId::ApplicationOpen,
            Self::FindTarget { .. } => CapabilityId::TargetFind,
            Self::OpenTarget { .. } => CapabilityId::TargetOpen,
            Self::OpenUrl { .. } => CapabilityId::UrlOpen,
            Self::WebSearch { .. } => CapabilityId::WebSearch,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::FindApplication { query } if query.trim().is_empty() || query.len() > 200 => {
                Err("application query must be between 1 and 200 characters".into())
            }
            Self::FindApplication { .. } => Ok(()),
            Self::FindTarget { intent }
                if intent.query().trim().is_empty() || intent.query().len() > 200 =>
            {
                Err("target query must be between 1 and 200 characters".into())
            }
            Self::FindTarget { intent }
                if !matches!(
                    intent,
                    crate::SystemIntent::OpenTarget { .. }
                        | crate::SystemIntent::OpenTargetWithApplication { .. }
                        | crate::SystemIntent::FindFile { .. }
                        | crate::SystemIntent::OpenFileByQuery { .. }
                        | crate::SystemIntent::RevealFileByQuery { .. }
                        | crate::SystemIntent::OpenFolder { .. }
                ) =>
            {
                Err("intent is not supported by target.find".into())
            }
            Self::FindTarget { .. } => Ok(()),
            Self::OpenApplication { target } | Self::OpenTarget { target } if target.is_empty() => {
                Err("target ID must not be empty".into())
            }
            Self::OpenApplication { .. } | Self::OpenTarget { .. } => Ok(()),
            Self::OpenUrl { .. } => Ok(()),
            Self::WebSearch { query, .. } if query.trim().is_empty() || query.len() > 200 => {
                Err("search query must be between 1 and 200 characters".into())
            }
            Self::WebSearch { .. } => Ok(()),
        }
    }
}

/// A short-lived identifier minted by the native dispatcher for one target
/// resolution. Unlike an `ApplicationId`, it never embeds platform paths and
/// is useless outside the dispatcher's active target store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResolvedTargetId(String);

impl ResolvedTargetId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CapabilityInput {
    Native(NativeCapabilityCall),
}

impl CapabilityInput {
    pub const fn capability_id(&self) -> CapabilityId {
        match self {
            Self::Native(call) => call.capability_id(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Native(call) => call.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObservationEvidence {
    ApplicationsResolved {
        candidates: Vec<ActionCandidateSummary>,
    },
    TargetsResolved {
        candidates: Vec<ActionCandidateSummary>,
    },
    ApplicationLaunched {
        display_name: String,
        native_result: String,
    },
    LocalTargetOpened {
        display_name: String,
        kind: CandidateKind,
        native_result: String,
    },
    LocalTargetRevealed {
        display_name: String,
        native_result: String,
    },
    UrlOpened {
        url: String,
        native_result: String,
    },
    Failure {
        reason: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionCandidateSummary {
    /// Runner-internal opaque reference. It is intentionally omitted from
    /// read-only JSON observations; live selection remains the only route
    /// that can carry it back into the trusted target store.
    #[serde(skip_serializing)]
    pub target: ResolvedTargetId,
    pub display_name: String,
    pub kind: CandidateKind,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedObservation {
    pub capability: CapabilityId,
    pub status: ObservationStatus,
    pub evidence: ObservationEvidence,
}

impl TypedObservation {
    pub fn succeeded(capability: CapabilityId, evidence: ObservationEvidence) -> Self {
        Self {
            capability,
            status: ObservationStatus::Succeeded,
            evidence,
        }
    }
    pub fn failed(capability: CapabilityId, reason: impl Into<String>) -> Self {
        Self {
            capability,
            status: ObservationStatus::Failed,
            evidence: ObservationEvidence::Failure {
                reason: reason.into(),
            },
        }
    }
    pub fn cancelled(capability: CapabilityId) -> Self {
        Self {
            capability,
            status: ObservationStatus::Cancelled,
            evidence: ObservationEvidence::Cancelled,
        }
    }
    pub fn is_success(&self) -> bool {
        self.status == ObservationStatus::Succeeded
    }
}

pub type CapabilityOutput = TypedObservation;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_has_only_typed_native_capabilities() {
        let json = serde_json::to_string(&CapabilityCatalog.contracts()).unwrap();
        assert!(json.contains("application.find"));
        assert!(!json.contains("shell"));
        assert!(!json.contains("command"));
    }

    #[test]
    fn open_contract_requires_an_opaque_target_not_a_platform_application_id() {
        let contract = CapabilityCatalog
            .contracts()
            .into_iter()
            .find(|contract| contract.id == CapabilityId::ApplicationOpen)
            .unwrap();
        assert!(
            contract.input_schema["required"]
                .to_string()
                .contains("target_id")
        );
        assert!(!contract.input_schema.to_string().contains("application_id"));
    }

    #[test]
    fn application_observations_are_classified_as_untrusted_environment_data() {
        assert!(
            CapabilityCatalog
                .contracts()
                .iter()
                .all(|contract| contract.observations_may_be_untrusted)
        );
    }
}

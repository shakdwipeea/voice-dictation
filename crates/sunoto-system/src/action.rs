use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{LocalTargetKind, ResolvedCandidate, SystemIntent};

/// Opaque platform identity for an installed application.
///
/// The command palette never receives this value. It sees a short-lived
/// suggestion token, while the daemon retains the application identity for
/// revalidation and execution after an explicit selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApplicationId(String);

impl ApplicationId {
    pub fn new(value: impl Into<String>) -> Result<Self, SystemOperationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SystemOperationError::new(
                "application ID must not be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalTargetId(String);

impl LocalTargetId {
    pub fn new(value: impl Into<String>) -> Result<Self, SystemOperationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SystemOperationError::new(
                "local target ID must not be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fully resolved actions accepted by platform executors.
///
/// There is deliberately no shell-command or generic executable variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSystemAction {
    LaunchApplication {
        application: ApplicationId,
        display_name: String,
    },
    OpenLocalTarget {
        target: LocalTargetId,
        display_name: String,
        kind: LocalTargetKind,
        reveal: bool,
        application: Option<ApplicationId>,
        application_display_name: Option<String>,
    },
}

impl ResolvedSystemAction {
    pub fn display_name(&self) -> &str {
        match self {
            Self::LaunchApplication { display_name, .. }
            | Self::OpenLocalTarget { display_name, .. } => display_name,
        }
    }
}

/// A rankable candidate paired with the typed action it represents.
///
/// The action stays daemon-side; only a generated suggestion token crosses
/// the UI protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionCandidate {
    pub candidate: ResolvedCandidate,
    pub action: ResolvedSystemAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub display_name: String,
    pub detail: String,
    pub evidence: ActionSuccessEvidence,
}

/// Native execution evidence is intentionally distinct from an API/process
/// return code. A dispatcher may only report a completed user goal when the
/// executor supplies verified evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSuccessEvidence {
    ApplicationRunning,
    LocalTargetOpened,
    LocalTargetRevealed,
    LaunchRequestedOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemOperationError {
    message: String,
}

impl SystemOperationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SystemOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SystemOperationError {}

pub trait ApplicationResolver {
    fn application_candidates(&mut self) -> Result<Vec<ActionCandidate>, SystemOperationError>;

    fn target_candidates(
        &mut self,
        _intent: &SystemIntent,
    ) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        self.application_candidates()
    }
}

pub trait ActionExecutor {
    fn execute(
        &mut self,
        action: &ResolvedSystemAction,
    ) -> Result<ActionResult, SystemOperationError>;

    /// Native URL navigation has no raw command-string escape hatch. Platform
    /// implementations may open the validated URL with the default browser or
    /// a previously revalidated resolved browser.
    fn open_url(
        &mut self,
        _url: &crate::ValidatedHttpUrl,
        _browser: Option<&ResolvedSystemAction>,
    ) -> Result<ActionResult, SystemOperationError> {
        Err(SystemOperationError::new(
            "native URL navigation is unavailable",
        ))
    }

    /// Verify that a resolved application advertises HTTP(S) handling before
    /// it is used as a selected browser. This is separate from application
    /// launching: arbitrary installed applications are never assumed to be
    /// URL handlers.
    fn can_open_http_urls(
        &mut self,
        _action: &ResolvedSystemAction,
    ) -> Result<bool, SystemOperationError> {
        Ok(false)
    }
}

/// Common application aliases are resolver data, not parser branches.
///
/// Generic vendor stripping handles names such as "Google Chrome" while the
/// small table covers established product names that are not derivable (for
/// example, users commonly say "Apple Music" for the app named "Music").
pub fn application_aliases(display_name: &str) -> Vec<String> {
    const KNOWN: &[(&str, &[&str])] = &[
        ("google chrome", &["chrome"]),
        ("music", &["apple music"]),
        ("visual studio code", &["vs code", "vscode"]),
        ("microsoft edge", &["edge"]),
    ];

    let normalized = normalize(display_name);
    let mut aliases = BTreeSet::new();
    for (name, values) in KNOWN {
        if normalized == *name {
            aliases.extend(values.iter().map(|value| (*value).to_string()));
        }
    }
    for prefix in ["apple ", "google ", "microsoft "] {
        if let Some(short_name) = normalized.strip_prefix(prefix)
            && !short_name.is_empty()
        {
            aliases.insert(short_name.to_string());
        }
    }
    aliases.into_iter().collect()
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_data_driven_and_deduplicated() {
        assert_eq!(application_aliases("Google Chrome"), vec!["chrome"]);
        assert_eq!(application_aliases("Music"), vec!["apple music"]);
    }

    #[test]
    fn application_ids_cannot_be_empty() {
        assert!(ApplicationId::new(" ").is_err());
    }
}

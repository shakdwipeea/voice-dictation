use serde::{Deserialize, Serialize};

/// Optional target information carried by a spoken `open` command.
///
/// `Auto` is resolved against installed applications, known folders, files,
/// and validated URLs. Stronger hints come from explicit words such as
/// "launch", "file", or "folder" and narrow the candidate providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetHint {
    Auto,
    Application,
    File,
    Folder,
    Url,
}

/// An unresolved, non-executable interpretation of a System-mode transcript.
///
/// Every field is still user-level text. A platform resolver must convert it
/// into stable application or canonical file identifiers before suggestions
/// can be displayed. There is intentionally no shell-command variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemIntent {
    OpenTarget {
        query: String,
        hint: TargetHint,
    },
    OpenTargetWithApplication {
        query: String,
        application_query: String,
    },
    FindFile {
        query: String,
    },
    OpenFileByQuery {
        query: String,
    },
    RevealFileByQuery {
        query: String,
    },
    OpenFolder {
        query: String,
    },
    WebSearch {
        query: String,
    },
    OpenUrl {
        spoken_url: String,
    },
    OpenBrowserAndUrl {
        browser_query: String,
        spoken_url: String,
    },
}

impl SystemIntent {
    /// The user-provided lookup text, used by resolver providers and UI.
    pub fn query(&self) -> &str {
        match self {
            Self::OpenTarget { query, .. }
            | Self::OpenTargetWithApplication { query, .. }
            | Self::FindFile { query }
            | Self::OpenFileByQuery { query }
            | Self::RevealFileByQuery { query }
            | Self::OpenFolder { query }
            | Self::WebSearch { query } => query,
            Self::OpenUrl { spoken_url } | Self::OpenBrowserAndUrl { spoken_url, .. } => spoken_url,
        }
    }
}

/// Inputs rejected before the LLM fallback because executing them would be
/// outside the one-action System-mode contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRejection {
    Empty,
    MultipleActions,
    ControlCharacters,
}

/// Result of the deterministic command catalog.
///
/// Only `NeedsLlm` may be sent to the constrained LLM router. Rejected input
/// fails closed and must not be reinterpreted by a more permissive component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RouteOutcome {
    Matched {
        normalized_text: String,
        intent: SystemIntent,
    },
    NeedsLlm {
        normalized_text: String,
    },
    Rejected {
        normalized_text: String,
        reason: RouteRejection,
    },
}

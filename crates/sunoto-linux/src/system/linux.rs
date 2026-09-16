//! Desktop Entry discovery with execution delegated to the GIO CLI.
//!
//! We parse only metadata needed for suggestions. We never parse or execute a
//! desktop file's `Exec` line ourselves. After selection, `gio launch` reads
//! the revalidated desktop entry and applies GAppInfo launch semantics.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sunoto_system::{
    ActionCandidate, ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationId,
    ApplicationResolver, CandidateKind, FilesystemTargetProvider, LocalTargetId, LocalTargetKind,
    ResolvedCandidate, ResolvedSystemAction, SystemIntent, SystemOperationError, TargetHint,
    application_aliases, rank_suggestions,
};

pub struct SystemPlatform {
    applications: HashMap<ApplicationId, PathBuf>,
    local_targets: HashMap<LocalTargetId, LocalTargetRecord>,
    search_roots: Vec<PathBuf>,
    target_generation: u64,
}

#[derive(Clone)]
struct LocalTargetRecord {
    path: PathBuf,
    kind: LocalTargetKind,
}

impl Default for SystemPlatform {
    fn default() -> Self {
        Self::with_search_roots(default_search_roots())
    }
}

impl SystemPlatform {
    pub fn with_search_roots(search_roots: Vec<PathBuf>) -> Self {
        Self {
            applications: HashMap::new(),
            local_targets: HashMap::new(),
            search_roots,
            target_generation: 0,
        }
    }

    fn filesystem_provider(&self) -> Result<FilesystemTargetProvider, SystemOperationError> {
        FilesystemTargetProvider::new(self.search_roots.clone(), 20_000, 20)
            .map_err(SystemOperationError::new)
    }

    fn revalidate_application(
        &self,
        application: &ApplicationId,
    ) -> Result<PathBuf, SystemOperationError> {
        let path = self
            .applications
            .get(application)
            .ok_or_else(|| SystemOperationError::new("application is no longer resolved"))?;
        let canonical = path.canonicalize().map_err(|error| {
            SystemOperationError::new(format!("application disappeared: {error}"))
        })?;
        if canonical != *path || parse_desktop_entry(&canonical).is_none() {
            return Err(SystemOperationError::new(
                "desktop entry changed after it was suggested",
            ));
        }
        Ok(canonical)
    }
}

impl ApplicationResolver for SystemPlatform {
    fn application_candidates(&mut self) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        self.applications.clear();
        let mut candidates = Vec::new();
        for path in desktop_entry_paths() {
            let Some(entry) = parse_desktop_entry(&path) else {
                continue;
            };
            let canonical = path.canonicalize().unwrap_or(path);
            let application =
                ApplicationId::new(format!("linux-app:{}", canonical.to_string_lossy()))?;
            self.applications
                .insert(application.clone(), canonical.clone());
            candidates.push(ActionCandidate {
                candidate: ResolvedCandidate {
                    stable_id: application.as_str().into(),
                    display_name: entry.name.clone(),
                    subtitle: Some("Desktop application".into()),
                    kind: CandidateKind::Application,
                    aliases: application_aliases(&entry.name),
                    recency_score: 0,
                },
                action: ResolvedSystemAction::LaunchApplication {
                    application,
                    display_name: entry.name,
                },
            });
        }
        candidates.sort_by(|left, right| {
            left.candidate
                .display_name
                .cmp(&right.candidate.display_name)
                .then_with(|| left.candidate.stable_id.cmp(&right.candidate.stable_id))
        });
        Ok(candidates)
    }

    fn target_candidates(
        &mut self,
        intent: &SystemIntent,
    ) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        let applications = self.application_candidates()?;
        if matches!(
            intent,
            SystemIntent::OpenTarget {
                hint: TargetHint::Application,
                ..
            }
        ) {
            return Ok(applications);
        }
        let selected_applications = match intent {
            SystemIntent::OpenTargetWithApplication {
                application_query, ..
            } => {
                let application_intent = SystemIntent::OpenTarget {
                    query: application_query.clone(),
                    hint: TargetHint::Application,
                };
                let ranked = rank_suggestions(
                    &application_intent,
                    applications
                        .iter()
                        .map(|candidate| candidate.candidate.clone()),
                    5,
                );
                ranked
                    .iter()
                    .filter_map(|suggestion| {
                        applications
                            .iter()
                            .find(|candidate| candidate.candidate.stable_id == suggestion.stable_id)
                            .and_then(|candidate| match &candidate.action {
                                ResolvedSystemAction::LaunchApplication {
                                    application,
                                    display_name,
                                } => Some((application.clone(), display_name.clone())),
                                _ => None,
                            })
                    })
                    .collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        if matches!(intent, SystemIntent::OpenTargetWithApplication { .. })
            && selected_applications.is_empty()
        {
            return Ok(Vec::new());
        }

        let provider = self.filesystem_provider()?;
        let reveal = matches!(
            intent,
            SystemIntent::FindFile { .. } | SystemIntent::RevealFileByQuery { .. }
        );
        let expected_kind = match intent {
            SystemIntent::OpenFileByQuery { .. }
            | SystemIntent::FindFile { .. }
            | SystemIntent::RevealFileByQuery { .. } => Some(LocalTargetKind::File),
            SystemIntent::OpenFolder { .. } => Some(LocalTargetKind::Folder),
            _ => None,
        };
        self.local_targets.clear();
        self.target_generation = self.target_generation.wrapping_add(1);
        let mut candidates = Vec::new();
        for (index, target) in provider
            .find_bounded(intent.query(), std::time::Duration::from_secs(2), || false)
            .into_iter()
            .enumerate()
        {
            if expected_kind.is_some_and(|kind| target.kind != kind) {
                continue;
            }
            let location = target
                .canonical_path()
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "approved location".into());
            let choices = if selected_applications.is_empty() {
                vec![None]
            } else {
                selected_applications.iter().cloned().map(Some).collect()
            };
            for (application_index, selected_application) in choices.into_iter().enumerate() {
                let id = LocalTargetId::new(format!(
                    "linux-target-{}-{}-{}",
                    self.target_generation,
                    index + 1,
                    application_index + 1
                ))?;
                let application_detail = selected_application
                    .as_ref()
                    .map(|(_, name)| format!("; opens with {name}"))
                    .unwrap_or_default();
                self.local_targets.insert(
                    id.clone(),
                    LocalTargetRecord {
                        path: target.canonical_path().to_path_buf(),
                        kind: target.kind,
                    },
                );
                let (application, application_display_name) = selected_application
                    .map(|(id, name)| (Some(id), Some(name)))
                    .unwrap_or((None, None));
                candidates.push(ActionCandidate {
                    candidate: ResolvedCandidate {
                        stable_id: id.as_str().into(),
                        display_name: target.display_name.clone(),
                        subtitle: Some(format!(
                            "{} · {}{}",
                            kind_label(target.kind),
                            location,
                            application_detail
                        )),
                        kind: candidate_kind(target.kind),
                        aliases: Vec::new(),
                        recency_score: target.recency_score,
                    },
                    action: ResolvedSystemAction::OpenLocalTarget {
                        target: id,
                        display_name: target.display_name.clone(),
                        kind: target.kind,
                        reveal,
                        application,
                        application_display_name,
                    },
                });
            }
        }
        if matches!(
            intent,
            SystemIntent::OpenTarget {
                hint: TargetHint::Auto,
                ..
            }
        ) {
            candidates.extend(applications);
        }
        let ranked = rank_suggestions(
            intent,
            candidates
                .iter()
                .map(|candidate| candidate.candidate.clone()),
            50,
        );
        Ok(ranked
            .into_iter()
            .filter_map(|suggestion| {
                candidates
                    .iter()
                    .find(|candidate| candidate.candidate.stable_id == suggestion.stable_id)
                    .cloned()
            })
            .collect())
    }
}

impl ActionExecutor for SystemPlatform {
    fn execute(
        &mut self,
        action: &ResolvedSystemAction,
    ) -> Result<ActionResult, SystemOperationError> {
        match action {
            ResolvedSystemAction::LaunchApplication {
                application,
                display_name,
            } => {
                let canonical = self.revalidate_application(application)?;
                let status = Command::new("gio")
                    .arg("launch")
                    .arg(&canonical)
                    .status()
                    .map_err(|error| {
                        SystemOperationError::new(format!("cannot start GIO launcher: {error}"))
                    })?;
                if !status.success() {
                    return Err(SystemOperationError::new(format!(
                        "GIO refused to open {display_name}"
                    )));
                }
                Ok(ActionResult {
                    display_name: display_name.clone(),
                    detail: "opened with GIO AppInfo".into(),
                    evidence: ActionSuccessEvidence::LaunchRequestedOnly,
                })
            }
            ResolvedSystemAction::OpenLocalTarget {
                target,
                display_name,
                kind,
                reveal,
                application,
                application_display_name,
            } => {
                let record = self.local_targets.get(target).ok_or_else(|| {
                    SystemOperationError::new("local target is no longer resolved")
                })?;
                if record.kind != *kind {
                    return Err(SystemOperationError::new(
                        "local target kind changed after it was selected",
                    ));
                }
                let canonical = self
                    .filesystem_provider()?
                    .revalidate(&record.path, *kind)
                    .map_err(SystemOperationError::new)?;
                let mut command = Command::new("gio");
                let (detail, evidence) = if *reveal {
                    command
                        .arg("open")
                        .arg(canonical.parent().unwrap_or(&canonical));
                    (
                        "opened containing folder with GIO".to_string(),
                        ActionSuccessEvidence::LocalTargetRevealed,
                    )
                } else if let Some(application) = application {
                    let desktop = self.revalidate_application(application)?;
                    command.arg("launch").arg(desktop).arg(&canonical);
                    (
                        format!(
                            "opened with {} through GIO",
                            application_display_name
                                .as_deref()
                                .unwrap_or("selected application")
                        ),
                        ActionSuccessEvidence::LocalTargetOpened,
                    )
                } else {
                    command.arg("open").arg(&canonical);
                    (
                        "opened with GIO".to_string(),
                        ActionSuccessEvidence::LocalTargetOpened,
                    )
                };
                let status = command.status().map_err(|error| {
                    SystemOperationError::new(format!("cannot start GIO target opener: {error}"))
                })?;
                if !status.success() {
                    return Err(SystemOperationError::new(format!(
                        "GIO refused to open {display_name}"
                    )));
                }
                Ok(ActionResult {
                    display_name: display_name.clone(),
                    detail,
                    evidence,
                })
            }
        }
    }

    fn open_url(
        &mut self,
        url: &sunoto_system::ValidatedHttpUrl,
        browser: Option<&ResolvedSystemAction>,
    ) -> Result<ActionResult, SystemOperationError> {
        let mut command = Command::new("gio");
        let display_name = match browser {
            Some(ResolvedSystemAction::LaunchApplication {
                application,
                display_name,
            }) => {
                let canonical = self.revalidate_application(application)?;
                command.arg("launch").arg(canonical).arg(url.as_str());
                display_name.clone()
            }
            None => {
                command.arg("open").arg(url.as_str());
                "Default browser".into()
            }
            Some(ResolvedSystemAction::OpenLocalTarget { .. }) => {
                return Err(SystemOperationError::new(
                    "selected URL handler is not an application",
                ));
            }
        };
        let status = command.status().map_err(|error| {
            SystemOperationError::new(format!("cannot start GIO URI opener: {error}"))
        })?;
        if !status.success() {
            return Err(SystemOperationError::new(
                "GIO refused to open the validated URL",
            ));
        }
        Ok(ActionResult {
            display_name,
            detail: "URL open requested with GIO".into(),
            evidence: ActionSuccessEvidence::LaunchRequestedOnly,
        })
    }

    fn can_open_http_urls(
        &mut self,
        action: &ResolvedSystemAction,
    ) -> Result<bool, SystemOperationError> {
        let ResolvedSystemAction::LaunchApplication { application, .. } = action else {
            return Ok(false);
        };
        let path = self
            .applications
            .get(application)
            .ok_or_else(|| SystemOperationError::new("selected browser is no longer resolved"))?;
        let canonical = path.canonicalize().map_err(|error| {
            SystemOperationError::new(format!("selected browser disappeared: {error}"))
        })?;
        if canonical != *path {
            return Ok(false);
        }
        Ok(parse_desktop_entry(&canonical).is_some_and(|entry| entry.http_handler))
    }
}

struct DesktopEntry {
    name: String,
    http_handler: bool,
}

fn default_search_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    ["Desktop", "Documents", "Downloads", "workspace"]
        .into_iter()
        .map(|name| home.join(name))
        .collect()
}

fn candidate_kind(kind: LocalTargetKind) -> CandidateKind {
    match kind {
        LocalTargetKind::File => CandidateKind::File,
        LocalTargetKind::Folder => CandidateKind::Folder,
        LocalTargetKind::Project => CandidateKind::Project,
    }
}

fn kind_label(kind: LocalTargetKind) -> &'static str {
    match kind {
        LocalTargetKind::File => "File",
        LocalTargetKind::Folder => "Folder",
        LocalTargetKind::Project => "Project",
    }
}

fn desktop_entry_paths() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.insert(0, PathBuf::from(home).join(".local/share/applications"));
    }

    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "desktop")
                && seen.insert(entry.file_name())
            {
                paths.push(path);
            }
        }
    }
    paths
}

fn parse_desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut application = false;
    let mut hidden = false;
    let mut http_handler = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Name" => name = Some(value.trim().to_string()),
            "Type" => application = value.trim() == "Application",
            "Hidden" | "NoDisplay" if value.trim().eq_ignore_ascii_case("true") => {
                hidden = true;
            }
            "MimeType" => {
                http_handler = value.split(';').any(|mime| {
                    matches!(
                        mime.trim(),
                        "x-scheme-handler/http" | "x-scheme-handler/https"
                    )
                });
            }
            _ => {}
        }
    }
    if hidden || !application {
        return None;
    }
    name.filter(|name| !name.is_empty())
        .map(|name| DesktopEntry { name, http_handler })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_parser_ignores_exec_and_hidden_entries() {
        let root =
            std::env::temp_dir().join(format!("sunoto-desktop-entry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let visible = root.join("visible.desktop");
        fs::write(
            &visible,
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=sh -c 'bad'\n",
        )
        .unwrap();
        assert_eq!(parse_desktop_entry(&visible).unwrap().name, "Browser");

        let hidden = root.join("hidden.desktop");
        fs::write(
            &hidden,
            "[Desktop Entry]\nType=Application\nName=Hidden\nNoDisplay=true\n",
        )
        .unwrap();
        assert!(parse_desktop_entry(&hidden).is_none());
        let _ = fs::remove_dir_all(root);
    }

    /// Deliberately opt-in: asks GIO to open harmless example.com URLs through
    /// the default handler and an installed desktop entry that declares HTTP
    /// support. It also proves that the same resolved application can be
    /// launched only after an explicit live-test invocation.
    #[test]
    #[ignore = "opens a harmless URL and installed HTTP handler through GIO"]
    fn live_default_and_installed_http_handler_navigation() {
        let mut platform = SystemPlatform::default();
        let default_url = sunoto_system::ValidatedHttpUrl::parse_spoken(
            "https://example.com/?sunoto=linux-default",
        )
        .unwrap();
        let default_opened = platform.open_url(&default_url, None).unwrap();
        assert_eq!(
            default_opened.evidence,
            ActionSuccessEvidence::LaunchRequestedOnly
        );

        let applications = platform.application_candidates().unwrap();
        let browser = applications
            .into_iter()
            .find(|candidate| {
                platform
                    .can_open_http_urls(&candidate.action)
                    .unwrap_or(false)
            })
            .expect("Linux desktop should expose an installed HTTP handler");
        let launched = platform.execute(&browser.action).unwrap();
        assert_eq!(
            launched.evidence,
            ActionSuccessEvidence::LaunchRequestedOnly
        );

        let selected_url = sunoto_system::ValidatedHttpUrl::parse_spoken(
            "https://example.com/?sunoto=linux-selected",
        )
        .unwrap();
        let selected_opened = platform
            .open_url(&selected_url, Some(&browser.action))
            .unwrap();
        assert_eq!(
            selected_opened.evidence,
            ActionSuccessEvidence::LaunchRequestedOnly
        );
    }

    /// Deliberately opt-in: opens/reveals only disposable targets created
    /// under the test's approved temporary root.
    #[test]
    #[ignore = "opens and reveals disposable file/folder/project fixtures through GIO"]
    fn live_disposable_file_folder_project_and_reveal() {
        let root = std::env::temp_dir().join(format!(
            "sunoto-live-linux-local-targets-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sunoto-test-folder")).unwrap();
        fs::create_dir_all(root.join("sunoto-test-project/.git")).unwrap();
        fs::write(
            root.join("sunoto-test-document.txt"),
            "Sunoto harmless fixture\n",
        )
        .unwrap();

        for (intent, name, kind, expected) in [
            (
                SystemIntent::OpenFileByQuery {
                    query: "sunoto-test-document".into(),
                },
                "sunoto-test-document.txt",
                CandidateKind::File,
                ActionSuccessEvidence::LocalTargetOpened,
            ),
            (
                SystemIntent::RevealFileByQuery {
                    query: "sunoto-test-document".into(),
                },
                "sunoto-test-document.txt",
                CandidateKind::File,
                ActionSuccessEvidence::LocalTargetRevealed,
            ),
            (
                SystemIntent::OpenFolder {
                    query: "sunoto-test-folder".into(),
                },
                "sunoto-test-folder",
                CandidateKind::Folder,
                ActionSuccessEvidence::LocalTargetOpened,
            ),
            (
                SystemIntent::OpenTarget {
                    query: "sunoto-test-project".into(),
                    hint: TargetHint::Auto,
                },
                "sunoto-test-project",
                CandidateKind::Project,
                ActionSuccessEvidence::LocalTargetOpened,
            ),
        ] {
            let mut platform = SystemPlatform::with_search_roots(vec![root.clone()]);
            let candidates = platform.target_candidates(&intent).unwrap();
            let action = candidates
                .into_iter()
                .find(|candidate| {
                    candidate.candidate.display_name == name && candidate.candidate.kind == kind
                })
                .unwrap_or_else(|| panic!("{name} should resolve as {kind:?}"))
                .action;
            let result = platform.execute(&action).unwrap();
            assert_eq!(result.evidence, expected);
        }
        let _ = fs::remove_dir_all(root);
    }
}

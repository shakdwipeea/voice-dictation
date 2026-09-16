//! Native macOS application discovery and launch support for System mode.
//!
//! Discovery turns installed `.app` bundles into opaque typed candidates.
//! Execution accepts only an `ApplicationId` produced by that discovery pass,
//! revalidates the bundle, and launches it with `NSWorkspace`. No spoken text
//! is ever passed to a shell or interpreted as an executable name.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Read;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use sunoto_system::{
    ActionCandidate, ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationId,
    ApplicationResolver, CandidateKind, FilesystemTargetProvider, LocalTarget, LocalTargetId,
    LocalTargetKind, ResolvedCandidate, ResolvedSystemAction, SystemIntent, SystemOperationError,
    TargetHint, application_aliases, rank_suggestions,
};

#[link(name = "AppKit", kind = "framework")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

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
}

impl ApplicationResolver for SystemPlatform {
    fn application_candidates(&mut self) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        let bundles = discover_application_bundles(&application_roots());
        self.applications.clear();

        let mut candidates = Vec::with_capacity(bundles.len());
        for path in bundles {
            let Some(display_name) = application_display_name(&path) else {
                continue;
            };
            let application = ApplicationId::new(format!("macos-app:{}", path.to_string_lossy()))?;
            self.applications.insert(application.clone(), path.clone());
            candidates.push(ActionCandidate {
                candidate: ResolvedCandidate {
                    stable_id: application.as_str().into(),
                    display_name: display_name.clone(),
                    // Platform paths remain private to the resolver/executor.
                    subtitle: Some("Application".into()),
                    kind: CandidateKind::Application,
                    aliases: application_aliases(&display_name),
                    recency_score: 0,
                },
                action: ResolvedSystemAction::LaunchApplication {
                    application,
                    display_name,
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
        let mut local = spotlight_targets(&provider, intent.query(), &self.search_roots, 20);
        let mut seen = local
            .iter()
            .map(|target| target.canonical_path().to_path_buf())
            .collect::<HashSet<_>>();
        for target in provider.find_bounded(intent.query(), Duration::from_secs(2), || false) {
            if seen.insert(target.canonical_path().to_path_buf()) {
                local.push(target);
            }
        }

        self.local_targets.clear();
        self.target_generation = self.target_generation.wrapping_add(1);
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
        let mut candidates = Vec::new();
        for (index, target) in local.into_iter().enumerate() {
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
                    "macos-target-{}-{}-{}",
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
                if !open_application_with_workspace(&canonical)? {
                    return Err(SystemOperationError::new(format!(
                        "macOS refused to open {display_name}"
                    )));
                }
                if !wait_for_application_running(&canonical, Duration::from_secs(5))? {
                    return Err(SystemOperationError::new(format!(
                        "macOS accepted {display_name} but it did not launch before the verification deadline"
                    )));
                }
                Ok(ActionResult {
                    display_name: display_name.clone(),
                    detail: "launch verified by NSWorkspace runningApplications".into(),
                    evidence: ActionSuccessEvidence::ApplicationRunning,
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
                let (opened, detail, evidence) = if *reveal {
                    (
                        reveal_file_with_workspace(&canonical)?,
                        "revealed with NSWorkspace file viewer".to_string(),
                        ActionSuccessEvidence::LocalTargetRevealed,
                    )
                } else if let Some(application) = application {
                    let app_path = self.revalidate_application(application)?;
                    (
                        open_file_in_application(&canonical, &app_path)?,
                        format!(
                            "opened with {} through NSWorkspace",
                            application_display_name
                                .as_deref()
                                .unwrap_or("selected application")
                        ),
                        ActionSuccessEvidence::LocalTargetOpened,
                    )
                } else {
                    (
                        open_file_with_workspace(&canonical)?,
                        "opened with NSWorkspace".to_string(),
                        ActionSuccessEvidence::LocalTargetOpened,
                    )
                };
                if !opened {
                    return Err(SystemOperationError::new(format!(
                        "macOS refused to open {display_name}"
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
        let display_name = match browser {
            Some(ResolvedSystemAction::LaunchApplication {
                application,
                display_name,
            }) => {
                let path = self.applications.get(application).ok_or_else(|| {
                    SystemOperationError::new("selected browser is no longer resolved")
                })?;
                let canonical = path.canonicalize().map_err(|error| {
                    SystemOperationError::new(format!("selected browser disappeared: {error}"))
                })?;
                if canonical != *path || !is_application_bundle(&canonical) {
                    return Err(SystemOperationError::new(
                        "selected browser changed after it was selected",
                    ));
                }
                if !open_http_url_in_application(url.as_str(), &canonical)? {
                    return Err(SystemOperationError::new(
                        "macOS refused to open the validated URL in the selected browser",
                    ));
                }
                display_name.clone()
            }
            None => {
                if !open_http_url_with_workspace(url.as_str())? {
                    return Err(SystemOperationError::new(
                        "macOS refused to open the validated URL",
                    ));
                }
                "Default browser".into()
            }
            Some(ResolvedSystemAction::OpenLocalTarget { .. }) => {
                return Err(SystemOperationError::new(
                    "selected URL handler is not an application",
                ));
            }
        };
        if display_name.is_empty() {
            return Err(SystemOperationError::new(
                "native browser did not provide a display name",
            ));
        }
        Ok(ActionResult {
            detail: "URL open accepted by NSWorkspace".into(),
            display_name,
            evidence: ActionSuccessEvidence::ApplicationRunning,
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
        if canonical != *path || !is_application_bundle(&canonical) {
            return Ok(false);
        }
        // Bundle metadata is data, not a hard-coded browser-name allowlist.
        // `CFBundleURLTypes` encodes scheme handlers in the app's Info.plist.
        let info = fs::read(canonical.join("Contents/Info.plist")).unwrap_or_default();
        let text = String::from_utf8_lossy(&info).to_ascii_lowercase();
        Ok(text.contains("http") && text.contains("https"))
    }
}

impl SystemPlatform {
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
        if canonical != *path || !is_application_bundle(&canonical) {
            return Err(SystemOperationError::new(
                "application changed after it was suggested",
            ));
        }
        Ok(canonical)
    }
}

fn application_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/CoreServices/Applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
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

/// Query the host's Spotlight index, then pass every hit back through the
/// shared approved-root/safe-type classifier. `mdfind` receives fixed
/// arguments directly (never a shell command), is killed at the deadline,
/// and is only an indexed accelerator over the portable bounded fallback.
fn spotlight_targets(
    provider: &FilesystemTargetProvider,
    query: &str,
    roots: &[PathBuf],
    limit: usize,
) -> Vec<LocalTarget> {
    if query.trim().is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
        return Vec::new();
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        if targets.len() >= limit || Instant::now() >= deadline {
            break;
        }
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        let mut child = match Command::new("/usr/bin/mdfind")
            .arg("-0")
            .arg("-onlyin")
            .arg(&root)
            .arg("-name")
            .arg(query)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => break,
        };
        let reader = child.stdout.take().map(|mut stdout| {
            thread::spawn(move || {
                let mut output = Vec::new();
                let _ = stdout.read_to_end(&mut output);
                output
            })
        });
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        let output = reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        for raw_path in output
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = PathBuf::from(String::from_utf8_lossy(raw_path).into_owned());
            if let Some(target) = provider.candidate_from_path(query, &path)
                && seen.insert(target.canonical_path().to_path_buf())
            {
                targets.push(target);
                if targets.len() >= limit {
                    break;
                }
            }
        }
    }
    targets
}

fn discover_application_bundles(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        collect_bundles(root, 3, &mut found);
    }
    let mut seen = HashSet::new();
    found.retain(|path| seen.insert(path.clone()));
    found
}

fn collect_bundles(directory: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_application_bundle(&path) {
            if let Ok(canonical) = path.canonicalize() {
                found.push(canonical);
            }
            continue;
        }
        if depth > 0
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && !entry.file_name().to_string_lossy().starts_with('.')
        {
            collect_bundles(&path, depth - 1, found);
        }
    }
}

fn is_application_bundle(path: &Path) -> bool {
    path.is_dir()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
}

fn application_display_name(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty())
}

fn open_application_with_workspace(path: &Path) -> Result<bool, SystemOperationError> {
    let path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| SystemOperationError::new("application path contains a NUL byte"))?;
    let pool_class = objc_class("NSAutoreleasePool")?;
    let string_class = objc_class("NSString")?;
    let url_class = objc_class("NSURL")?;
    let workspace_class = objc_class("NSWorkspace")?;

    // SAFETY: every receiver and selector is a Foundation/AppKit object with
    // the exact method signature used below. The autorelease pool bounds all
    // temporary Objective-C objects created on this Rust worker thread.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            path.as_ptr(),
        );
        if string.is_null() {
            send_void(pool, selector("drain")?);
            return Err(SystemOperationError::new(
                "cannot create the native application path",
            ));
        }
        let url = send_id_id(url_class, selector("fileURLWithPath:")?, string);
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let opened = !url.is_null()
            && !workspace.is_null()
            && send_bool_id(workspace, selector("openURL:")?, url);
        send_void(pool, selector("drain")?);
        Ok(opened)
    }
}

fn open_file_with_workspace(path: &Path) -> Result<bool, SystemOperationError> {
    open_application_with_workspace(path)
}

fn open_file_in_application(path: &Path, application: &Path) -> Result<bool, SystemOperationError> {
    let path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| SystemOperationError::new("local target path contains a NUL byte"))?;
    let application = CString::new(application.to_string_lossy().as_bytes())
        .map_err(|_| SystemOperationError::new("application path contains a NUL byte"))?;
    let pool_class = objc_class("NSAutoreleasePool")?;
    let string_class = objc_class("NSString")?;
    let url_class = objc_class("NSURL")?;
    let array_class = objc_class("NSArray")?;
    let bundle_class = objc_class("NSBundle")?;
    let workspace_class = objc_class("NSWorkspace")?;
    // SAFETY: selectors are documented AppKit/Foundation methods and all
    // temporary objects are bounded by this worker-thread autorelease pool.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let path_string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            path.as_ptr(),
        );
        let app_string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            application.as_ptr(),
        );
        let target_url = send_id_id(url_class, selector("fileURLWithPath:")?, path_string);
        let app_url = send_id_id(url_class, selector("fileURLWithPath:")?, app_string);
        let urls = send_id_id(array_class, selector("arrayWithObject:")?, target_url);
        let bundle = send_id_id(bundle_class, selector("bundleWithURL:")?, app_url);
        let bundle_id = send_id(bundle, selector("bundleIdentifier")?);
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let opened = !target_url.is_null()
            && !bundle_id.is_null()
            && !workspace.is_null()
            && send_bool_id_id_usize_id_id(
                workspace,
                selector(
                    "openURLs:withAppBundleIdentifier:options:additionalEventParamDescriptor:launchIdentifiers:",
                )?,
                urls,
                bundle_id,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        send_void(pool, selector("drain")?);
        Ok(opened)
    }
}

fn reveal_file_with_workspace(path: &Path) -> Result<bool, SystemOperationError> {
    let path = CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| SystemOperationError::new("local target path contains a NUL byte"))?;
    let pool_class = objc_class("NSAutoreleasePool")?;
    let string_class = objc_class("NSString")?;
    let url_class = objc_class("NSURL")?;
    let array_class = objc_class("NSArray")?;
    let workspace_class = objc_class("NSWorkspace")?;
    // SAFETY: `activateFileViewerSelectingURLs:` is a documented NSWorkspace
    // call. The path was revalidated and is represented as an NSURL.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            path.as_ptr(),
        );
        let url = send_id_id(url_class, selector("fileURLWithPath:")?, string);
        let urls = send_id_id(array_class, selector("arrayWithObject:")?, url);
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let accepted = !url.is_null() && !urls.is_null() && !workspace.is_null();
        if accepted {
            send_void_id(
                workspace,
                selector("activateFileViewerSelectingURLs:")?,
                urls,
            );
        }
        send_void(pool, selector("drain")?);
        Ok(accepted)
    }
}

fn open_http_url_with_workspace(url: &str) -> Result<bool, SystemOperationError> {
    let url =
        CString::new(url).map_err(|_| SystemOperationError::new("URL contains a NUL byte"))?;
    let pool_class = objc_class("NSAutoreleasePool")?;
    let string_class = objc_class("NSString")?;
    let url_class = objc_class("NSURL")?;
    let workspace_class = objc_class("NSWorkspace")?;
    // SAFETY: all selectors use documented Foundation/AppKit object signatures.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            url.as_ptr(),
        );
        let native_url = if string.is_null() {
            std::ptr::null_mut()
        } else {
            send_id_id(url_class, selector("URLWithString:")?, string)
        };
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let opened = !native_url.is_null()
            && !workspace.is_null()
            && send_bool_id(workspace, selector("openURL:")?, native_url);
        send_void(pool, selector("drain")?);
        Ok(opened)
    }
}

/// Opens a validated URL through a resolved application bundle.  This is the
/// documented NSWorkspace API family rather than `open -a` or AppleScript;
/// the spoken URL is never interpreted by a shell.
fn open_http_url_in_application(
    url: &str,
    application: &Path,
) -> Result<bool, SystemOperationError> {
    let url =
        CString::new(url).map_err(|_| SystemOperationError::new("URL contains a NUL byte"))?;
    let application = CString::new(application.to_string_lossy().as_bytes())
        .map_err(|_| SystemOperationError::new("application path contains a NUL byte"))?;
    let pool_class = objc_class("NSAutoreleasePool")?;
    let string_class = objc_class("NSString")?;
    let url_class = objc_class("NSURL")?;
    let array_class = objc_class("NSArray")?;
    let bundle_class = objc_class("NSBundle")?;
    let workspace_class = objc_class("NSWorkspace")?;
    // SAFETY: selectors below are documented AppKit/Foundation calls.  The
    // temporary objects remain within this worker thread's autorelease pool.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let url_string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            url.as_ptr(),
        );
        let app_string = send_id_cstr(
            string_class,
            selector("stringWithUTF8String:")?,
            application.as_ptr(),
        );
        let native_url = if url_string.is_null() {
            std::ptr::null_mut()
        } else {
            send_id_id(url_class, selector("URLWithString:")?, url_string)
        };
        let app_url = if app_string.is_null() {
            std::ptr::null_mut()
        } else {
            send_id_id(url_class, selector("fileURLWithPath:")?, app_string)
        };
        let urls = if native_url.is_null() {
            std::ptr::null_mut()
        } else {
            send_id_id(array_class, selector("arrayWithObject:")?, native_url)
        };
        let bundle = if app_url.is_null() {
            std::ptr::null_mut()
        } else {
            send_id_id(bundle_class, selector("bundleWithURL:")?, app_url)
        };
        let bundle_id = if bundle.is_null() {
            std::ptr::null_mut()
        } else {
            send_id(bundle, selector("bundleIdentifier")?)
        };
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let opened = !urls.is_null()
            && !bundle_id.is_null()
            && !workspace.is_null()
            && send_bool_id_id_usize_id_id(
                workspace,
                selector(
                    "openURLs:withAppBundleIdentifier:options:additionalEventParamDescriptor:launchIdentifiers:",
                )?,
                urls,
                bundle_id,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        send_void(pool, selector("drain")?);
        Ok(opened)
    }
}

/// `openURL:` acknowledges the request synchronously but does not prove the
/// target process started. Poll the native running-application inventory on
/// the isolated System worker before reporting a completed launch.
fn wait_for_application_running(
    path: &Path,
    timeout: Duration,
) -> Result<bool, SystemOperationError> {
    let deadline = Instant::now() + timeout;
    loop {
        if application_is_running(path)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn application_is_running(expected_path: &Path) -> Result<bool, SystemOperationError> {
    let pool_class = objc_class("NSAutoreleasePool")?;
    let workspace_class = objc_class("NSWorkspace")?;
    // SAFETY: the selectors below match documented AppKit/Foundation method
    // signatures. The pool bounds Objective-C autoreleased objects created on
    // this worker thread, and returned C strings live until the pool drains.
    unsafe {
        let pool = send_id(pool_class, selector("alloc")?);
        let pool = send_id(pool, selector("init")?);
        let workspace = send_id(workspace_class, selector("sharedWorkspace")?);
        let applications = if workspace.is_null() {
            std::ptr::null_mut()
        } else {
            send_id(workspace, selector("runningApplications")?)
        };
        let count = if applications.is_null() {
            0
        } else {
            send_usize(applications, selector("count")?)
        };
        let mut running = false;
        for index in 0..count {
            let application = send_id_usize(applications, selector("objectAtIndex:")?, index);
            let bundle_url = if application.is_null() {
                std::ptr::null_mut()
            } else {
                send_id(application, selector("bundleURL")?)
            };
            let bundle_path = if bundle_url.is_null() {
                std::ptr::null_mut()
            } else {
                send_id(bundle_url, selector("path")?)
            };
            if bundle_path.is_null() {
                continue;
            }
            let c_path = send_cstr(bundle_path, selector("UTF8String")?);
            if !c_path.is_null() {
                let running_path =
                    PathBuf::from(CStr::from_ptr(c_path).to_string_lossy().into_owned());
                if running_path == expected_path
                    || running_path
                        .canonicalize()
                        .is_ok_and(|canonical| canonical == expected_path)
                {
                    running = true;
                    break;
                }
            }
        }
        send_void(pool, selector("drain")?);
        Ok(running)
    }
}

fn objc_class(name: &str) -> Result<*mut c_void, SystemOperationError> {
    let name = CString::new(name).expect("Objective-C class names contain no NUL");
    // SAFETY: objc_getClass reads the terminated class name and returns either
    // a valid class object or null.
    let class = unsafe { objc_getClass(name.as_ptr()) };
    if class.is_null() {
        Err(SystemOperationError::new(
            "required macOS class is unavailable",
        ))
    } else {
        Ok(class)
    }
}

fn selector(name: &str) -> Result<*mut c_void, SystemOperationError> {
    let name = CString::new(name).expect("Objective-C selectors contain no NUL");
    // SAFETY: sel_registerName copies/interns the terminated selector name.
    let selector = unsafe { sel_registerName(name.as_ptr()) };
    if selector.is_null() {
        Err(SystemOperationError::new(
            "required macOS selector is unavailable",
        ))
    } else {
        Ok(selector)
    }
}

unsafe fn send_id(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void {
    // SAFETY: the caller selected a no-argument Objective-C method returning id.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_id_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *mut c_void,
) -> *mut c_void {
    // SAFETY: the caller selected a one-id-argument method returning id.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_id_usize(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: usize,
) -> *mut c_void {
    // SAFETY: the caller selected a NSUInteger-argument method returning id.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_id_cstr(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *const c_char,
) -> *mut c_void {
    // SAFETY: the caller selected a C-string-argument method returning id.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

unsafe fn send_bool_id_id_usize_id_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    first: *mut c_void,
    second: *mut c_void,
    options: usize,
    third: *mut c_void,
    fourth: *mut c_void,
) -> bool {
    // SAFETY: the caller supplies NSWorkspace's documented five-argument URL
    // launch selector with the matching Objective-C ABI.
    let function: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut c_void,
        usize,
        *mut c_void,
        *mut c_void,
    ) -> bool = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, first, second, options, third, fourth) }
}

unsafe fn send_bool_id(
    receiver: *mut c_void,
    selector: *mut c_void,
    argument: *mut c_void,
) -> bool {
    // SAFETY: the caller selected a one-id-argument method returning BOOL.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i8 =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) != 0 }
}

unsafe fn send_usize(receiver: *mut c_void, selector: *mut c_void) -> usize {
    // SAFETY: the caller selected a no-argument Objective-C method returning NSUInteger.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_cstr(receiver: *mut c_void, selector: *mut c_void) -> *const c_char {
    // SAFETY: the caller selected NSString's no-argument UTF8String method.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *const c_char =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_void(receiver: *mut c_void, selector: *mut c_void) {
    // SAFETY: the caller selected a no-argument Objective-C method returning void.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector) }
}

unsafe fn send_void_id(receiver: *mut c_void, selector: *mut c_void, argument: *mut c_void) {
    // SAFETY: the caller selected a one-id-argument Objective-C method returning void.
    let function: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { function(receiver, selector, argument) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunoto_system::{
        CapabilityDispatcher, CapabilityInput, NativeCapabilityCall, NativeCapabilityDispatcher,
        ObservationEvidence, PendingSuggestionSet, SystemIntent, TargetHint, ValidatedHttpUrl,
    };

    #[test]
    fn discovery_finds_bundles_without_descending_into_them() {
        let root =
            std::env::temp_dir().join(format!("sunoto-app-discovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Utilities/Test Utility.app/Contents/Nested.app")).unwrap();

        let found = discover_application_bundles(std::slice::from_ref(&root));
        assert_eq!(found.len(), 1);
        assert_eq!(
            application_display_name(&found[0]).as_deref(),
            Some("Test Utility")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unified_provider_finds_project_and_rejects_disappearing_target_before_open() {
        let root = std::env::temp_dir().join(format!(
            "sunoto-macos-target-provider-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("who-else-is-free/.git")).unwrap();
        fs::write(root.join("who-else-is-free/README.md"), "fixture").unwrap();
        let mut platform = SystemPlatform::with_search_roots(vec![root.clone()]);
        let intent = SystemIntent::OpenTarget {
            query: "who-else-is-free".into(),
            hint: TargetHint::Auto,
        };
        let candidates = platform.target_candidates(&intent).unwrap();
        let project = candidates
            .into_iter()
            .find(|candidate| candidate.candidate.kind == CandidateKind::Project)
            .expect("fixture project should resolve");
        assert_eq!(project.candidate.display_name, "who-else-is-free");
        assert!(
            !project
                .candidate
                .stable_id
                .contains(root.to_string_lossy().as_ref())
        );
        fs::remove_dir_all(root.join("who-else-is-free")).unwrap();
        assert!(platform.execute(&project.action).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_host_known_workspace_finds_real_project_when_present() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let project = home.join("workspace/who-else-is-free");
        if !project.is_dir() {
            return;
        }
        let mut platform = SystemPlatform::default();
        let candidates = platform
            .target_candidates(&SystemIntent::OpenTarget {
                query: "who-else-is-free".into(),
                hint: TargetHint::Auto,
            })
            .unwrap();
        assert!(candidates.iter().any(|candidate| {
            candidate.candidate.display_name == "who-else-is-free"
                && candidate.candidate.kind == CandidateKind::Project
        }));
    }

    /// Deliberately opt-in: opens the existing harmless project in the
    /// installed editor through NSWorkspace after the combined selection.
    #[test]
    #[ignore = "opens the real who-else-is-free project in the selected VS Code"]
    fn live_project_in_selected_editor() {
        let mut dispatcher = NativeCapabilityDispatcher::new(SystemPlatform::default());
        let intent = SystemIntent::OpenTargetWithApplication {
            query: "who-else-is-free".into(),
            application_query: "VS Code".into(),
        };
        let found =
            dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                intent: intent.clone(),
            }));
        assert!(found.is_success(), "{found:?}");
        let candidates = dispatcher.take_last_candidates();
        let mut palette = PendingSuggestionSet::build(2, &intent, candidates, 10);
        let suggestion_id = palette
            .suggestions()
            .iter()
            .find(|suggestion| suggestion.title == "Open who-else-is-free")
            .expect("real project and VS Code should resolve")
            .suggestion_id
            .clone();
        let action = palette.select(2, &suggestion_id).unwrap();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let opened = dispatcher.dispatch(&input);
        assert!(opened.is_success(), "{opened:?}");
        assert!(matches!(
            opened.evidence,
            ObservationEvidence::LocalTargetOpened {
                kind: CandidateKind::Project,
                ..
            }
        ));
    }

    /// Deliberately opt-in: opens/reveals only disposable targets created
    /// under the test's approved temporary root.
    #[test]
    #[ignore = "opens and reveals disposable file/folder fixtures"]
    fn live_disposable_file_folder_and_reveal() {
        let root =
            std::env::temp_dir().join(format!("sunoto-live-local-targets-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sunoto-test-folder")).unwrap();
        fs::write(
            root.join("sunoto-test-document.txt"),
            "Sunoto harmless fixture\n",
        )
        .unwrap();

        for (intent, expected) in [
            (
                SystemIntent::OpenFileByQuery {
                    query: "sunoto-test-document".into(),
                },
                ActionSuccessEvidence::LocalTargetOpened,
            ),
            (
                SystemIntent::RevealFileByQuery {
                    query: "sunoto-test-document".into(),
                },
                ActionSuccessEvidence::LocalTargetRevealed,
            ),
            (
                SystemIntent::OpenFolder {
                    query: "sunoto-test-folder".into(),
                },
                ActionSuccessEvidence::LocalTargetOpened,
            ),
        ] {
            let mut platform = SystemPlatform::with_search_roots(vec![root.clone()]);
            let candidates = platform.target_candidates(&intent).unwrap();
            let mut palette = PendingSuggestionSet::build(3, &intent, candidates, 5);
            let suggestion_id = palette.suggestions()[0].suggestion_id.clone();
            let action = palette.select(3, &suggestion_id).unwrap();
            let result = platform.execute(&action).unwrap();
            assert_eq!(result.evidence, expected);
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Deliberately opt-in: this invokes NSWorkspace and opens harmless URLs
    /// in the user's default browser and installed Chrome. It remains ignored
    /// in normal CI and is exercised only after an explicit live-test request.
    #[test]
    #[ignore = "opens harmless URLs through native macOS browser APIs"]
    fn live_default_and_selected_chrome_navigation() {
        let default_url =
            ValidatedHttpUrl::parse_spoken("https://example.com/?sunoto=default").unwrap();
        let mut default_dispatcher = NativeCapabilityDispatcher::new(SystemPlatform::default());
        let default_opened =
            default_dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
                url: default_url.clone(),
                browser: None,
            }));
        assert!(default_opened.is_success(), "{default_opened:?}");

        let url = ValidatedHttpUrl::parse_spoken("https://example.com/?sunoto=chrome").unwrap();
        let mut dispatcher = NativeCapabilityDispatcher::new(SystemPlatform::default());
        let found = dispatcher.dispatch(&CapabilityInput::Native(
            NativeCapabilityCall::FindApplication {
                query: "Chrome".into(),
            },
        ));
        assert!(found.is_success(), "{found:?}");
        let candidates = dispatcher.take_last_candidates();
        let intent = SystemIntent::OpenTarget {
            query: "Chrome".into(),
            hint: TargetHint::Application,
        };
        let mut palette = PendingSuggestionSet::build(1, &intent, candidates, 5);
        let suggestion_id = {
            let selection = palette.suggestions().first().expect("Chrome is installed");
            assert_eq!(selection.title, "Open Google Chrome");
            selection.suggestion_id.clone()
        };
        let action = palette.select(1, &suggestion_id).unwrap();
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let CapabilityInput::Native(NativeCapabilityCall::OpenApplication { target }) = input
        else {
            panic!("browser selection did not create an opaque target");
        };
        let opened = dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
            url: url.clone(),
            browser: Some(target),
        }));
        assert!(opened.is_success(), "{opened:?}");
        assert!(matches!(
            opened.evidence,
            ObservationEvidence::UrlOpened { url: observed, .. } if observed == url.as_str()
        ));
    }
}

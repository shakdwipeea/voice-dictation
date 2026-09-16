//! Bounded local target discovery for the Voice Spotlight foundation.
//!
//! This module never opens a path. It canonicalizes configured roots, walks
//! only inside them, skips hidden/system-like locations, caps work/results,
//! and returns private canonical paths for a later platform target store.

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalTargetKind {
    File,
    Folder,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTarget {
    pub display_name: String,
    pub kind: LocalTargetKind,
    pub evidence: String,
    pub recency_score: u16,
    canonical_path: PathBuf,
}

impl LocalTarget {
    /// This is intentionally crate-private: only a platform resolver can
    /// retain a path in its opaque target store. UI/planner data uses names,
    /// kinds, and evidence rather than filesystem paths.
    /// Native platform providers retain this path in their private target
    /// store; it is never serialized to the palette or planner.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

#[derive(Debug, Clone)]
pub struct FilesystemTargetProvider {
    roots: Vec<PathBuf>,
    max_entries: usize,
    max_results: usize,
}

impl FilesystemTargetProvider {
    pub fn new(
        roots: impl IntoIterator<Item = PathBuf>,
        max_entries: usize,
        max_results: usize,
    ) -> Result<Self, String> {
        if max_entries == 0 || max_results == 0 || max_results > 50 {
            return Err("target-search limits must be bounded and non-zero".into());
        }
        let mut seen = HashSet::new();
        let roots = roots
            .into_iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| root.is_dir() && seen.insert(root.clone()))
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err("no usable approved search roots".into());
        }
        Ok(Self {
            roots,
            max_entries,
            max_results,
        })
    }

    pub fn find(&self, query: &str) -> Vec<LocalTarget> {
        self.find_bounded(query, Duration::from_secs(2), || false)
    }

    /// Run the portable fallback with both a work cap and a wall-clock /
    /// cancellation boundary. Platform indexed providers may feed additional
    /// paths through `candidate_from_path`, but all results still pass the
    /// same approved-root and safe-type checks.
    pub fn find_bounded(
        &self,
        query: &str,
        timeout: Duration,
        mut cancelled: impl FnMut() -> bool,
    ) -> Vec<LocalTarget> {
        let query = normalize(query);
        if query.is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
            return Vec::new();
        }
        let deadline = Instant::now() + timeout;
        let mut queue = self.roots.iter().cloned().collect::<VecDeque<_>>();
        let mut visited = 0usize;
        let mut results = Vec::new();
        while let Some(directory) = queue.pop_front() {
            if cancelled()
                || Instant::now() >= deadline
                || visited >= self.max_entries
                || results.len() >= self.max_results.saturating_mul(4)
            {
                break;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if cancelled() || Instant::now() >= deadline || visited >= self.max_entries {
                    break;
                }
                visited += 1;
                let name = entry.file_name().to_string_lossy().into_owned();
                if excluded_name(&name) {
                    continue;
                }
                let Ok(canonical) = entry.path().canonicalize() else {
                    continue;
                };
                if !self.roots.iter().any(|root| canonical.starts_with(root)) {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    let target_kind = project_kind(&canonical).unwrap_or(LocalTargetKind::Folder);
                    if score(&query, &name) > 0 {
                        results.push(LocalTarget {
                            display_name: name,
                            kind: target_kind,
                            evidence: match target_kind {
                                LocalTargetKind::Project => "approved root; project marker".into(),
                                _ => "approved root; folder name match".into(),
                            },
                            recency_score: recency_score(&canonical),
                            canonical_path: canonical.clone(),
                        });
                    }
                    queue.push_back(canonical);
                } else if kind.is_file() && safe_document(&canonical) && score(&query, &name) > 0 {
                    results.push(LocalTarget {
                        display_name: name,
                        kind: LocalTargetKind::File,
                        evidence: "approved root; safe filename match".into(),
                        recency_score: recency_score(&canonical),
                        canonical_path: canonical,
                    });
                }
            }
        }
        results.sort_by_key(|target| {
            (
                Reverse(score(&query, &target.display_name)),
                target.canonical_path.components().count(),
                target.display_name.to_ascii_lowercase(),
            )
        });
        results.truncate(self.max_results);
        results
    }

    /// Convert an indexed-provider hit into the same safe local target model.
    pub fn candidate_from_path(&self, query: &str, path: &Path) -> Option<LocalTarget> {
        let query = normalize(query);
        if query.is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
            return None;
        }
        let canonical = path.canonicalize().ok()?;
        let (kind, root) = self.classify(&canonical)?;
        let display_name = canonical.file_name()?.to_string_lossy().into_owned();
        (score(&query, &display_name) > 0).then(|| LocalTarget {
            display_name,
            kind,
            evidence: match kind {
                LocalTargetKind::Project => "indexed name match; approved root; project marker",
                LocalTargetKind::Folder => "indexed name match; approved root; folder",
                LocalTargetKind::File => "indexed name match; approved root; safe file type",
            }
            .into(),
            recency_score: recency_score(&canonical),
            canonical_path: canonical
                .strip_prefix(root)
                .map(|relative| root.join(relative))
                .unwrap_or(canonical),
        })
    }

    /// Revalidate an opaque stored path immediately before a native action.
    pub fn revalidate(
        &self,
        path: &Path,
        expected_kind: LocalTargetKind,
    ) -> Result<PathBuf, String> {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("target disappeared: {error}"))?;
        if canonical != path {
            return Err("target changed after it was selected".into());
        }
        let (actual_kind, _) = self
            .classify(&canonical)
            .ok_or_else(|| "target is outside approved roots or no longer safe".to_string())?;
        if actual_kind != expected_kind {
            return Err("target kind changed after it was selected".into());
        }
        Ok(canonical)
    }

    fn classify<'a>(&'a self, canonical: &Path) -> Option<(LocalTargetKind, &'a Path)> {
        let root = self
            .roots
            .iter()
            .find(|root| canonical.starts_with(root.as_path()))?;
        let relative = canonical.strip_prefix(root).ok()?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| component.as_os_str().to_str().is_none_or(excluded_name))
        {
            return None;
        }
        if canonical.is_dir() {
            Some((
                project_kind(canonical).unwrap_or(LocalTargetKind::Folder),
                root,
            ))
        } else if canonical.is_file() && safe_document(canonical) {
            Some((LocalTargetKind::File, root))
        } else {
            None
        }
    }
}

fn excluded_name(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "Library" | "node_modules" | "target" | "__pycache__" | ".Trash"
        )
}

fn safe_document(path: &Path) -> bool {
    const SAFE: &[&str] = &[
        "txt", "md", "pdf", "doc", "docx", "rtf", "csv", "json", "toml", "yaml", "yml",
    ];
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let lower = name.to_ascii_lowercase();
    if [
        ".sh.",
        ".command.",
        ".app.",
        ".pkg.",
        ".dmg.",
        ".exe.",
        ".bat.",
        ".ps1.",
    ]
    .iter()
    .any(|unsafe_marker| lower.contains(unsafe_marker))
        || fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    {
        return false;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SAFE.iter().any(|safe| extension.eq_ignore_ascii_case(safe)))
}

fn project_kind(path: &Path) -> Option<LocalTargetKind> {
    [
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "Makefile",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
    .then_some(LocalTargetKind::Project)
}

fn recency_score(path: &Path) -> u16 {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return 0;
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    match age.as_secs() / 86_400 {
        0..=1 => 50,
        2..=7 => 30,
        8..=30 => 15,
        31..=90 => 5,
        _ => 0,
    }
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn score(query: &str, name: &str) -> u16 {
    let name = normalize(name);
    if name == query {
        1_000
    } else if name.strip_suffix(".md").is_some_and(|stem| stem == query) {
        950
    } else if name.starts_with(query) {
        800
    } else if query.split_whitespace().all(|token| name.contains(token)) {
        600
    } else if name.contains(query) {
        400
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sunoto-targets-{}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("who-else-is-free/.git")).unwrap();
        fs::write(root.join("who-else-is-free/README.md"), "fixture").unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden/secret.txt"), "no").unwrap();
        fs::write(root.join("run.sh"), "no").unwrap();
        root
    }

    #[test]
    fn finds_projects_and_safe_files_only_inside_approved_root() {
        let root = fixture();
        let provider = FilesystemTargetProvider::new(vec![root.clone()], 100, 5).unwrap();
        let found = provider.find("who else free");
        assert_eq!(found[0].kind, LocalTargetKind::Project);
        assert_eq!(found[0].display_name, "who-else-is-free");
        assert!(
            found[0]
                .canonical_path()
                .starts_with(root.canonicalize().unwrap())
        );
        assert!(provider.find("secret").is_empty());
        assert!(provider.find("run").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_roots_and_unbounded_queries_fail_closed() {
        assert!(
            FilesystemTargetProvider::new(vec![PathBuf::from("/no/such/root")], 20, 5).is_err()
        );
        let root = fixture();
        let provider = FilesystemTargetProvider::new(vec![root.clone()], 1, 5).unwrap();
        assert!(provider.find(&"a".repeat(201)).is_empty());
        assert!(provider.find("x\ny").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlink_escape_executables_and_target_mutation_fail_closed() {
        use std::os::unix::fs::symlink;

        let root = fixture();
        let outside = root.with_extension("outside");
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("outside.txt"), "no").unwrap();
        symlink(outside.join("outside.txt"), root.join("escape.txt")).unwrap();
        fs::write(root.join("payload.sh.txt"), "no").unwrap();
        fs::write(root.join("executable.txt"), "no").unwrap();
        let mut permissions = fs::metadata(root.join("executable.txt"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(root.join("executable.txt"), permissions).unwrap();
        let provider = FilesystemTargetProvider::new(vec![root.clone()], 100, 10).unwrap();
        assert!(provider.find("escape").is_empty());
        assert!(provider.find("payload").is_empty());
        assert!(provider.find("executable").is_empty());

        let project = provider.find("who else free").remove(0);
        fs::remove_dir_all(project.canonical_path()).unwrap();
        assert!(
            provider
                .revalidate(project.canonical_path(), LocalTargetKind::Project)
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn bounded_search_honors_cancellation_and_result_caps() {
        let root = fixture();
        for index in 0..20 {
            fs::write(root.join(format!("report-{index}.txt")), "safe").unwrap();
        }
        let provider = FilesystemTargetProvider::new(vec![root.clone()], 100, 3).unwrap();
        assert_eq!(provider.find("report").len(), 3);
        assert!(
            provider
                .find_bounded("report", Duration::from_secs(1), || true)
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fixture_quality_gate_has_full_top_five_recall_zero_false_actions_and_bounded_latency() {
        let root = fixture();
        fs::create_dir_all(root.join("quarterly reports")).unwrap();
        fs::write(root.join("quarterly report.txt"), "safe").unwrap();
        let provider = FilesystemTargetProvider::new(vec![root.clone()], 1_000, 5).unwrap();
        let started = Instant::now();
        let cases = [
            ("who-else-is-free", "who-else-is-free"),
            ("quarterly report", "quarterly report.txt"),
            ("quarterly reports", "quarterly reports"),
        ];
        let mut top_one = 0usize;
        let mut top_five = 0usize;
        for (query, expected) in cases {
            let found = provider.find(query);
            top_one += usize::from(
                found
                    .first()
                    .is_some_and(|item| item.display_name == expected),
            );
            top_five += usize::from(found.iter().any(|item| item.display_name == expected));
        }
        assert_eq!(top_one, cases.len(), "fixture top-1 recall must be 100%");
        assert_eq!(top_five, cases.len(), "fixture top-5 recall must be 100%");
        assert!(provider.find("definitely-not-present").is_empty());
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "small fixture provider p95 proxy must remain below 500ms"
        );
        let cancelled = Instant::now();
        assert!(
            provider
                .find_bounded("report", Duration::from_secs(1), || true)
                .is_empty()
        );
        assert!(cancelled.elapsed() < Duration::from_millis(50));
        let _ = fs::remove_dir_all(root);
    }
}

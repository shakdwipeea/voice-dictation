//! Focused-tool context: detect a developer tool running inside the focused
//! window's process subtree and the working directory it operates in, then
//! index that directory's files so spoken file references can be resolved.
//!
//! Linux-only and dependency-free: window PID comes from the daemon (X11
//! `_NET_WM_PID` or `hyprctl`), everything else is read from `/proc` and `git`.
//! Nothing here is on the dictation latency path — the daemon calls it off the
//! hot path and caches the result per working directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// What the detector needs to recognize one agent: a `name` to report back
/// (which selects the reference syntax) and the `/proc/<pid>/comm` value the
/// agent runs as. The daemon builds these from the configured registry, so
/// adding an agent is config, not code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProcess {
    /// Configured agent identifier, echoed back in `DetectedAgent::name`.
    pub name: String,
    /// The `/proc/<pid>/comm` value to match (e.g. `claude`, `gemini`).
    pub comm: String,
}

/// A coding agent found running in the focused window's process subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgent {
    /// The configured agent `name` that matched (selects the reference syntax).
    pub name: String,
    /// The working directory the agent is running in.
    pub cwd: PathBuf,
}

/// Upper bound on the `/proc` sweep, so a runaway process table can never stall
/// the caller. Real systems have far fewer agent processes than this.
const MAX_AGENT_CANDIDATES: usize = 64;
/// Bound on the parent-chain walk when proving descent.
const MAX_ANCESTRY_DEPTH: usize = 64;

/// Detect a coding agent running inside the process subtree of `window_pid`
/// (the PID that owns the focused window — for a terminal, the emulator
/// process). `agents` is the configured registry: each entry pairs a `name`
/// with the `/proc/<pid>/comm` value to match, so new agents are added by
/// config rather than code.
///
/// gnome-terminal runs every tab and window under a single server process, so
/// `window_pid` alone cannot say which session is focused. When more than one
/// agent session matches, we break the tie by terminal activity — the focused
/// session is the one whose pts was most recently used (see `tty_activity`) —
/// which is agent-agnostic and needs no per-agent hook. A single candidate is
/// used directly; on a tie, or with no activity to compare, we refuse to guess.
pub fn detect_agent(window_pid: u32, agents: &[AgentProcess]) -> Option<DetectedAgent> {
    let candidates = agent_candidates(window_pid, agents);
    select_candidate(candidates).map(|chosen| DetectedAgent {
        name: chosen.name,
        cwd: chosen.cwd,
    })
}

/// One agent session under the focused window: which agent matched, its working
/// directory, and when its terminal was last active (used only to break ties).
struct Candidate {
    name: String,
    cwd: PathBuf,
    activity: Option<SystemTime>,
}

/// Every configured agent session under `window_pid`, found in a single `/proc`
/// pass and de-duplicated by (name, cwd) keeping the most recent activity.
fn agent_candidates(window_pid: u32, agents: &[AgentProcess]) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return candidates;
    };
    for entry in entries.flatten() {
        if candidates.len() >= MAX_AGENT_CANDIDATES {
            break;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|raw| raw.parse::<u32>().ok())
        else {
            continue; // non-numeric /proc entries (self, cpuinfo, ...)
        };
        let Some(comm) = process_comm(pid) else {
            continue;
        };
        let Some(spec) = agents.iter().find(|agent| agent.comm == comm) else {
            continue;
        };
        if !is_descendant_of(pid, window_pid) {
            continue;
        }
        let Some(cwd) = process_cwd(pid) else {
            continue;
        };
        let activity = tty_activity(pid);
        match candidates
            .iter_mut()
            .find(|candidate| candidate.name == spec.name && candidate.cwd == cwd)
        {
            Some(existing) => existing.activity = max_time(existing.activity, activity),
            None => candidates.push(Candidate {
                name: spec.name.clone(),
                cwd,
                activity,
            }),
        }
    }
    candidates
}

/// Choose the session to resolve against: the sole candidate, or — when several
/// sessions share a terminal — the one whose terminal was most recently active.
/// Refuses to guess on a tie, or when no activity is known, the same safe
/// fallback the resolver uses elsewhere for an undecidable match.
fn select_candidate(mut candidates: Vec<Candidate>) -> Option<Candidate> {
    match candidates.len() {
        0 => None,
        1 => candidates.pop(),
        _ => {
            let newest = candidates.iter().filter_map(|c| c.activity).max()?;
            let mut at_newest = candidates.into_iter().filter(|c| c.activity == Some(newest));
            match (at_newest.next(), at_newest.next()) {
                (Some(only), None) => Some(only),
                _ => None, // tie → refuse to guess
            }
        }
    }
}

/// Most recent activity on `pid`'s controlling terminal: the newer of its pts
/// device's access (input) and modify (output) times. The focused terminal is
/// the one the user is interacting with, so its pts is the most recently
/// active. Agent-agnostic — it reads the terminal device, not the agent — and
/// replaces the old per-agent cwd hook. `None` when the process has no pts, in
/// which case it cannot win a tie.
fn tty_activity(pid: u32) -> Option<SystemTime> {
    let pts = process_pts(pid)?;
    let meta = fs::metadata(&pts).ok()?;
    max_time(meta.accessed().ok(), meta.modified().ok())
}

/// The `/dev/pts/N` slave backing `pid`'s stdio — stdin, then stdout, then
/// stderr, whichever first points at a pts.
fn process_pts(pid: u32) -> Option<PathBuf> {
    [0, 1, 2].into_iter().find_map(|fd| {
        let target = fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok()?;
        target.to_str()?.starts_with("/dev/pts/").then_some(target)
    })
}

/// The later of two optional timestamps.
fn max_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The process name from `/proc/<pid>/comm` (the kernel-truncated comm value).
fn process_comm(pid: u32) -> Option<String> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end_matches('\n').to_string())
}

/// Parent PID parsed from `/proc/<pid>/status`.
fn parent_pid(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// True if `pid` is `ancestor` or transitively descends from it.
fn is_descendant_of(mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if pid == ancestor {
            return true;
        }
        match parent_pid(pid) {
            Some(parent) if parent != 0 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

/// The working directory of `pid` from the `/proc/<pid>/cwd` symlink.
fn process_cwd(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Directories that never contain interesting source and would only bloat the
/// index; used by the non-git fallback walk.
const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", "dist", "build", "__pycache__"];

/// A repo-relative file listing for a working directory, used to resolve spoken
/// file references against real paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIndex {
    /// The working directory the listing is relative to.
    pub root: PathBuf,
    /// Repo-relative, forward-slash paths.
    pub files: Vec<String>,
}

impl FileIndex {
    /// Build the index for `root`: `git ls-files` when it is a git working
    /// tree (fast, honors `.gitignore`), otherwise a bounded recursive walk.
    pub fn build(root: &Path, max_files: usize) -> Self {
        let mut files = git_files(root).unwrap_or_else(|| walk_files(root, max_files));
        files.truncate(max_files);
        Self {
            root: root.to_path_buf(),
            files,
        }
    }
}

/// Tracked + untracked-but-not-ignored files via git, NUL-separated so paths
/// with spaces or newlines survive. `None` when `root` is not a git tree.
fn git_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = output
        .stdout
        .split(|&byte| byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    Some(files)
}

/// Bounded, symlink-free recursive walk for non-git directories.
fn walk_files(root: &Path, max_files: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max_files {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue; // skips broken symlinks; is_dir/is_file are false for symlinks
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with(".venv") {
                    continue;
                }
                stack.push(entry.path());
            } else if file_type.is_file()
                && let Ok(relative) = entry.path().strip_prefix(root)
            {
                out.push(relative.to_string_lossy().replace('\\', "/"));
                if out.len() >= max_files {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_is_a_descendant_of_itself_and_its_parent() {
        let me = std::process::id();
        assert!(is_descendant_of(me, me));
        if let Some(parent) = parent_pid(me) {
            assert!(is_descendant_of(me, parent));
        }
        // No process descends from an impossible PID.
        assert!(!is_descendant_of(me, u32::MAX - 1));
    }

    #[test]
    fn own_comm_is_readable() {
        let comm = process_comm(std::process::id());
        assert!(comm.is_some_and(|name| !name.is_empty()));
    }

    #[test]
    fn select_candidate_uses_terminal_activity_only_to_break_ties() {
        use std::time::Duration;
        let at = |secs| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        let candidate = |name: &str, cwd: &str, activity| Candidate {
            name: name.to_string(),
            cwd: PathBuf::from(cwd),
            activity,
        };

        // Sole candidate: used directly, activity irrelevant.
        assert_eq!(
            select_candidate(vec![candidate("claude", "/x/voice-dictation", None)])
                .map(|chosen| chosen.cwd),
            Some(PathBuf::from("/x/voice-dictation"))
        );
        // Several candidates: the most recently active terminal wins, across
        // agents as well as sessions.
        assert_eq!(
            select_candidate(vec![
                candidate("claude", "/x/vaani-livekit", at(10)),
                candidate("gemini", "/x/voice-dictation", at(20)),
            ])
            .map(|chosen| chosen.cwd),
            Some(PathBuf::from("/x/voice-dictation"))
        );
        // Several candidates tied on activity: refuse to guess.
        assert_eq!(
            select_candidate(vec![
                candidate("claude", "/x/vaani-livekit", at(5)),
                candidate("claude", "/x/voice-dictation", at(5)),
            ])
            .map(|chosen| chosen.cwd),
            None
        );
        // Several candidates, no activity to compare: refuse to guess.
        assert_eq!(
            select_candidate(vec![
                candidate("claude", "/x/vaani-livekit", None),
                candidate("claude", "/x/voice-dictation", None),
            ])
            .map(|chosen| chosen.cwd),
            None
        );
    }

    #[test]
    fn walk_lists_files_and_skips_noise_dirs() {
        let dir = std::env::temp_dir().join(format!("sunoto-context-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::write(dir.join("Cargo.toml"), "x").unwrap();
        fs::write(dir.join("src/lib.rs"), "x").unwrap();
        fs::write(dir.join("target/debug/junk.o"), "x").unwrap();

        let mut files = walk_files(&dir, 100);
        files.sort();
        assert_eq!(files, vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()]);

        // max_files is honored.
        assert!(FileIndex::build(&dir, 1).files.len() <= 1);

        let _ = fs::remove_dir_all(&dir);
    }
}

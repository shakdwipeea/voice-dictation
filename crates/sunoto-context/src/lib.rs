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

/// A developer tool detected in the focused window's process subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevTool {
    /// Claude Code CLI (the process is literally named `claude`), with the
    /// working directory it is running in.
    ClaudeCode { cwd: PathBuf },
}

/// Upper bound on the `/proc` sweep, so a runaway process table can never stall
/// the caller. Real systems have far fewer processes named `claude`.
const MAX_CLAUDE_CANDIDATES: usize = 64;
/// Bound on the parent-chain walk when proving descent.
const MAX_ANCESTRY_DEPTH: usize = 64;

/// Detect a supported developer tool running inside the process subtree of
/// `window_pid` (the PID that owns the focused window — for a terminal, the
/// emulator process).
///
/// gnome-terminal runs every tab and window under a single server process, so
/// `window_pid` alone cannot say which Claude session is focused. We gather
/// every `claude` descendant's working directory and, when there is more than
/// one, break the tie with the active-cwd hint the Claude Code hook records
/// (the most recently active session). A single candidate needs no hint; with
/// several and no usable hint we refuse to guess.
pub fn detect_tool(window_pid: u32) -> Option<DevTool> {
    let cwds = claude_descendant_cwds(window_pid);
    select_cwd(&cwds, read_active_cwd().as_deref()).map(|cwd| DevTool::ClaudeCode { cwd })
}

/// Working directories of every `claude` process under `window_pid`, de-duped.
fn claude_descendant_cwds(window_pid: u32) -> Vec<PathBuf> {
    let mut cwds: Vec<PathBuf> = Vec::new();
    for pid in processes_named("claude") {
        if is_descendant_of(pid, window_pid)
            && let Some(cwd) = process_cwd(pid)
            && !cwds.contains(&cwd)
        {
            cwds.push(cwd);
        }
    }
    cwds
}

/// Pick the working directory to resolve against: the sole candidate, or the
/// one the active-cwd hint points to when several Claude sessions share a
/// terminal. Returns `None` rather than guess when the hint cannot decide.
fn select_cwd(cwds: &[PathBuf], active: Option<&Path>) -> Option<PathBuf> {
    match cwds {
        [] => None,
        [only] => Some(only.clone()),
        many => active
            .filter(|hint| many.iter().any(|cwd| cwd.as_path() == *hint))
            .map(Path::to_path_buf),
    }
}

/// Path of the active-cwd hint file written by `bin/sunoto-claude-cwd-hook.sh`:
/// `$XDG_RUNTIME_DIR/sunoto/claude-active-cwd`.
pub fn active_cwd_file() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("sunoto/claude-active-cwd")
}

/// The most recently recorded active Claude working directory, if any.
fn read_active_cwd() -> Option<PathBuf> {
    let raw = fs::read_to_string(active_cwd_file()).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// PIDs whose `/proc/<pid>/comm` equals `name`, capped at a sane maximum.
fn processes_named(name: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|raw| raw.parse::<u32>().ok())
        else {
            continue; // non-numeric /proc entries (self, cpuinfo, ...)
        };
        if process_comm(pid).as_deref() == Some(name) {
            pids.push(pid);
            if pids.len() >= MAX_CLAUDE_CANDIDATES {
                break;
            }
        }
    }
    pids
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
    fn select_cwd_uses_the_active_hint_only_to_break_ties() {
        let voice = PathBuf::from("/x/voice-dictation");
        let vaani = PathBuf::from("/x/vaani-livekit");
        // Sole candidate: used directly, hint irrelevant.
        assert_eq!(
            select_cwd(std::slice::from_ref(&voice), None),
            Some(voice.clone())
        );
        // Several candidates: the hint picks the matching one.
        assert_eq!(
            select_cwd(&[vaani.clone(), voice.clone()], Some(&voice)),
            Some(voice.clone())
        );
        // Several candidates, hint not among them: refuse to guess.
        assert_eq!(
            select_cwd(&[vaani.clone(), voice.clone()], Some(Path::new("/x/other"))),
            None
        );
        // Several candidates, no hint: refuse to guess.
        assert_eq!(select_cwd(&[vaani, voice], None), None);
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

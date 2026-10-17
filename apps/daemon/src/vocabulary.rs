//! Words to favour while transcribing (the word list).
//!
//! When a dictation starts, a worker thread gathers unusual words from around
//! the user, and the daemon sends them to the ASR sidecar, which favours them
//! while it decodes (`services/asr/word_boost.py`). Three sources, strongest
//! first:
//!
//! - screen: the focused text box or terminal screen, and the window title;
//! - project: folder and file names of the project being worked on, found
//!   from the focused window (a terminal's folder, an editor's title);
//! - everywhere: the user's project folders and installed apps.
//!
//! "Unusual" means the Mac's English word list does not know the word, so an
//! ordinary word never gets a bonus. After the transcript arrives,
//! [`fix_near_misses`] swaps a word one letter off a listed word ("Ghosty"
//! for "Ghostty") for the listed word. The words live in memory for one
//! dictation and are never logged or written anywhere.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::events::DaemonEvent;

/// The system's English word list (Webster's Second, on every Mac).
const DICTIONARY: &str = "/usr/share/dict/words";
/// Everyday and tech words newer than that list ("email", "dataset").
const MODERN_WORDS: &str = include_str!("modern_words.txt");
/// Folders under the home folder whose subfolders are the user's projects.
const PROJECT_ROOTS: &[&str] = &[
    "workspace", "Developer", "Projects", "projects", "code", "Code", "src", "dev", "repos", "git",
    "GitHub", "Documents/GitHub", "Documents/Projects",
];
const APP_FOLDERS: &[&str] = &["/Applications", "~/Applications"];
/// Folders a project walk never enters: build output, dependencies, caches.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", "build", "dist", "out", "vendor", "venv", "env", "__pycache__",
    "Pods", "DerivedData", "coverage", "tmp",
];
/// Characters of the focused text kept around the cursor.
pub(crate) const SCREEN_CHARS: usize = 20_000;
const SCREEN_MAX: usize = 150;
const PROJECT_MAX: usize = 200;
const EVERYWHERE_MAX: usize = 250;
const PROJECT_WALK_MAX: usize = 4_000;
/// Folder and app names need five letters: shorter names ("auth", "fly")
/// sound like everyday words too often to be favoured in every dictation.
const NAME_MIN_LETTERS: usize = 5;
const SCREEN_MIN_LETTERS: usize = 4;
const MAX_WORD_CHARS: usize = 32;
/// Folder and project lists are read again after this long.
const REFRESH: Duration = Duration::from_secs(600);

/// One dictation's words, by source, spelled as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWords {
    #[serde(default)]
    pub screen: Vec<String>,
    #[serde(default)]
    pub project: Vec<String>,
    #[serde(default)]
    pub everywhere: Vec<String>,
    /// The folder the project words came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<PathBuf>,
}

impl SessionWords {
    pub fn is_empty(&self) -> bool {
        self.screen.is_empty() && self.project.is_empty() && self.everywhere.is_empty()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} on screen, {} in project{}, {} everywhere",
            self.screen.len(),
            self.project.len(),
            self.project_dir
                .as_ref()
                .and_then(|dir| dir.file_name())
                .map(|name| format!(" {}", name.to_string_lossy()))
                .unwrap_or_default(),
            self.everywhere.len()
        )
    }

    fn all(&self) -> impl Iterator<Item = &String> {
        self.screen.iter().chain(&self.project).chain(&self.everywhere)
    }
}

// ----- English ---------------------------------------------------------------

/// English words: a sorted, lowercase list in one buffer (about 3.5 MB for
/// the system list) with plural and verb endings tried on lookup, and on
/// macOS the system's modern English vocabulary for what that list misses
/// ("timeout", "clipboard").
pub struct EnglishWords {
    blob: String,
    starts: Vec<u32>,
    /// MODERN_WORDS, sorted: everyday words even when the system is asked.
    modern: Vec<String>,
    #[cfg(target_os = "macos")]
    system: Option<sunoto_desktop::EnglishVocabulary>,
    /// The system vocabulary's answers so far; it is slower to ask.
    #[cfg(target_os = "macos")]
    asked: std::sync::Mutex<HashMap<String, bool>>,
}

impl EnglishWords {
    pub fn load() -> Self {
        let text = std::fs::read_to_string(DICTIONARY).unwrap_or_default();
        let modern = MODERN_WORDS
            .lines()
            .filter(|line| !line.starts_with('#'))
            .flat_map(str::split_whitespace);
        let mut words = Self::from_words(text.lines().chain(modern.clone()));
        words.modern = modern.map(str::to_lowercase).collect();
        words.modern.sort_unstable();
        words.modern.dedup();
        #[cfg(target_os = "macos")]
        {
            words.system = sunoto_desktop::EnglishVocabulary::load();
        }
        words
    }

    /// Whether the system's modern vocabulary is there to ask.
    pub fn has_system_vocabulary(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.system.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    #[cfg(target_os = "macos")]
    fn system_knows(&self, word: &str) -> bool {
        let Some(system) = self.system.as_ref() else {
            return false;
        };
        let mut asked = self.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(&known) = asked.get(word) {
            return known;
        }
        if asked.len() > 20_000 {
            asked.clear();
        }
        let known = system.contains(word);
        asked.insert(word.to_string(), known);
        known
    }

    #[cfg(not(target_os = "macos"))]
    fn system_knows(&self, _word: &str) -> bool {
        false
    }

    pub fn from_words<'a>(words: impl Iterator<Item = &'a str>) -> Self {
        let mut list: Vec<String> = words
            .map(|word| word.trim().to_lowercase())
            .filter(|word| !word.is_empty() && word.chars().all(char::is_alphabetic))
            .collect();
        list.sort_unstable();
        list.dedup();
        let mut blob = String::with_capacity(list.iter().map(String::len).sum());
        let mut starts = Vec::with_capacity(list.len() + 1);
        for word in &list {
            starts.push(blob.len() as u32);
            blob.push_str(word);
        }
        starts.push(blob.len() as u32);
        Self {
            blob,
            starts,
            modern: Vec::new(),
            #[cfg(target_os = "macos")]
            system: None,
            #[cfg(target_os = "macos")]
            asked: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    fn has(&self, word: &str) -> bool {
        let count = self.len();
        let (mut lo, mut hi) = (0, count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = &self.blob[self.starts[mid] as usize..self.starts[mid + 1] as usize];
            match entry.cmp(word) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    /// Whether `word` (any case) is English, counting plural and verb forms.
    pub fn knows(&self, word: &str) -> bool {
        let word = word.to_lowercase();
        if word.chars().count() < 2 || self.listed(&word) {
            return true;
        }
        self.system_knows(&word)
    }

    /// Whether `word` is an everyday word: the system's modern vocabulary
    /// and MODERN_WORDS where the system has one, else `knows`. The old list
    /// also holds rare words ("lytic"), which must not stop "Dun Lytics" from
    /// being fixed.
    pub fn common(&self, word: &str) -> bool {
        if !self.has_system_vocabulary() {
            return self.knows(word);
        }
        let word = word.to_lowercase();
        word.chars().count() < 2 || self.modern(&word) || self.system_knows(&word)
    }

    fn modern(&self, word: &str) -> bool {
        self.modern.binary_search_by(|entry| entry.as_str().cmp(word)).is_ok()
    }

    /// `knows` from the word list alone.
    fn listed(&self, word: &str) -> bool {
        if self.has(word) {
            return true;
        }
        const ENDINGS: &[(&str, &str)] = &[
            ("ies", "y"),
            ("ied", "y"),
            ("es", ""),
            ("s", ""),
            ("ed", ""),
            ("ed", "e"),
            ("d", ""),
            ("ing", ""),
            ("ing", "e"),
            ("ly", ""),
            ("er", ""),
            ("er", "e"),
            ("ers", ""),
            ("est", ""),
        ];
        for (ending, replacement) in ENDINGS {
            let Some(stem) = word.strip_suffix(ending) else {
                continue;
            };
            if stem.len() < 2 {
                continue;
            }
            if self.has(&format!("{stem}{replacement}")) {
                return true;
            }
            // "running" -> "run", "stopped" -> "stop"
            let mut chars = stem.chars().rev();
            if replacement.is_empty()
                && let (Some(last), Some(before)) = (chars.next(), chars.next())
                && last == before
                && self.has(&stem[..stem.len() - last.len_utf8()])
            {
                return true;
            }
        }
        false
    }
}

// ----- unusual words ---------------------------------------------------------

/// The parts people say in a written word: `huskyAI` -> husky, AI;
/// `tinySDS3d` -> tiny, SDS, 3, d. Mirrors `word_parts` in word_boost.py.
pub fn word_parts(word: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        let mut j = i + 1;
        if c.is_ascii_digit() {
            while j < chars.len() && chars[j].1.is_ascii_digit() {
                j += 1;
            }
        } else if c.is_uppercase() {
            while j < chars.len() && chars[j].1.is_uppercase() {
                j += 1;
            }
            if j < chars.len() && chars[j].1.is_lowercase() {
                if j - i > 1 {
                    j -= 1; // "CGEvent": the E starts the next part
                } else {
                    while j < chars.len() && chars[j].1.is_lowercase() {
                        j += 1;
                    }
                }
            }
        } else if c.is_alphabetic() {
            while j < chars.len() && chars[j].1.is_lowercase() {
                j += 1;
            }
        } else {
            i += 1;
            continue;
        }
        let end = chars.get(j).map_or(word.len(), |&(index, _)| index);
        parts.push(&word[start..end]);
        i = j;
    }
    parts
}

fn is_acronym(part: &str) -> bool {
    let count = part.chars().count();
    (2..=6).contains(&count) && part.chars().all(|c| c.is_ascii_uppercase())
}

/// Whether the model would likely spell `word` wrong: the English list does
/// not know it, and it is not just ordinary words run together
/// ("TestFlight").
pub fn is_unusual(word: &str, min_letters: usize, english: &EnglishWords) -> bool {
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    if letters < min_letters || word.chars().count() > MAX_WORD_CHARS {
        return false;
    }
    if !word.chars().all(|c| c.is_alphanumeric()) {
        return false;
    }
    let digits = word.chars().filter(char::is_ascii_digit).count();
    if digits > 0 && word.chars().all(|c| c.is_ascii_hexdigit()) {
        return false; // a hash or an id, not a word anyone says
    }
    if digits > 3 {
        return false;
    }
    if english.knows(word) {
        return false;
    }
    word_parts(word).iter().any(|part| {
        part.chars().count() >= 2
            && part.chars().any(char::is_alphabetic)
            && (is_acronym(part) || !english.knows(part))
    })
}

/// The unusual words in `text`, most frequent first, at most `max`.
pub fn unusual_words(
    texts: &[&str],
    min_letters: usize,
    max: usize,
    english: &EnglishWords,
) -> Vec<String> {
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut order = 0;
    for text in texts {
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            if word.is_empty() {
                continue;
            }
            let entry = counts.entry(word).or_insert_with(|| {
                order += 1;
                (0, order)
            });
            entry.0 += 1;
        }
    }
    let mut words: Vec<(&str, usize, usize)> = counts
        .into_iter()
        .filter(|(word, _)| is_unusual(word, min_letters, english))
        .map(|(word, (count, first))| (word, count, first))
        .collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    let mut seen = std::collections::HashSet::new();
    words
        .into_iter()
        .filter(|(word, ..)| seen.insert(word.to_lowercase()))
        .take(max)
        .map(|(word, ..)| word.to_string())
        .collect()
}

// ----- near misses -----------------------------------------------------------

/// Edits allowed between a heard word and a listed word of `letters` letters.
fn allowed_edits(letters: usize) -> usize {
    match letters {
        0..=4 => 0,
        5..=7 => 1,
        _ => 2,
    }
}

/// Optimal string alignment distance, giving up past `limit`.
fn distance(a: &[char], b: &[char], limit: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > limit {
        return None;
    }
    let mut before: Vec<usize> = Vec::new();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        let mut best = row[0];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (previous[j] + 1).min(row[j - 1] + 1).min(previous[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
            best = best.min(row[j]);
        }
        if best > limit {
            return None;
        }
        before = std::mem::replace(&mut previous, row);
    }
    let found = previous[b.len()];
    (found <= limit).then_some(found)
}

fn letters_of(word: &str) -> Vec<char> {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether a word starting at `index` begins a sentence.
fn starts_sentence(text: &str, index: usize) -> bool {
    let before = text[..index].trim_end();
    before.is_empty() || before.ends_with(['.', '?', '!', ':', '\n'])
}

/// `written` as it belongs in the sentence: a lowercase name gets a capital
/// only where the heard word starts a sentence with one.
fn written_like(written: &str, heard: &str, sentence_start: bool) -> String {
    if sentence_start
        && written.chars().all(|c| !c.is_uppercase())
        && heard.starts_with(char::is_uppercase)
    {
        let mut chars = written.chars();
        return chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
    }
    written.to_string()
}

/// Swap words the model got one letter off ("Ghosty", "Tsunoto"), or two
/// for long words ("Dun Lytics" for "daanlytics"), for the listed word. Only
/// words the English list does not know are touched, so "code" never turns
/// into "Xcode".
pub fn fix_near_misses(text: &str, words: &SessionWords, english: &EnglishWords) -> String {
    // One entry per spelling; the strongest source's casing wins ("sunoto"
    // in the project over "Sunoto" the app), so the two never tie.
    let mut seen = std::collections::HashSet::new();
    let targets: Vec<(Vec<char>, &str)> = words
        .all()
        .map(|word| (letters_of(word), word.as_str()))
        .filter(|(letters, _)| allowed_edits(letters.len()) > 0 && seen.insert(letters.clone()))
        .collect();
    if targets.is_empty() {
        return text.to_string();
    }
    // (start, end) byte ranges of the words in `text`.
    let mut spans = Vec::new();
    let mut start = None;
    for (index, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                spans.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    // The closest listed word and its distance; 0 means the letters match.
    let best_target = |heard: &[char]| -> Option<(&str, usize)> {
        let mut found: Option<(usize, &str)> = None;
        let mut tie = false;
        for (letters, written) in &targets {
            if letters.as_slice() == heard {
                return Some((written, 0));
            }
            if let Some(edits) = distance(heard, letters, allowed_edits(letters.len())) {
                match found {
                    Some((best, _)) if edits > best => {}
                    Some((best, _)) if edits == best => tie = true,
                    _ => {
                        found = Some((edits, written));
                        tie = false;
                    }
                }
            }
        }
        found.filter(|_| !tie).map(|(edits, written)| (written, edits))
    };
    let unknown = |span: (usize, usize)| {
        let word = &text[span.0..span.1];
        !word.chars().any(|c| c.is_ascii_digit()) && !english.common(word)
    };
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < spans.len() {
        let first = spans[i];
        // Two heard words for one listed word: "Dun Lytics", "mini sign".
        // Spelled exactly it is the word; merely close, one of the two must
        // be an uncommon word, so "work action" never becomes "workation".
        if let Some(&second) = spans.get(i + 1) {
            let gap = &text[first.1..second.0];
            if gap == " " || gap == "-" {
                let heard: Vec<char> = letters_of(&text[first.0..second.1]);
                if heard.len() >= 6
                    && let Some((written, edits)) = best_target(&heard)
                    && (edits == 0 || unknown(first) || unknown(second))
                {
                    out.push_str(&text[copied..first.0]);
                    let start = starts_sentence(text, first.0);
                    out.push_str(&written_like(written, &text[first.0..first.1], start));
                    copied = second.1;
                    i += 2;
                    continue;
                }
            }
        }
        if unknown(first) {
            let heard = letters_of(&text[first.0..first.1]);
            // Right letters already: the sidecar wrote it as listed.
            if heard.len() >= 4
                && let Some((written, edits)) = best_target(&heard)
                && edits > 0
            {
                out.push_str(&text[copied..first.0]);
                let start = starts_sentence(text, first.0);
                out.push_str(&written_like(written, &text[first.0..first.1], start));
                copied = first.1;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

// ----- sources ---------------------------------------------------------------

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn expand(path: &str) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => home().map(|home| home.join(rest)),
        None => Some(PathBuf::from(path)),
    }
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| !is_hidden(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect()
}

/// The user's projects (folder name -> path) under the project roots: each
/// subfolder, and the repos inside a subfolder that only groups repos.
pub fn find_projects(home: &Path) -> HashMap<String, PathBuf> {
    let mut projects = HashMap::new();
    for root in PROJECT_ROOTS {
        for dir in child_dirs(&home.join(root)) {
            let grouped: Vec<PathBuf> = if dir.join(".git").exists() {
                Vec::new()
            } else {
                child_dirs(&dir)
                    .into_iter()
                    .filter(|child| child.join(".git").exists())
                    .collect()
            };
            for path in std::iter::once(dir).chain(grouped) {
                if let Some(name) = path.file_name() {
                    projects
                        .entry(name.to_string_lossy().into_owned())
                        .or_insert(path);
                }
                if projects.len() >= 1_000 {
                    return projects;
                }
            }
        }
    }
    projects
}

fn app_names() -> Vec<String> {
    APP_FOLDERS
        .iter()
        .filter_map(|folder| expand(folder))
        .filter_map(|folder| std::fs::read_dir(folder).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".app").map(str::to_string)
        })
        .collect()
}

/// Names in a project: folders and files three deep, skipping build output
/// and dependencies.
pub fn project_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(name) = root.file_name() {
        names.push(name.to_string_lossy().into_owned());
    }
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if names.len() >= PROJECT_WALK_MAX {
                return names;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_hidden(&name) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with("venv") {
                    continue;
                }
                names.push(name);
                if depth < 2 {
                    queue.push_back((entry.path(), depth + 1));
                }
            } else {
                let stem = Path::new(&name)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or(name);
                names.push(stem);
            }
        }
    }
    names
}

/// The nearest folder at or above `path` holding a `.git`, or `path` itself.
fn project_root(path: &Path, home: &Path) -> Option<PathBuf> {
    let dir = if path.is_dir() { path } else { path.parent()? };
    let mut current = Some(dir);
    for _ in 0..8 {
        let Some(candidate) = current else { break };
        if candidate == home {
            break;
        }
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    (dir != home && dir.starts_with(home)).then(|| dir.to_path_buf())
}

fn decode_percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the focused window says about where the user is working.
#[derive(Debug, Clone, Default)]
pub struct Focus {
    pub window_title: Option<String>,
    pub document: Option<String>,
    pub text: Option<String>,
}

/// The project the focused window is in: the folder of its document (a
/// terminal's working folder, an open file), a path in its title, or a known
/// project name in its title (an editor's "file — project").
pub fn detect_project(
    focus: &Focus,
    projects: &HashMap<String, PathBuf>,
    home: &Path,
) -> Option<PathBuf> {
    if let Some(path) = focus
        .document
        .as_deref()
        .and_then(|document| document.strip_prefix("file://"))
        .map(decode_percent)
        .map(PathBuf::from)
        .filter(|path| path.exists())
    {
        return project_root(&path, home);
    }
    let title = focus.window_title.as_deref()?;
    for word in title.split_whitespace() {
        let path = match word.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None if word.starts_with('/') => PathBuf::from(word),
            None => continue,
        };
        if path.exists() {
            return project_root(&path, home);
        }
    }
    let title_words: Vec<String> = title
        .split(|c: char| c.is_whitespace() || "—–|:·()[]".contains(c))
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    let mut known: Vec<(&String, &PathBuf)> = projects
        .iter()
        .filter(|(name, _)| name.chars().count() >= 4)
        .collect();
    known.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    known
        .into_iter()
        .find(|(name, _)| title_words.contains(&name.to_lowercase()))
        .map(|(_, path)| path.clone())
}

/// The focused window now, or with `pid` that app's focused window.
#[cfg(target_os = "macos")]
fn read_focus(pid: Option<i32>) -> Option<Focus> {
    let snapshot = match pid {
        Some(pid) => sunoto_desktop::app_snapshot(pid, SCREEN_CHARS),
        None => sunoto_desktop::focus_snapshot(SCREEN_CHARS),
    }?;
    Some(Focus {
        window_title: snapshot.window_title,
        document: snapshot.document,
        text: snapshot.text,
    })
}

#[cfg(not(target_os = "macos"))]
fn read_focus(_pid: Option<i32>) -> Option<Focus> {
    None
}

/// Builds word lists and keeps the slow parts (folders, projects) cached.
pub struct Collector {
    pub english: Arc<EnglishWords>,
    extra: Vec<String>,
    home: PathBuf,
    everywhere: Option<(Instant, Vec<String>, HashMap<String, PathBuf>)>,
    project_cache: HashMap<PathBuf, (Instant, Vec<String>)>,
}

impl Collector {
    pub fn new(extra: Vec<String>) -> Self {
        Self {
            english: Arc::new(EnglishWords::load()),
            extra,
            home: home().unwrap_or_default(),
            everywhere: None,
            project_cache: HashMap::new(),
        }
    }

    /// Read the folder and app names again when they are older than REFRESH.
    fn refresh_everywhere(&mut self) {
        let stale = self
            .everywhere
            .as_ref()
            .is_none_or(|(at, ..)| at.elapsed() > REFRESH);
        if stale {
            let projects = find_projects(&self.home);
            let mut names: Vec<String> = projects.keys().cloned().collect();
            names.sort();
            names.extend(app_names());
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let words = unusual_words(&refs, NAME_MIN_LETTERS, EVERYWHERE_MAX, &self.english);
            self.everywhere = Some((Instant::now(), words, projects));
        }
    }

    fn project(&mut self, root: &Path) -> Vec<String> {
        if let Some((at, words)) = self.project_cache.get(root)
            && at.elapsed() <= REFRESH
        {
            return words.clone();
        }
        let names = project_names(root);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let words = unusual_words(&refs, SCREEN_MIN_LETTERS, PROJECT_MAX, &self.english);
        if self.project_cache.len() > 32 {
            self.project_cache.clear();
        }
        self.project_cache
            .insert(root.to_path_buf(), (Instant::now(), words.clone()));
        words
    }

    /// The word list for a dictation starting now in the focused window.
    pub fn collect(&mut self, focus: Option<Focus>) -> SessionWords {
        let focus = focus.unwrap_or_default();
        self.refresh_everywhere();
        let (everywhere, project_dir) = match &self.everywhere {
            Some((_, words, projects)) => {
                (words.clone(), detect_project(&focus, projects, &self.home))
            }
            None => (Vec::new(), None),
        };
        let project = project_dir
            .as_deref()
            .map(|dir| self.project(dir))
            .unwrap_or_default();
        let document_path = focus
            .document
            .as_deref()
            .map(|document| decode_percent(document.strip_prefix("file://").unwrap_or(document)));
        let texts: Vec<&str> = [
            focus.text.as_deref(),
            focus.window_title.as_deref(),
            document_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut screen: Vec<String> = self.extra.clone();
        screen.extend(unusual_words(&texts, SCREEN_MIN_LETTERS, SCREEN_MAX, &self.english));
        let mut words = SessionWords {
            screen,
            project,
            everywhere,
            project_dir,
        };
        merge_spellings(&mut words);
        words
    }

    pub fn collect_now(&mut self) -> SessionWords {
        let focus = read_focus(None);
        self.collect(focus)
    }
}

/// Keep each word once, in its strongest list, written the most deliberate
/// way any list has it: mixed case ("Sunoto", the app) over lowercase
/// ("sunoto", a folder) over all caps (a heading).
pub fn merge_spellings(words: &mut SessionWords) {
    fn rank(word: &str) -> u8 {
        let upper = word.chars().any(char::is_uppercase);
        let lower = word.chars().any(char::is_lowercase);
        match (upper, lower) {
            (true, true) => 2,
            (false, _) => 1,
            (true, false) => 0,
        }
    }
    let key = |word: &str| letters_of(word).into_iter().collect::<String>();
    let mut best: HashMap<String, String> = HashMap::new();
    for word in words.all() {
        let entry = best.entry(key(word)).or_insert_with(|| word.clone());
        if rank(word) > rank(entry) {
            *entry = word.clone();
        }
    }
    let mut seen = std::collections::HashSet::new();
    for list in [&mut words.screen, &mut words.project, &mut words.everywhere] {
        list.retain_mut(|word| {
            let key = key(word);
            if !seen.insert(key.clone()) {
                return false;
            }
            *word = best[&key].clone();
            true
        });
    }
}

// ----- worker ----------------------------------------------------------------

enum Job {
    Collect { session_id: u64 },
}

/// The thread that reads the screen and folders, off the event loop: a
/// slow app can take up to a few tenths of a second to answer.
pub struct WordWorker {
    jobs: Sender<Job>,
}

impl WordWorker {
    pub fn spawn(extra: Vec<String>, events: Sender<DaemonEvent>) -> Self {
        let (jobs, inbox): (Sender<Job>, Receiver<Job>) = mpsc::channel();
        std::thread::Builder::new()
            .name("word-list".into())
            .spawn(move || {
                let mut collector = Collector::new(extra);
                // Warm the folder list so the first dictation does not wait.
                collector.refresh_everywhere();
                while let Ok(Job::Collect { session_id }) = inbox.recv() {
                    let started = Instant::now();
                    let words = collector.collect_now();
                    let event = DaemonEvent::Words {
                        session_id,
                        words,
                        english: Arc::clone(&collector.english),
                        took: started.elapsed(),
                    };
                    if events.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn word-list thread");
        Self { jobs }
    }

    pub fn collect(&self, session_id: u64) {
        let _ = self.jobs.send(Job::Collect { session_id });
    }
}

// ----- CLI -------------------------------------------------------------------

#[derive(Deserialize)]
struct FixItem {
    text: String,
    words: SessionWords,
}

/// `sunoto-daemon words`: print the word list for the focused window now
/// (`--delay S` waits first, `--pid N` reads that app instead, `--text FILE`
/// stands in for the focused text, `--json` for machine output), or with `--fix
/// FILE` apply `fix_near_misses` to `[{"text", "words"}]` and print the
/// fixed texts as JSON (for tools/vocab/bench_words.py).
pub fn run_cli(args: &[String]) -> Result<(), String> {
    let value_of = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
    };
    if let Some(path) = value_of("--fix") {
        let items: Vec<FixItem> = serde_json::from_str(
            &std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?,
        )
        .map_err(|error| format!("{path}: {error}"))?;
        let english = EnglishWords::load();
        let started = Instant::now();
        let fixed: Vec<String> = items
            .iter()
            .map(|item| fix_near_misses(&item.text, &item.words, &english))
            .collect();
        let took = started.elapsed();
        eprintln!(
            "fixed {} texts in {:.1} ms ({:.3} ms each)",
            items.len(),
            took.as_secs_f64() * 1e3,
            took.as_secs_f64() * 1e3 / items.len().max(1) as f64
        );
        println!("{}", serde_json::to_string(&fixed).map_err(|error| error.to_string())?);
        return Ok(());
    }
    if let Some(delay) = value_of("--delay") {
        let seconds: f64 = delay.parse().map_err(|_| format!("bad --delay {delay}"))?;
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
    let started = Instant::now();
    let mut collector = Collector::new(Vec::new());
    let loaded = started.elapsed();
    collector.refresh_everywhere();
    let cold = started.elapsed() - loaded;
    let pid = match value_of("--pid") {
        Some(pid) => Some(pid.parse::<i32>().map_err(|_| format!("bad --pid {pid}"))?),
        None => None,
    };
    let read_started = Instant::now();
    let mut focus = read_focus(pid);
    // `--text FILE` stands in for the focused text (benchmarks, tests).
    if let Some(path) = value_of("--text") {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
        focus.get_or_insert_with(Focus::default).text = Some(text);
    }
    let read = read_started.elapsed();
    let words = collector.collect(focus.clone());
    let total = read_started.elapsed();
    if args.iter().any(|arg| arg == "--json") {
        println!("{}", serde_json::to_string_pretty(&words).map_err(|error| error.to_string())?);
        return Ok(());
    }
    println!(
        "English list: {} words{}, loaded in {} ms",
        collector.english.len(),
        if collector.english.has_system_vocabulary() {
            " and the system vocabulary"
        } else {
            ""
        },
        loaded.as_millis()
    );
    println!("Folders and apps: read in {} ms (then cached for 10 minutes)", cold.as_millis());
    match &focus {
        Some(focus) => println!(
            "Focused window: title {:?}, document {:?}, {} characters of text; read in {} ms",
            focus.window_title,
            focus.document,
            focus.text.as_deref().map_or(0, |text| text.chars().count()),
            read.as_millis()
        ),
        None => println!("Focused window: nothing readable (Accessibility permission?)"),
    }
    println!("Word list built in {} ms: {}", total.as_millis(), words.summary());
    for (name, list) in [
        ("screen", &words.screen),
        ("project", &words.project),
        ("everywhere", &words.everywhere),
    ] {
        println!("\n{name} ({}):\n  {}", list.len(), list.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> EnglishWords {
        EnglishWords::from_words(
            [
                "code", "the", "push", "report", "main", "branch", "test", "flight", "husky",
                "tiny", "crash", "note", "load", "run", "stop", "who", "else", "is", "free",
                "dun", "live", "kit", "analytic", "weave", "a", "i", "ai",
            ]
            .into_iter(),
        )
    }

    #[test]
    fn english_knows_plurals_and_verb_forms() {
        let words = english();
        for word in ["code", "Codes", "crashes", "notes", "loaded", "running", "stopped", "analytics"] {
            assert!(words.knows(word), "{word}");
        }
        for word in ["sunoto", "daanlytics", "ghostty"] {
            assert!(!words.knows(word), "{word}");
        }
    }

    #[test]
    fn word_parts_split_like_people_say_them() {
        assert_eq!(word_parts("huskyAI"), ["husky", "AI"]);
        assert_eq!(word_parts("tinySDS3d"), ["tiny", "SDS", "3", "d"]);
        assert_eq!(word_parts("CGEventTap"), ["CG", "Event", "Tap"]);
        assert_eq!(word_parts("Xcode"), ["Xcode"]);
        assert_eq!(word_parts("vaani"), ["vaani"]);
    }

    #[test]
    fn only_words_the_model_would_misspell_are_unusual() {
        let words = english();
        for word in ["daanlytics", "huskyAI", "tinySDS3d", "Ghostty", "Sunoto", "XTEST"] {
            assert!(is_unusual(word, 5, &words), "{word}");
        }
        // English, ordinary words run together, too short, ids and hashes.
        for word in ["code", "Crashes", "TestFlight", "free", "f3d", "dc6cf6d5", "12345abc"] {
            assert!(!is_unusual(word, 5, &words), "{word}");
        }
    }

    #[test]
    fn unusual_words_ranks_by_frequency_and_dedups_case() {
        let words = english();
        let found = unusual_words(
            &["sunoto huskyAI the code", "Sunoto daemon: sunoto again", "who-else-is-free"],
            5,
            10,
            &words,
        );
        assert_eq!(found[0], "sunoto");
        assert!(found.contains(&"huskyAI".to_string()));
        assert!(!found.iter().any(|word| word == "Sunoto"));
        assert!(!found.iter().any(|word| word == "free" || word == "who"));
    }

    fn list(everywhere: &[&str]) -> SessionWords {
        SessionWords {
            everywhere: everywhere.iter().map(|word| word.to_string()).collect(),
            ..SessionWords::default()
        }
    }

    #[test]
    fn near_misses_become_the_listed_word() {
        let words = english();
        let listed = list(&["Ghostty", "Sunoto", "daanlytics", "Xcode"]);
        assert_eq!(
            fix_near_misses("Ghosty crashed. Restart the Tsunoto app.", &listed, &words),
            "Ghostty crashed. Restart the Sunoto app."
        );
        assert_eq!(
            fix_near_misses("Push the Dun Lytics report.", &listed, &words),
            "Push the daanlytics report."
        );
    }

    #[test]
    fn two_words_spelling_a_listed_word_join() {
        let words = english();
        let listed = list(&["minisign", "workation"]);
        assert_eq!(
            fix_near_misses("Sign it with mini sign first.", &listed, &words),
            "Sign it with minisign first."
        );
        // Close is not enough when both words are ordinary.
        let text = "Take the work action now.";
        let words = EnglishWords::from_words(["take", "the", "work", "action", "now"].into_iter());
        assert_eq!(fix_near_misses(text, &listed, &words), text);
    }

    #[test]
    fn near_misses_never_touch_english_words_or_short_names() {
        let words = english();
        let listed = list(&["Xcode", "weif", "analytix"]);
        let text = "The code is in the weave branch, see analytics.";
        assert_eq!(fix_near_misses(text, &listed, &words), text);
    }

    #[test]
    fn one_word_in_two_lists_is_not_a_tie() {
        let words = english();
        let listed = SessionWords {
            project: vec!["sunoto".into()],
            everywhere: vec!["Sunoto".into()],
            ..SessionWords::default()
        };
        assert_eq!(fix_near_misses("Restart the Tsunoto app.", &listed, &words), "Restart the sunoto app.");
    }

    #[test]
    fn merged_spellings_keep_the_strongest_list_and_the_deliberate_case() {
        let mut words = SessionWords {
            screen: vec!["SUNOTO".into(), "Ghostty".into()],
            project: vec!["sunoto".into(), "nemotron".into()],
            everywhere: vec!["Sunoto".into(), "ghostty".into(), "uniblox".into()],
            project_dir: None,
        };
        merge_spellings(&mut words);
        assert_eq!(words.screen, ["Sunoto", "Ghostty"]);
        assert_eq!(words.project, ["nemotron"]);
        assert_eq!(words.everywhere, ["uniblox"]);
    }

    #[test]
    fn near_miss_ties_are_left_alone() {
        let words = english();
        let listed = list(&["sunota", "sunoti"]);
        assert_eq!(fix_near_misses("sunoto", &listed, &words), "sunoto");
    }

    #[test]
    fn lowercase_names_keep_a_capital_at_sentence_start() {
        let words = english();
        let listed = list(&["uniblox"]);
        assert_eq!(fix_near_misses("Uniblux is down.", &listed, &words), "Uniblox is down.");
    }

    #[test]
    fn project_comes_from_document_title_path_or_known_name() {
        let base = std::env::temp_dir().join(format!("sunoto-words-{}", std::process::id()));
        let repo = base.join("workspace/huskyAI");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        let projects = HashMap::from([("huskyAI".to_string(), repo.clone())]);
        let by_document = Focus {
            document: Some(format!("file://{}/src/deep/", repo.display())),
            ..Focus::default()
        };
        assert_eq!(detect_project(&by_document, &projects, &base), Some(repo.clone()));
        let by_title_path = Focus {
            window_title: Some("zsh ~/workspace/huskyAI/src".into()),
            ..Focus::default()
        };
        assert_eq!(detect_project(&by_title_path, &projects, &base), Some(repo.clone()));
        let by_name = Focus {
            window_title: Some("main.rs — huskyAI".into()),
            ..Focus::default()
        };
        assert_eq!(detect_project(&by_name, &projects, &base), Some(repo.clone()));
        let elsewhere = Focus {
            window_title: Some("Untitled".into()),
            ..Focus::default()
        };
        assert_eq!(detect_project(&elsewhere, &projects, &base), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn project_names_skip_build_output_and_hidden_folders() {
        let base = std::env::temp_dir().join(format!("sunoto-walk-{}", std::process::id()));
        for dir in ["crates/sunoto-core", "node_modules/leftpad", ".git/objects", "target/debug"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("crates/nemotron.py"), "").unwrap();
        let names = project_names(&base);
        assert!(names.contains(&"sunoto-core".to_string()));
        assert!(names.contains(&"nemotron".to_string()));
        assert!(!names.iter().any(|name| name == "leftpad" || name == "objects" || name == "debug"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn collect_puts_screen_words_first_and_extra_words_on_screen() {
        let mut collector = Collector {
            english: Arc::new(english()),
            extra: vec!["Kubernetes".into()],
            home: std::env::temp_dir().join("sunoto-no-home"),
            everywhere: None,
            project_cache: HashMap::new(),
        };
        let words = collector.collect(Some(Focus {
            window_title: Some("sunoto — notes".into()),
            text: Some("Restart the Ghostty window".into()),
            ..Focus::default()
        }));
        assert_eq!(words.screen[0], "Kubernetes");
        assert!(words.screen.contains(&"Ghostty".to_string()));
        // "Sunoto" when this Mac has the app installed (merged spellings).
        assert!(words.screen.iter().any(|word| word.eq_ignore_ascii_case("sunoto")));
        assert!(words.project.is_empty());
    }
}

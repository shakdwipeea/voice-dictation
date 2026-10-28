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
/// Folders macOS guards with a permission dialog ("Sunoto would like to
/// access files in your Documents folder"). Nothing under them is touched,
/// not even to see whether a path exists, unless the user turned on the
/// Documents source (`word_list_documents`), which accepts that dialog.
const PROTECTED: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    "Library/Mobile Documents",
];

/// Whether reading `path` could raise one of macOS's folder dialogs.
pub fn is_protected(path: &Path, home: &Path) -> bool {
    path.starts_with("/Volumes")
        || PROTECTED
            .iter()
            .any(|folder| path.starts_with(home.join(folder)))
}

/// Folders under the home folder whose subfolders are the user's projects.
const PROJECT_ROOTS: &[&str] = &[
    "workspace",
    "Developer",
    "Projects",
    "projects",
    "code",
    "Code",
    "src",
    "dev",
    "repos",
    "git",
    "GitHub",
    "Documents/GitHub",
    "Documents/Projects",
];
const APP_FOLDERS: &[&str] = &["/Applications", "Applications"];
/// Folders under the home folder whose subfolders name what people work on
/// outside code: clients, cases, courses, books.
const DOCUMENT_ROOTS: &[&str] = &["Documents", "Desktop"];
/// Folders a project walk never enters: build output, dependencies, caches.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "vendor",
    "venv",
    "env",
    "__pycache__",
    "Pods",
    "DerivedData",
    "coverage",
    "tmp",
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
/// App and people's names are deliberate, so four letters are enough.
const PROPER_MIN_LETTERS: usize = 4;
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
    /// How much of the screen was read for this list.
    #[serde(skip)]
    pub read: ScreenRead,
}

/// How much of the focused window one word list read, in counts only (for
/// the accuracy record; never the words).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenRead {
    /// The whole window was walked because the text box was short.
    pub window: bool,
    /// Elements visited in that walk.
    pub elements: u32,
    /// Characters of screen text the list was drawn from.
    pub chars: u32,
}

impl SessionWords {
    pub fn is_empty(&self) -> bool {
        self.screen.is_empty() && self.project.is_empty() && self.everywhere.is_empty()
    }

    /// Counts only: the project folder's name is the user's, so it stays
    /// out of the log.
    pub fn summary(&self) -> String {
        format!(
            "{} on screen, {} in project, {} everywhere",
            self.screen.len(),
            self.project.len(),
            self.everywhere.len()
        )
    }

    fn all(&self) -> impl Iterator<Item = &String> {
        self.screen
            .iter()
            .chain(&self.project)
            .chain(&self.everywhere)
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
    /// Treat rare English words ("hidradenitis", "alembic") as unusual too:
    /// only everyday words (`common`) are left off word lists.
    pub keep_rare: bool,
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
        let mut asked = self
            .asked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            keep_rare: false,
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
        self.modern
            .binary_search_by(|entry| entry.as_str().cmp(word))
            .is_ok()
    }

    /// Whether a word list leaves `word` off: `common` with `keep_rare`,
    /// else `knows`.
    pub fn ordinary(&self, word: &str) -> bool {
        if self.keep_rare {
            self.common(word)
        } else {
            self.knows(word)
        }
    }

    /// Whether the old list has exactly this word, no endings tried (it has
    /// "William" and "Rachel", but "Lytics" only as a plural of "lytic").
    pub fn listed_exactly(&self, word: &str) -> bool {
        self.has(&word.to_lowercase())
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
    if english.ordinary(word) {
        return false;
    }
    word_parts(word).iter().any(|part| {
        part.chars().count() >= 2
            && part.chars().any(char::is_alphabetic)
            && (is_acronym(part) || !english.ordinary(part))
    })
}

/// The unusual words in `text`, most frequent first, at most `max`.
pub fn unusual_words(
    texts: &[&str],
    min_letters: usize,
    max: usize,
    english: &EnglishWords,
) -> Vec<String> {
    ranked_words(texts, max, |word| is_unusual(word, min_letters, english))
}

/// A brand written as ordinary words run together ("TablePlus",
/// "iManage"): the model writes "Table Plus", so the list keeps app names
/// like this, though `is_unusual` passes them by.
pub fn is_brand(word: &str, english: &EnglishWords) -> bool {
    let parts = word_parts(word);
    parts.len() >= 2
        && word.chars().skip(1).any(char::is_uppercase)
        && word.chars().any(char::is_lowercase)
        && !english.ordinary(word)
}

/// Names of apps and people: unusual words from four letters ("Anki",
/// "Iyer"), and brands.
pub fn name_words(texts: &[&str], max: usize, english: &EnglishWords) -> Vec<String> {
    ranked_words(texts, max, |word| {
        is_unusual(word, PROPER_MIN_LETTERS, english) || is_brand(word, english)
    })
}

/// The words of `texts` that `keep` accepts, most frequent first, once
/// each whatever their case, at most `max`.
fn ranked_words(texts: &[&str], max: usize, keep: impl Fn(&str) -> bool) -> Vec<String> {
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
        .filter(|(word, _)| keep(word))
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
            row[j] = (previous[j] + 1)
                .min(row[j - 1] + 1)
                .min(previous[j - 1] + cost);
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

/// How far `fix_near_misses` reaches; the defaults are what the daemon uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct FixOptions {
    /// Most heard words joined into one listed word ("C G event tap").
    /// Three or more must spell it exactly.
    pub max_join: usize,
    /// Also match an uncommon heard word that sounds like a listed word
    /// ("Vanny" for "vaani"): same `sound_key`, at most three letters apart.
    pub phonetic: bool,
}

impl Default for FixOptions {
    fn default() -> Self {
        // Exact joins of three or four words also keep a close pair from
        // swallowing the end of one ("C G event tap" -> "C G CGEventTap").
        // Sound matching added 25 of 536 persona names and changed no common
        // word on LibriSpeech test-clean (tools/vocab, 2026-10-18).
        Self {
            max_join: 4,
            phonetic: true,
        }
    }
}

/// A rough sound of a word: its first letter, then its consonant sounds,
/// with silent and doubled letters dropped ("vaani" and "Vanny" -> "vn",
/// "hyprland" and "Hipperland" -> "hprlnd").
pub fn sound_key(word: &str) -> String {
    let letters: Vec<char> = letters_of(word)
        .into_iter()
        .filter(char::is_ascii_alphabetic)
        .collect();
    let mut sounds: Vec<char> = Vec::with_capacity(letters.len());
    let mut i = 0;
    while i < letters.len() {
        let c = letters[i];
        let next = letters.get(i + 1).copied();
        let soft = matches!(next, Some('e' | 'i' | 'y'));
        let sound = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' => {
                if i == 0 {
                    Some('a')
                } else {
                    None
                }
            }
            'p' if next == Some('h') => {
                i += 1;
                Some('f')
            }
            'g' if next == Some('h') => {
                i += 1;
                if i == 1 { Some('g') } else { None }
            }
            'k' if i == 0 && next == Some('n') => None,
            'w' if i == 0 && next == Some('r') => None,
            'w' | 'h' => {
                if i == 0 {
                    Some(c)
                } else {
                    None
                }
            }
            'c' if next == Some('k') => {
                i += 1;
                Some('k')
            }
            'c' | 's' | 'z' if c != 'c' || soft => Some('s'),
            'c' | 'q' => Some('k'),
            'x' => Some('k'),
            'g' if soft => Some('j'),
            'd' | 't' => Some('t'),
            'b' | 'p' => Some('p'),
            'f' | 'v' => Some('f'),
            other => Some(other),
        };
        if let Some(sound) = sound
            && sounds.last() != Some(&sound)
        {
            sounds.push(sound);
        }
        i += 1;
    }
    sounds.into_iter().collect()
}

/// Swap words the model got one letter off ("Ghosty", "Tsunoto"), or two
/// for long words ("Dun Lytics" for "daanlytics"), for the listed word. Only
/// words the English list does not know are touched, so "code" never turns
/// into "Xcode".
pub fn fix_near_misses(text: &str, words: &SessionWords, english: &EnglishWords) -> String {
    fix_near_misses_with(text, words, english, FixOptions::default())
}

/// `fix_near_misses` with explicit options (benchmarks compare them).
pub fn fix_near_misses_with(
    text: &str,
    words: &SessionWords,
    english: &EnglishWords,
    options: FixOptions,
) -> String {
    // One entry per spelling; the strongest source's casing wins ("sunoto"
    // in the project over "Sunoto" the app), so the two never tie.
    let mut seen = std::collections::HashSet::new();
    let targets: Vec<(Vec<char>, &str, String)> = words
        .all()
        .map(|word| (letters_of(word), word.as_str(), sound_key(word)))
        .filter(|(letters, ..)| allowed_edits(letters.len()) > 0 && seen.insert(letters.clone()))
        .collect();
    if targets.is_empty() {
        return text.to_string();
    }
    // (start, end) byte ranges of the words in `text`; an apostrophe
    // between letters stays inside the word ("Priya's", "it's").
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans = Vec::new();
    let mut start = None;
    for (k, &(index, c)) in chars.iter().enumerate() {
        let inner_apostrophe = matches!(c, '\'' | '\u{2019}')
            && start.is_some()
            && chars
                .get(k + 1)
                .is_some_and(|&(_, next)| next.is_alphanumeric());
        match (c.is_alphanumeric() || inner_apostrophe, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                spans.push((begin, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        spans.push((begin, text.len()));
    }
    // A possessive ending is kept off the match and put back after it.
    let possessive = |span: (usize, usize)| -> usize {
        let word = &text[span.0..span.1];
        let lower = word.to_lowercase();
        if lower.ends_with("'s") || lower.ends_with("\u{2019}s") {
            word.char_indices()
                .rev()
                .nth(1)
                .map_or(0, |(at, _)| span.1 - (span.0 + at))
        } else {
            0
        }
    };
    // The closest listed word and its distance; 0 means the letters match.
    let best_target = |heard: &[char], phonetic: bool| -> Option<(&str, usize)> {
        let mut found: Option<(usize, &str)> = None;
        let mut tie = false;
        let heard_key: String = if phonetic {
            sound_key(&heard.iter().collect::<String>())
        } else {
            String::new()
        };
        for (letters, written, key) in &targets {
            if letters.as_slice() == heard {
                return Some((written, 0));
            }
            let limit = allowed_edits(letters.len());
            // Sounding the same allows up to three letters' difference; a
            // long name that sounds one sound off, up to four.
            let sounds_alike = phonetic && key.len() >= 2 && heard.len() >= 5 && *key == heard_key;
            let key_chars: Vec<char> = key.chars().collect();
            let heard_key_chars: Vec<char> = heard_key.chars().collect();
            let sounds_close = phonetic
                && !sounds_alike
                && key_chars.len() >= 6
                && letters.len() >= 9
                && distance(&heard_key_chars, &key_chars, 1).is_some();
            let limit = if sounds_alike {
                limit.max(3)
            } else if sounds_close {
                limit.max(4)
            } else {
                limit
            };
            if let Some(edits) = distance(heard, letters, limit) {
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
        found
            .filter(|_| !tie)
            .map(|(edits, written)| (written, edits))
    };
    // An uncommon word, and not a name or word the old list has exactly
    // ("William", "Rachel"): those are spelled right as heard. A contraction
    // ("haven't", "won't") is never a misheard name.
    let unknown = |span: (usize, usize)| {
        let word = &text[span.0..span.1 - possessive(span)];
        !word
            .chars()
            .any(|c| c.is_ascii_digit() || matches!(c, '\'' | '\u{2019}'))
            && !english.common(word)
            && !english.listed_exactly(word)
    };
    // Little words never join into a name ("Ruth a" is not "Rutha").
    const LITTLE: &[&str] = &[
        "a", "an", "the", "of", "to", "and", "or", "in", "on", "at", "is", "it", "i", "for", "as",
        "by",
    ];
    let little =
        |span: (usize, usize)| LITTLE.contains(&text[span.0..span.1].to_lowercase().as_str());
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    'words: while i < spans.len() {
        let first = spans[i];
        // Several heard words for one listed word: "Dun Lytics", "mini
        // sign", "C G event tap". Two that are merely close need an uncommon
        // word among them, so "work action" never becomes "workation";
        // three or more must spell it exactly.
        for count in (2..=options.max_join.max(1)).rev() {
            let Some(&last) = spans.get(i + count - 1) else {
                continue;
            };
            let joined = &spans[i..i + count];
            if joined
                .windows(2)
                .any(|pair| !matches!(&text[pair[0].1..pair[1].0], " " | "-"))
                || joined.iter().any(|&span| little(span))
            {
                continue;
            }
            let ending = possessive(last);
            let heard: Vec<char> = letters_of(&text[first.0..last.1 - ending]);
            if heard.len() < 5 {
                continue;
            }
            // Joins match by letters only: sounding alike across two words
            // changed common words on LibriSpeech (1.21% -> 1.29% U-WER).
            let Some((written, edits)) = best_target(&heard, false) else {
                continue;
            };
            let close_enough = edits == 0
                || (count == 2 && heard.len() >= 6 && joined.iter().any(|&span| unknown(span)))
                || (count >= 3
                    && edits == 1
                    && heard.len() >= 9
                    && joined.iter().any(|&span| unknown(span)));
            if close_enough {
                out.push_str(&text[copied..first.0]);
                let start = starts_sentence(text, first.0);
                out.push_str(&written_like(written, &text[first.0..first.1], start));
                copied = last.1 - ending;
                i += count;
                continue 'words;
            }
        }
        // Right letters already, with or without the possessive: the
        // sidecar wrote it as listed.
        let whole = letters_of(&text[first.0..first.1]);
        let exact = targets.iter().any(|(letters, ..)| *letters == whole);
        if !exact && unknown(first) {
            let ending = possessive(first);
            let heard = letters_of(&text[first.0..first.1 - ending]);
            // Sounding alike is looser, so the word must be unknown to the
            // old list in every form ("indians" is not "undines").
            let phonetic = options.phonetic && !english.knows(&text[first.0..first.1 - ending]);
            if heard.len() >= 4
                && let Some((written, edits)) = best_target(&heard, phonetic)
                && edits > 0
            {
                out.push_str(&text[copied..first.0]);
                let start = starts_sentence(text, first.0);
                out.push_str(&written_like(written, &text[first.0..first.1], start));
                copied = first.1 - ending;
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
pub fn find_projects(home: &Path, allow_protected: bool) -> HashMap<String, PathBuf> {
    let mut projects = HashMap::new();
    for root in PROJECT_ROOTS {
        let root = home.join(root);
        if !allow_protected && is_protected(&root, home) {
            continue;
        }
        for dir in child_dirs(&root) {
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

fn app_names(home: &Path) -> Vec<String> {
    APP_FOLDERS
        .iter()
        .map(|folder| home.join(folder))
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
    /// The rest of the window's visible text, when it was read.
    pub window_text: Option<String>,
    /// Elements visited reading `window_text`, when the window was walked.
    pub walked_elements: Option<u32>,
}

/// The project the focused window is in: the folder of its document (a
/// terminal's working folder, an open file), a path in its title, or a known
/// project name in its title (an editor's "file — project").
pub fn detect_project(
    focus: &Focus,
    projects: &HashMap<String, PathBuf>,
    home: &Path,
    allow_protected: bool,
) -> Option<PathBuf> {
    let allowed = |path: &Path| allow_protected || !is_protected(path, home);
    if let Some(path) = focus
        .document
        .as_deref()
        .and_then(|document| document.strip_prefix("file://"))
        .map(decode_percent)
        .map(PathBuf::from)
        .filter(|path| allowed(path) && path.exists())
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
        if allowed(&path) && path.exists() {
            return project_root(&path, home);
        }
    }
    let title_words: Vec<String> = title
        .split(|c: char| c.is_whitespace() || "—–|:·()[]".contains(c))
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
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
fn read_focus(pid: Option<i32>, sources: &Sources) -> Option<Focus> {
    let options = sunoto_desktop::SnapshotOptions {
        max_chars: SCREEN_CHARS,
        walk_window: sources.walk_window,
        wake_electron: sources.wake_electron,
    };
    let snapshot = match pid {
        Some(pid) => sunoto_desktop::app_snapshot(pid, options),
        None => sunoto_desktop::focus_snapshot(options),
    }?;
    Some(Focus {
        window_title: snapshot.window_title,
        document: snapshot.document,
        text: snapshot.text,
        window_text: snapshot.window_text,
        walked_elements: snapshot.walk.map(|walk| walk.elements),
    })
}

#[cfg(not(target_os = "macos"))]
fn read_focus(_pid: Option<i32>, _sources: &Sources) -> Option<Focus> {
    None
}

/// Where the everywhere list looks besides project folders and apps.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// Subfolders of Documents and Desktop ("Okafor Holdings").
    pub documents: bool,
    /// Names of documents opened in the last two weeks (Spotlight).
    pub recent_files: bool,
    /// Recent file names given directly, instead of asking Spotlight.
    pub recent: Vec<String>,
    /// People's names (from Contacts, or given directly).
    pub contacts: Vec<String>,
    /// Keep rare English words on the lists (see `EnglishWords::keep_rare`).
    pub keep_rare: bool,
    /// Also read the rest of the window when the focused box is nearly empty.
    pub walk_window: bool,
    /// Turn on Electron apps' accessibility so they share their text.
    pub wake_electron: bool,
    /// Read people's names from Contacts, when access was already granted.
    pub system_contacts: bool,
}

/// Turning the source on is the user's yes to macOS's Contacts dialog.
#[cfg(target_os = "macos")]
fn system_contact_names() -> Vec<String> {
    sunoto_desktop::contact_names(5_000, true).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn system_contact_names() -> Vec<String> {
    Vec::new()
}

/// Names of documents used in the last two weeks, from Spotlight. Slow
/// (about 2 s), so it runs on its own thread and the list catches up.
fn spotlight_recent(home: &Path) -> Vec<String> {
    let output = std::process::Command::new("/usr/bin/mdfind")
        .arg("-onlyin")
        .arg(home)
        .arg("kMDItemLastUsedDate >= $time.today(-14) && kMDItemContentTypeTree == \"public.content\"")
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.contains("/Library/") && !line.contains("/."))
        .filter_map(|line| {
            Path::new(line)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .take(400)
        .collect()
}

/// Builds word lists and keeps the slow parts (folders, projects) cached.
pub struct Collector {
    pub english: Arc<EnglishWords>,
    extra: Vec<String>,
    sources: Sources,
    home: PathBuf,
    everywhere: Option<(Instant, Vec<String>, HashMap<String, PathBuf>)>,
    project_cache: HashMap<PathBuf, (Instant, Vec<String>)>,
    /// Spotlight's latest answer, filled by its own thread.
    spotlight: Arc<std::sync::Mutex<Vec<String>>>,
    spotlight_asked: Option<Instant>,
}

impl Collector {
    pub fn new(extra: Vec<String>, sources: Sources) -> Self {
        Self::with_home(extra, sources, home().unwrap_or_default())
    }

    pub fn with_home(extra: Vec<String>, sources: Sources, home: PathBuf) -> Self {
        let mut english = EnglishWords::load();
        english.keep_rare = sources.keep_rare;
        Self {
            english: Arc::new(english),
            extra,
            sources,
            home,
            everywhere: None,
            project_cache: HashMap::new(),
            spotlight: Arc::default(),
            spotlight_asked: None,
        }
    }

    /// Ask Spotlight again in the background when its answer is old.
    fn refresh_spotlight(&mut self) {
        if !self.sources.recent_files
            || self
                .spotlight_asked
                .is_some_and(|at| at.elapsed() <= REFRESH)
        {
            return;
        }
        self.spotlight_asked = Some(Instant::now());
        let (home, answer) = (self.home.clone(), Arc::clone(&self.spotlight));
        std::thread::spawn(move || {
            let found = spotlight_recent(&home);
            *answer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = found;
        });
    }

    /// Read the folder and app names again when they are older than REFRESH.
    fn refresh_everywhere(&mut self) {
        let stale = self
            .everywhere
            .as_ref()
            .is_none_or(|(at, ..)| at.elapsed() > REFRESH);
        self.refresh_spotlight();
        if stale {
            let projects = find_projects(&self.home, self.sources.documents);
            // People and apps first: a busy Mac has more names than the
            // list keeps. Their names count from four letters, and brands.
            let mut people: Vec<String> = self.sources.contacts.clone();
            if self.sources.system_contacts {
                people.extend(system_contact_names());
            }
            people.extend(app_names(&self.home));
            let refs: Vec<&str> = people.iter().map(String::as_str).collect();
            let mut words = name_words(&refs, EVERYWHERE_MAX, &self.english);
            let mut names: Vec<String> = Vec::new();
            names.extend(self.sources.recent.iter().cloned());
            names.extend(
                self.spotlight
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .iter()
                    .cloned(),
            );
            let mut folders: Vec<String> = projects.keys().cloned().collect();
            if self.sources.documents {
                for root in DOCUMENT_ROOTS {
                    folders.extend(
                        child_dirs(&self.home.join(root))
                            .iter()
                            .filter_map(|dir| dir.file_name())
                            .map(|name| name.to_string_lossy().into_owned()),
                    );
                }
            }
            folders.sort();
            names.extend(folders);
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let room = EVERYWHERE_MAX.saturating_sub(words.len());
            let seen: std::collections::HashSet<String> =
                words.iter().map(|word| word.to_lowercase()).collect();
            words.extend(
                unusual_words(&refs, NAME_MIN_LETTERS, EVERYWHERE_MAX, &self.english)
                    .into_iter()
                    .filter(|word| !seen.contains(&word.to_lowercase()))
                    .take(room),
            );
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
            Some((_, words, projects)) => (
                words.clone(),
                detect_project(&focus, projects, &self.home, self.sources.documents),
            ),
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
            focus.window_text.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let read = ScreenRead {
            window: focus.walked_elements.is_some(),
            elements: focus.walked_elements.unwrap_or(0),
            chars: texts.iter().map(|text| text.chars().count() as u32).sum(),
        };
        let mut screen: Vec<String> = self.extra.clone();
        screen.extend(unusual_words(
            &texts,
            SCREEN_MIN_LETTERS,
            SCREEN_MAX,
            &self.english,
        ));
        let mut words = SessionWords {
            screen,
            project,
            everywhere,
            project_dir,
            read,
        };
        merge_spellings(&mut words);
        words
    }

    pub fn collect_now(&mut self) -> SessionWords {
        let focus = read_focus(None, &self.sources);
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
    pub fn spawn(extra: Vec<String>, sources: Sources, events: Sender<DaemonEvent>) -> Self {
        let (jobs, inbox): (Sender<Job>, Receiver<Job>) = mpsc::channel();
        std::thread::Builder::new()
            .name("word-list".into())
            .spawn(move || {
                let mut collector = Collector::new(extra, sources);
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
    #[serde(default)]
    options: FixOptions,
}

/// `sunoto-daemon words`: print the word list for the focused window now
/// (`--delay S` waits first, `--pid N` reads that app instead, `--json` for
/// machine output). For benchmarks: `--home DIR` reads another home folder,
/// `--text FILE` and `--document URL` stand in for the focused window,
/// `--documents`, `--spotlight`, `--recent FILE` and `--contacts FILE` turn
/// on those sources. With `--fix FILE` it applies `fix_near_misses` to
/// `[{"text", "words", "options"}]` and prints the fixed texts as JSON (for
/// tools/vocab/bench_words.py).
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
            .map(|item| fix_near_misses_with(&item.text, &item.words, &english, item.options))
            .collect();
        let took = started.elapsed();
        eprintln!(
            "fixed {} texts in {:.1} ms ({:.3} ms each)",
            items.len(),
            took.as_secs_f64() * 1e3,
            took.as_secs_f64() * 1e3 / items.len().max(1) as f64
        );
        println!(
            "{}",
            serde_json::to_string(&fixed).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    // `--filter FILE`: JSON array of word lists -> the words a list keeps.
    if let Some(path) = value_of("--filter") {
        let lists: Vec<Vec<String>> = serde_json::from_str(
            &std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?,
        )
        .map_err(|error| format!("{path}: {error}"))?;
        let mut english = EnglishWords::load();
        english.keep_rare = args.iter().any(|arg| arg == "--keep-rare");
        let kept: Vec<Vec<&String>> = lists
            .iter()
            .map(|list| {
                list.iter()
                    .filter(|word| is_unusual(word, SCREEN_MIN_LETTERS, &english))
                    .collect()
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&kept).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if let Some(delay) = value_of("--delay") {
        let seconds: f64 = delay.parse().map_err(|_| format!("bad --delay {delay}"))?;
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
    let lines_of = |flag: &str| -> Result<Vec<String>, String> {
        match value_of(flag) {
            Some(path) => Ok(std::fs::read_to_string(path)
                .map_err(|error| format!("{path}: {error}"))?
                .lines()
                .map(str::to_string)
                .filter(|line| !line.trim().is_empty())
                .collect()),
            None => Ok(Vec::new()),
        }
    };
    let sources = Sources {
        documents: args.iter().any(|arg| arg == "--documents"),
        recent_files: args.iter().any(|arg| arg == "--spotlight"),
        recent: lines_of("--recent")?,
        contacts: lines_of("--contacts")?,
        keep_rare: args.iter().any(|arg| arg == "--keep-rare"),
        walk_window: args.iter().any(|arg| arg == "--walk"),
        wake_electron: args.iter().any(|arg| arg == "--wake"),
        system_contacts: args.iter().any(|arg| arg == "--system-contacts"),
    };
    let home_dir = value_of("--home")
        .map(PathBuf::from)
        .or_else(home)
        .unwrap_or_default();
    let started = Instant::now();
    let mut collector = Collector::with_home(Vec::new(), sources.clone(), home_dir);
    let loaded = started.elapsed();
    collector.refresh_everywhere();
    let cold = started.elapsed() - loaded;
    let pid = match value_of("--pid") {
        Some(pid) => Some(pid.parse::<i32>().map_err(|_| format!("bad --pid {pid}"))?),
        None => None,
    };
    let read_started = Instant::now();
    let mut focus = read_focus(pid, &sources);
    // `--text FILE` stands in for the focused text (benchmarks, tests).
    if value_of("--home").is_some() {
        focus = None; // a pretend home has no windows
    }
    if let Some(document) = value_of("--document") {
        focus.get_or_insert_with(Focus::default).document = Some(document.clone());
    }
    if let Some(path) = value_of("--text") {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
        focus.get_or_insert_with(Focus::default).text = Some(text);
    }
    let read = read_started.elapsed();
    let words = collector.collect(focus.clone());
    let total = read_started.elapsed();
    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&words).map_err(|error| error.to_string())?
        );
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
    println!(
        "Folders and apps: read in {} ms (then cached for 10 minutes)",
        cold.as_millis()
    );
    match &focus {
        Some(focus) => println!(
            "Focused window: title {:?}, document {:?}, {} characters of text, {} more in the window; read in {} ms",
            focus.window_title,
            focus.document,
            focus.text.as_deref().map_or(0, |text| text.chars().count()),
            focus
                .window_text
                .as_deref()
                .map_or(0, |text| text.chars().count()),
            read.as_millis()
        ),
        None => println!("Focused window: nothing readable (Accessibility permission?)"),
    }
    println!(
        "Word list built in {} ms: {}",
        total.as_millis(),
        words.summary()
    );
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
                "tiny", "crash", "note", "load", "run", "stop", "who", "else", "is", "free", "dun",
                "live", "kit", "analytic", "weave", "a", "i", "ai",
            ]
            .into_iter(),
        )
    }

    #[test]
    fn english_knows_plurals_and_verb_forms() {
        let words = english();
        for word in [
            "code",
            "Codes",
            "crashes",
            "notes",
            "loaded",
            "running",
            "stopped",
            "analytics",
        ] {
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
        for word in [
            "daanlytics",
            "huskyAI",
            "tinySDS3d",
            "Ghostty",
            "Sunoto",
            "XTEST",
        ] {
            assert!(is_unusual(word, 5, &words), "{word}");
        }
        // English, ordinary words run together, too short, ids and hashes.
        for word in [
            "code",
            "Crashes",
            "TestFlight",
            "free",
            "f3d",
            "dc6cf6d5",
            "12345abc",
        ] {
            assert!(!is_unusual(word, 5, &words), "{word}");
        }
    }

    #[test]
    fn unusual_words_ranks_by_frequency_and_dedups_case() {
        let words = english();
        let found = unusual_words(
            &[
                "sunoto huskyAI the code",
                "Sunoto daemon: sunoto again",
                "who-else-is-free",
            ],
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
    fn sound_keys_match_names_heard_differently() {
        for (heard, listed) in [
            ("Vanny", "vaani"),
            ("Vonnie", "vaani"),
            ("Hipperland", "Hyprland"),
            ("Gosti", "Ghostty"),
            ("Fone", "phone"),
        ] {
            assert_eq!(sound_key(heard), sound_key(listed), "{heard} / {listed}");
        }
        assert_ne!(sound_key("Bonnie"), sound_key("vaani"));
    }

    #[test]
    fn longer_joins_and_sound_matching_by_default() {
        let words = english();
        let listed = list(&["vaani", "pbcopy", "CGEventTap", "weif"]);
        let text = "Ask Vanny to use P B copy after the C G event tap.";
        let letters_only = FixOptions {
            phonetic: false,
            ..FixOptions::default()
        };
        assert_eq!(
            fix_near_misses_with(text, &listed, &words, letters_only),
            "Ask Vanny to use pbcopy after the CGEventTap."
        );
        let options = FixOptions::default();
        assert_eq!(
            fix_near_misses_with(text, &listed, &words, options),
            "Ask vaani to use pbcopy after the CGEventTap."
        );
        // Sounding alike never touches a common word.
        let text = "Push it to the weave repo.";
        assert_eq!(fix_near_misses_with(text, &listed, &words, options), text);
    }

    #[test]
    fn possessives_and_contractions_stay_whole() {
        let words = english();
        let listed = list(&["Ghostty", "anne's", "goliath", "Gilliam"]);
        assert_eq!(
            fix_near_misses(
                "Ghosty's window. Anne's book, it's goliath.",
                &listed,
                &words
            ),
            "Ghostty's window. Anne's book, it's goliath."
        );
        // A name the old list has is spelled right as heard, and so are
        // contractions and joins through little words.
        let words = EnglishWords::from_words(["william", "ruth"].into_iter());
        assert_eq!(
            fix_near_misses("Ask William.", &listed, &words),
            "Ask William."
        );
        let listed = list(&["woant", "rutha"]);
        let text = "I won't ask Ruth a thing.";
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
        assert_eq!(
            fix_near_misses("Restart the Tsunoto app.", &listed, &words),
            "Restart the sunoto app."
        );
    }

    #[test]
    fn merged_spellings_keep_the_strongest_list_and_the_deliberate_case() {
        let mut words = SessionWords {
            screen: vec!["SUNOTO".into(), "Ghostty".into()],
            project: vec!["sunoto".into(), "nemotron".into()],
            everywhere: vec!["Sunoto".into(), "ghostty".into(), "uniblox".into()],
            ..SessionWords::default()
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
        assert_eq!(
            fix_near_misses("Uniblux is down.", &listed, &words),
            "Uniblox is down."
        );
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
        assert_eq!(
            detect_project(&by_document, &projects, &base, false),
            Some(repo.clone())
        );
        let by_title_path = Focus {
            window_title: Some("zsh ~/workspace/huskyAI/src".into()),
            ..Focus::default()
        };
        assert_eq!(
            detect_project(&by_title_path, &projects, &base, false),
            Some(repo.clone())
        );
        let by_name = Focus {
            window_title: Some("main.rs — huskyAI".into()),
            ..Focus::default()
        };
        assert_eq!(
            detect_project(&by_name, &projects, &base, false),
            Some(repo.clone())
        );
        let elsewhere = Focus {
            window_title: Some("Untitled".into()),
            ..Focus::default()
        };
        assert_eq!(detect_project(&elsewhere, &projects, &base, false), None);
        // A file in Documents is left alone unless the Documents source is on.
        let letters = base.join("Documents/Okafor Holdings");
        std::fs::create_dir_all(&letters).unwrap();
        std::fs::write(letters.join("brief.docx"), "").unwrap();
        let in_documents = Focus {
            document: Some(format!("file://{}/brief.docx", letters.display())),
            ..Focus::default()
        };
        assert_eq!(detect_project(&in_documents, &projects, &base, false), None);
        assert_eq!(
            detect_project(&in_documents, &projects, &base, true),
            Some(letters)
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn project_names_skip_build_output_and_hidden_folders() {
        let base = std::env::temp_dir().join(format!("sunoto-walk-{}", std::process::id()));
        for dir in [
            "crates/sunoto-core",
            "node_modules/leftpad",
            ".git/objects",
            "target/debug",
        ] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        std::fs::write(base.join("crates/nemotron.py"), "").unwrap();
        let names = project_names(&base);
        assert!(names.contains(&"sunoto-core".to_string()));
        assert!(names.contains(&"nemotron".to_string()));
        assert!(
            !names
                .iter()
                .any(|name| name == "leftpad" || name == "objects" || name == "debug")
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn more_sources_add_documents_folders_recent_files_and_contacts() {
        let home = std::env::temp_dir().join(format!("sunoto-sources-{}", std::process::id()));
        std::fs::create_dir_all(home.join("Documents/Okafor Holdings")).unwrap();
        std::fs::create_dir_all(home.join("Desktop/Wexford Appeal")).unwrap();
        let collect = |sources: Sources| {
            let mut collector = Collector {
                english: Arc::new(english()),
                extra: Vec::new(),
                sources,
                home: home.clone(),
                everywhere: None,
                project_cache: HashMap::new(),
                spotlight: Arc::default(),
                spotlight_asked: None,
            };
            collector.collect(None).everywhere
        };
        let plain = collect(Sources::default());
        assert!(!plain.iter().any(|word| word == "Okafor"));
        let more = collect(Sources {
            documents: true,
            recent: vec!["Thibodeaux deposition outline".into()],
            contacts: vec!["Ngozi Okafor".into()],
            ..Sources::default()
        });
        for word in ["Okafor", "Wexford", "Thibodeaux", "Ngozi"] {
            assert!(
                more.iter().any(|listed| listed == word),
                "{word} in {more:?}"
            );
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn keep_rare_lists_rare_english_words_too() {
        let mut words = english();
        words.modern = vec!["the".into()];
        assert!(!is_unusual("analytics", 5, &words));
        words.keep_rare = true;
        // Without the system vocabulary, `common` falls back to `knows`, so
        // only the macOS build can tell rare English from everyday English.
        assert_eq!(
            is_unusual("analytics", 5, &words),
            words.has_system_vocabulary()
        );
    }

    #[test]
    fn collect_puts_screen_words_first_and_extra_words_on_screen() {
        let mut collector = Collector {
            english: Arc::new(english()),
            extra: vec!["Kubernetes".into()],
            sources: Sources::default(),
            home: std::env::temp_dir().join("sunoto-no-home"),
            everywhere: None,
            project_cache: HashMap::new(),
            spotlight: Arc::default(),
            spotlight_asked: None,
        };
        let words = collector.collect(Some(Focus {
            window_title: Some("sunoto — notes".into()),
            text: Some("Restart the Ghostty window".into()),
            ..Focus::default()
        }));
        assert_eq!(words.screen[0], "Kubernetes");
        assert!(words.screen.contains(&"Ghostty".to_string()));
        // "Sunoto" when this Mac has the app installed (merged spellings).
        assert!(
            words
                .screen
                .iter()
                .any(|word| word.eq_ignore_ascii_case("sunoto"))
        );
        assert!(words.project.is_empty());
    }
}

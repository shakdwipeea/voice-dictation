use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub spoken: String,
    pub written: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub trigger: String,
    pub expansion: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyleProfile {
    Default,
    Terminal,
    Prose,
}

/// Maps a focused application to a style profile by matching a
/// case-insensitive substring against the window's WM_CLASS names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleRule {
    pub class_contains: String,
    pub style: StyleProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolishConfig {
    pub resolve_corrections: bool,
    pub remove_fillers: bool,
    pub fillers: Vec<String>,
    pub dictionary: Vec<DictionaryEntry>,
    pub snippets: Vec<Snippet>,
    /// Base style, used when no app_styles rule matches the focused window.
    pub style: StyleProfile,
    pub app_styles: Vec<StyleRule>,
    pub file_references: FileReferenceConfig,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            resolve_corrections: true,
            remove_fillers: true,
            fillers: ["um", "uh", "uhm", "umm", "er", "erm", "hmm", "mhm"]
                .map(str::to_string)
                .to_vec(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
            style: StyleProfile::Prose,
            app_styles: default_app_styles(),
            file_references: FileReferenceConfig::default(),
        }
    }
}

/// Spoken file-reference resolution (e.g. Claude Code `@path` mentions). The
/// daemon gates this on `enabled` and on a matching focused tool; this config
/// carries the triggers and the index bound. Off by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileReferenceConfig {
    /// Master switch (the daemon also requires a matching focused tool).
    pub enabled: bool,
    /// Focused tools that activate resolution (forward-compatible; v1: claude).
    pub tools: Vec<String>,
    /// Nouns that, following a name, mark a file reference ("the daemon file").
    pub trigger_nouns: Vec<String>,
    /// Verbs that, preceding a name, mark a file reference ("open daemon").
    pub trigger_verbs: Vec<String>,
    /// Upper bound on indexed files (used by the daemon's index builder).
    pub max_index_files: usize,
}

impl Default for FileReferenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tools: vec!["claude".to_string()],
            trigger_nouns: ["file", "module", "script"].map(str::to_string).to_vec(),
            trigger_verbs: ["open", "edit", "check", "update", "read", "see"]
                .map(str::to_string)
                .to_vec(),
            max_index_files: 5000,
        }
    }
}

fn default_app_styles() -> Vec<StyleRule> {
    [
        "terminal",
        "xterm",
        "urxvt",
        "konsole",
        "alacritty",
        "kitty",
        "tilix",
        "terminator",
    ]
    .map(|class| StyleRule {
        class_contains: class.to_string(),
        style: StyleProfile::Terminal,
    })
    .to_vec()
}

/// Pick the style for the focused window: the first rule whose substring
/// appears in the window's WM_CLASS instance or class name wins; without a
/// match (or without a readable class) the configured base style applies.
pub fn resolve_style(
    window_class: Option<(&str, &str)>,
    rules: &[StyleRule],
    base: StyleProfile,
) -> StyleProfile {
    let Some((instance, class)) = window_class else {
        return base;
    };
    let instance = instance.to_ascii_lowercase();
    let class = class.to_ascii_lowercase();
    rules
        .iter()
        .find(|rule| {
            let needle = rule.class_contains.trim().to_ascii_lowercase();
            !needle.is_empty() && (instance.contains(&needle) || class.contains(&needle))
        })
        .map(|rule| rule.style)
        .unwrap_or(base)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageTrace {
    pub stage: &'static str,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolishOutcome {
    pub text: String,
    pub trace: Vec<StageTrace>,
}

pub fn polish(raw: &str, config: &PolishConfig) -> PolishOutcome {
    let mut trace = Vec::new();
    let mut text = raw.to_string();

    let next = normalize(&text);
    apply_stage(&mut trace, "normalize", &mut text, next);
    if config.resolve_corrections {
        let next = resolve_corrections(&text);
        apply_stage(&mut trace, "corrections", &mut text, next);
    }
    if config.remove_fillers {
        let next = remove_fillers(&text, &config.fillers);
        apply_stage(&mut trace, "fillers", &mut text, next);
    }
    if !config.dictionary.is_empty() {
        let next = apply_dictionary(&text, &config.dictionary);
        apply_stage(&mut trace, "dictionary", &mut text, next);
    }
    if let Some(expansion) = match_snippet(&text, &config.snippets) {
        apply_stage(&mut trace, "snippets", &mut text, expansion);
        // Snippet expansions are user-authored literal text (may be
        // multi-line); styling and whitespace normalization must not rewrite
        // them.
        return PolishOutcome { text, trace };
    }
    let next = apply_style(&text, config.style);
    apply_stage(&mut trace, "style", &mut text, next);
    let next = normalize(&text);
    apply_stage(&mut trace, "normalize", &mut text, next);

    PolishOutcome { text, trace }
}

fn apply_stage(trace: &mut Vec<StageTrace>, stage: &'static str, text: &mut String, next: String) {
    if *text != next {
        trace.push(StageTrace {
            stage,
            before: text.clone(),
            after: next.clone(),
        });
        *text = next;
    }
}

const SENTENCE_TERMINATORS: [char; 3] = ['.', '!', '?'];
const CLAUSE_PUNCTUATION: [char; 6] = [',', '.', '!', '?', ';', ':'];

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '\'' || character == '-'
}

fn normalize(text: &str) -> String {
    // Collapse all whitespace runs (incl. newlines) into single spaces.
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");

    let chars: Vec<char> = collapsed.chars().collect();
    let mut output: Vec<char> = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == ' ' {
            // Drop spaces that only precede clause punctuation.
            let mut next = index + 1;
            while next < chars.len() && chars[next] == ' ' {
                next += 1;
            }
            if next < chars.len() && CLAUSE_PUNCTUATION.contains(&chars[next]) {
                index = next;
                continue;
            }
            output.push(' ');
            index += 1;
            continue;
        }
        if current == ',' {
            output.push(',');
            // Collapse ", ," and ",," runs into a single comma.
            let mut next = index + 1;
            loop {
                let mut probe = next;
                while probe < chars.len() && chars[probe] == ' ' {
                    probe += 1;
                }
                if probe < chars.len() && chars[probe] == ',' {
                    next = probe + 1;
                } else {
                    break;
                }
            }
            index = next;
            continue;
        }
        output.push(current);
        index += 1;
    }

    // Ensure a space after clause punctuation when a word follows directly,
    // except inside numbers ("3.14", "1,000").
    let chars = output;
    let mut spaced: Vec<char> = Vec::with_capacity(chars.len());
    for (index, &current) in chars.iter().enumerate() {
        spaced.push(current);
        if !CLAUSE_PUNCTUATION.contains(&current) {
            continue;
        }
        let Some(&next) = chars.get(index + 1) else {
            continue;
        };
        if !next.is_alphanumeric() {
            continue;
        }
        let previous = index.checked_sub(1).and_then(|p| chars.get(p)).copied();
        // Keep tokens whole: a dot inside a domain / file name / identifier /
        // decimal ("google.com", "foo.rs", "3.14") and a comma inside a grouped
        // number ("1,000") must not gain a following space.
        let intra_token = match current {
            '.' => previous.is_some_and(char::is_alphanumeric) && next.is_alphanumeric(),
            ',' => previous.is_some_and(|c| c.is_ascii_digit()) && next.is_ascii_digit(),
            _ => false,
        };
        if !intra_token {
            spaced.push(' ');
        }
    }
    spaced.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
}

impl Token {
    fn word(&self) -> Option<&str> {
        match self {
            Token::Word(word) => Some(word),
            Token::Punct(_) => None,
        }
    }

    fn is_terminator(&self) -> bool {
        matches!(self, Token::Punct(p) if SENTENCE_TERMINATORS.contains(p))
    }
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in text.chars() {
        if is_word_char(character) {
            word.push(character);
            continue;
        }
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(&mut word)));
        }
        if !character.is_whitespace() {
            tokens.push(Token::Punct(character));
        }
    }
    if !word.is_empty() {
        tokens.push(Token::Word(word));
    }
    tokens
}

fn detokenize(tokens: &[Token]) -> String {
    let mut output = String::new();
    for token in tokens {
        match token {
            Token::Word(word) => {
                if !output.is_empty() && !output.ends_with(' ') {
                    output.push(' ');
                }
                output.push_str(word);
            }
            Token::Punct(punct) => output.push(*punct),
        }
    }
    normalize(&output)
}

fn capitalize_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn shape_compatible(left: &str, right: &str) -> bool {
    let left_first = left.chars().next();
    let right_first = right.chars().next();
    match (left_first, right_first) {
        (Some(l), Some(r)) => {
            (l.is_uppercase() && r.is_uppercase())
                || (l.is_lowercase() && r.is_lowercase())
                || (l.is_ascii_digit() && r.is_ascii_digit())
        }
        _ => false,
    }
}

/// L and R must look alike word-for-word ("next Tuesday" vs "next Wednesday");
/// a single shared shape would let "it Tuesday" match "wednesday morning".
fn clauses_shape_compatible(
    tokens: &[Token],
    left_start: usize,
    right_start: usize,
    len: usize,
) -> bool {
    (0..len).all(|offset| {
        match (
            tokens[left_start + offset].word(),
            tokens[right_start + offset].word(),
        ) {
            (Some(left), Some(right)) => shape_compatible(left, right),
            _ => false,
        }
    })
}

fn equals_ignore_case(word: &str, expected: &str) -> bool {
    word.eq_ignore_ascii_case(expected)
}

const SWAP_MARKERS: [&[&str]; 4] = [
    &["actually"],
    &["i", "mean"],
    &["no", "wait"],
    &["wait", "no"],
];
const RESTART_MARKERS: [&[&str]; 2] = [&["scratch", "that"], &["strike", "that"]];
const SWAP_CLAUSE_CAP: usize = 4;

fn resolve_corrections(text: &str) -> String {
    let mut tokens = tokenize(text);
    let mut changed = false;
    // Each applied correction shortens the token list, so this terminates; the
    // explicit cap guards against any unforeseen non-shrinking rewrite.
    for _ in 0..16 {
        if let Some(next) = apply_restart_marker(&tokens) {
            tokens = next;
            changed = true;
            continue;
        }
        if let Some(next) = apply_swap_marker(&tokens) {
            tokens = next;
            changed = true;
            continue;
        }
        break;
    }
    // Only rebuild when something was corrected: detokenization normalizes
    // token spacing and must not rewrite untouched text.
    if changed {
        detokenize(&tokens)
    } else {
        text.to_string()
    }
}

fn match_marker(tokens: &[Token], index: usize, marker: &[&str]) -> bool {
    marker.iter().enumerate().all(|(offset, expected)| {
        tokens
            .get(index + offset)
            .and_then(Token::word)
            .is_some_and(|word| equals_ignore_case(word, expected))
    })
}

/// "scratch that" / "strike that": drop everything dictated since the start of
/// the previous sentence (the phrase cancels the sentence being corrected).
fn apply_restart_marker(tokens: &[Token]) -> Option<Vec<Token>> {
    for index in 0..tokens.len() {
        let Some(marker) = RESTART_MARKERS
            .iter()
            .find(|marker| match_marker(tokens, index, marker))
        else {
            continue;
        };
        let mut delete_from = sentence_start(tokens, index);
        if delete_from == index {
            // The marker opens its own sentence: cancel the previous sentence,
            // including its terminator.
            let previous_end = delete_from.checked_sub(1)?;
            delete_from = sentence_start(tokens, previous_end);
        }
        let mut delete_to = index + marker.len();
        if matches!(tokens.get(delete_to), Some(Token::Punct(',' | '.'))) {
            delete_to += 1;
        }
        let mut next: Vec<Token> = Vec::with_capacity(tokens.len());
        next.extend_from_slice(&tokens[..delete_from]);
        next.extend_from_slice(&tokens[delete_to..]);
        // The surviving clause now starts a sentence; restore capitalization.
        if (delete_from == 0 || next.get(delete_from - 1).is_some_and(Token::is_terminator))
            && let Some(Token::Word(word)) = next.get_mut(delete_from)
        {
            *word = capitalize_first(word);
        }
        return Some(next);
    }
    None
}

fn sentence_start(tokens: &[Token], index: usize) -> usize {
    (0..index)
        .rev()
        .find(|&candidate| tokens[candidate].is_terminator())
        .map(|terminator| terminator + 1)
        .unwrap_or(0)
}

/// "<L tokens>, <marker> <R tokens>": the speaker replaced L with R mid-flight
/// ("Tuesday, actually Wednesday"). Applied only with a comma before the
/// marker, a short following clause, and shape-compatible first words.
fn apply_swap_marker(tokens: &[Token]) -> Option<Vec<Token>> {
    for index in 0..tokens.len() {
        if !matches!(
            index.checked_sub(1).map(|p| &tokens[p]),
            Some(Token::Punct(','))
        ) {
            continue;
        }
        let Some(marker) = SWAP_MARKERS
            .iter()
            .find(|marker| match_marker(tokens, index, marker))
        else {
            continue;
        };
        let mut replacement_start = index + marker.len();
        if matches!(tokens.get(replacement_start), Some(Token::Punct(','))) {
            replacement_start += 1;
        }
        let mut clause_len = 0;
        while let Some(Token::Word(_)) = tokens.get(replacement_start + clause_len) {
            clause_len += 1;
        }
        if clause_len == 0 || clause_len > SWAP_CLAUSE_CAP {
            continue;
        }
        // L: the same number of contiguous words right before the comma.
        let comma = index - 1;
        if comma < clause_len {
            continue;
        }
        let replaced_start = comma - clause_len;
        let all_words = (replaced_start..comma).all(|i| tokens[i].word().is_some());
        if !all_words {
            continue;
        }
        if !clauses_shape_compatible(tokens, replaced_start, replacement_start, clause_len) {
            continue;
        }
        let mut next: Vec<Token> = Vec::with_capacity(tokens.len());
        next.extend_from_slice(&tokens[..replaced_start]);
        next.extend_from_slice(&tokens[replacement_start..]);
        return Some(next);
    }
    None
}

fn remove_fillers(text: &str, fillers: &[String]) -> String {
    let filler_phrases: Vec<Vec<String>> = fillers
        .iter()
        .map(|filler| {
            filler
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .filter(|phrase: &Vec<String>| !phrase.is_empty())
        .collect();
    if filler_phrases.is_empty() {
        return text.to_string();
    }

    let tokens = tokenize(text);
    let mut output: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut capitalize_next = false;
    let mut changed = false;
    let mut index = 0;
    while index < tokens.len() {
        let matched = filler_phrases.iter().find(|phrase| {
            phrase.iter().enumerate().all(|(offset, expected)| {
                tokens
                    .get(index + offset)
                    .and_then(Token::word)
                    .is_some_and(|word| word.eq_ignore_ascii_case(expected))
            })
        });
        let Some(phrase) = matched else {
            let mut token = tokens[index].clone();
            if capitalize_next && let Token::Word(word) = &mut token {
                let capitalized = capitalize_first(word);
                changed |= capitalized != *word;
                *word = capitalized;
                capitalize_next = false;
            }
            output.push(token);
            index += 1;
            continue;
        };
        changed = true;
        let at_sentence_start =
            output.is_empty() || output.last().is_some_and(Token::is_terminator);
        index += phrase.len();
        // A filler bounded by commas was a parenthetical aside; both commas
        // disappear with it ("I, uh, think" -> "I think").
        if matches!(tokens.get(index), Some(Token::Punct(','))) {
            index += 1;
        }
        if matches!(output.last(), Some(Token::Punct(','))) {
            output.pop();
        }
        if at_sentence_start {
            // The filler opened the sentence ("Um, send it"), so the next word
            // inherits the sentence-start capitalization.
            capitalize_next = true;
        }
    }
    if changed {
        detokenize(&output)
    } else {
        text.to_string()
    }
}

fn apply_dictionary(text: &str, entries: &[DictionaryEntry]) -> String {
    let mut tokens = tokenize(text);
    let mut changed = false;
    for entry in entries {
        let spoken: Vec<String> = entry
            .spoken
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        if spoken.is_empty() {
            continue;
        }
        let mut output: Vec<Token> = Vec::with_capacity(tokens.len());
        let mut index = 0;
        while index < tokens.len() {
            let matches = spoken.iter().enumerate().all(|(offset, expected)| {
                tokens
                    .get(index + offset)
                    .and_then(Token::word)
                    .is_some_and(|word| word.eq_ignore_ascii_case(expected))
            });
            if matches {
                changed |= tokens[index].word() != Some(entry.written.as_str()) || spoken.len() > 1;
                output.push(Token::Word(entry.written.clone()));
                index += spoken.len();
            } else {
                output.push(tokens[index].clone());
                index += 1;
            }
        }
        tokens = output;
    }
    if changed {
        detokenize(&tokens)
    } else {
        text.to_string()
    }
}

fn match_snippet(text: &str, snippets: &[Snippet]) -> Option<String> {
    let trimmed = text.trim();
    let without_terminator = trimmed
        .strip_suffix(SENTENCE_TERMINATORS)
        .unwrap_or(trimmed);
    snippets
        .iter()
        .find(|snippet| {
            let trigger = snippet.trigger.trim();
            !trigger.is_empty() && without_terminator.eq_ignore_ascii_case(trigger)
        })
        .map(|snippet| snippet.expansion.clone())
}

fn apply_style(text: &str, style: StyleProfile) -> String {
    match style {
        StyleProfile::Default => text.to_string(),
        StyleProfile::Terminal => {
            // Shell input rarely wants the dictated trailing period.
            if text.ends_with('.') && !text.ends_with("..") {
                text[..text.len() - 1].to_string()
            } else {
                text.to_string()
            }
        }
        StyleProfile::Prose => format_prose(text),
    }
}

/// A whitespace-free token with an internal dot flanked by alphanumerics — a
/// URL, domain, file name, or dotted identifier ("google.com", "foo.rs",
/// "os.path") — which prose styling must treat literally.
fn is_dotted_token(text: &str) -> bool {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .windows(3)
        .any(|w| w[1] == '.' && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}

fn format_prose(text: &str) -> String {
    let trimmed = text.trim();
    // A bare dotted token (URL, domain, file name, dotted identifier) is literal
    // text — don't capitalize it or append a sentence period.
    if is_dotted_token(trimmed) {
        return trimmed.to_string();
    }
    let mut chars: Vec<char> = trimmed.chars().collect();
    if !chars.iter().any(|character| character.is_alphabetic()) {
        return trimmed.to_string();
    }
    chars = capitalize_sentences(&chars);

    let punctuation_index = chars
        .iter()
        .rposition(|character| !matches!(character, '"' | '\'' | ')' | ']' | '}'));
    let Some(index) = punctuation_index else {
        return chars.into_iter().collect();
    };
    // Nemotron already predicts punctuation. Preserve any punctuation it
    // emitted and add a period only when the utterance plainly ends in text.
    if !chars[index].is_alphanumeric() {
        return chars.into_iter().collect();
    }

    chars.insert(index + 1, '.');
    chars.into_iter().collect()
}

fn capitalize_sentences(chars: &[char]) -> Vec<char> {
    let mut output = Vec::with_capacity(chars.len());
    let mut capitalize_next = true;
    for (index, &character) in chars.iter().enumerate() {
        if capitalize_next && character.is_alphabetic() {
            output.extend(character.to_uppercase());
            capitalize_next = false;
            continue;
        }
        output.push(character);
        if SENTENCE_TERMINATORS.contains(&character) {
            // A dot inside a token ("google.com", "foo.rs", "3.14") is not a
            // sentence boundary, so the following letter must not be capitalized.
            let intra_word_dot = character == '.'
                && index
                    .checked_sub(1)
                    .and_then(|previous| chars.get(previous))
                    .is_some_and(|previous| previous.is_alphanumeric())
                && chars
                    .get(index + 1)
                    .is_some_and(|next| next.is_alphanumeric());
            if !intra_word_dot {
                capitalize_next = true;
            }
        }
    }
    output
}

// ---------------------------------------------------------------------------
// File-reference resolution
//
// Rewrites spoken file references to `@relative/path` mentions for tools (like
// Claude Code) that understand them. A pure transform over a file list the
// daemon supplies from the focused tool's working directory; the daemon gates
// it on `FileReferenceConfig::enabled` and the focused tool.
//
// Conservative by construction: a span is rewritten only when it maps to a
// single, unambiguous file. Anything ambiguous or unmatched is left untouched,
// the same safe-fallback rule the correction and snippet stages follow.
// ---------------------------------------------------------------------------

const REF_DETERMINERS: [&str; 14] = [
    "the", "a", "an", "this", "that", "these", "those", "my", "our", "your", "his", "her", "their",
    "its",
];
const REF_STOPWORDS: [&str; 11] = [
    "to", "of", "in", "on", "at", "and", "or", "it", "please", "then", "also",
];
/// Spoken punctuation words dropped from a file name before matching, so
/// "claude hyphen code dot rs" reduces to the tokens claude / code / rs.
const REF_SEPARATORS: [&str; 8] = [
    "hyphen", "dash", "dot", "point", "period", "underscore", "slash", "space",
];
/// Most spoken words taken as a file name on either side of a trigger word.
const RUN_CAP: usize = 8;

struct RefWord {
    lower: String,
    start: usize,
    end: usize,
}

/// Word tokens (alphanumeric plus `_`, `-`, `.`) with their byte spans in the
/// original text, so resolved references can be spliced back precisely.
fn ref_words(text: &str) -> Vec<RefWord> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, character) in text.char_indices() {
        let is_word = character.is_alphanumeric() || matches!(character, '_' | '-' | '.');
        match (is_word, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                out.push(RefWord {
                    lower: text[begin..index].to_lowercase(),
                    start: begin,
                    end: index,
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        out.push(RefWord {
            lower: text[begin..].to_lowercase(),
            start: begin,
            end: text.len(),
        });
    }
    out
}

fn is_determiner(word: &str) -> bool {
    REF_DETERMINERS.contains(&word)
}

/// (basename, stem), both lowercased, for a forward-slash relative path.
fn path_keys(path: &str) -> (String, String) {
    let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let stem = match base.rfind('.') {
        Some(dot) if dot > 0 => base[..dot].to_string(),
        _ => base.clone(),
    };
    (base, stem)
}

/// The one file matching `pred(path_lower, basename_lower, stem_lower)`, or
/// `None` if zero or more than one match.
fn unique_where<'a>(
    files: &'a [String],
    keyed: &[(String, String, String)],
    pred: impl Fn(&str, &str, &str) -> bool,
) -> Option<&'a str> {
    let mut hit: Option<&'a str> = None;
    for (file, (path, base, stem)) in files.iter().zip(keyed.iter()) {
        if pred(path, base, stem) {
            if hit.is_some() {
                return None;
            }
            hit = Some(file.as_str());
        }
    }
    hit
}

/// Resolve a spoken name (lowercased word tokens) to one file path, trying
/// tiers from most precise to most forgiving and returning the first tier that
/// yields a unique answer.
fn resolve_query<'a>(query: &[String], files: &'a [String]) -> Option<&'a str> {
    let toks: Vec<String> = query
        .iter()
        .map(|token| token.trim_matches('.').to_lowercase())
        .filter(|token| {
            token.len() >= 2
                && !REF_SEPARATORS.contains(&token.as_str())
                && !REF_STOPWORDS.contains(&token.as_str())
        })
        .collect();
    if toks.is_empty() {
        return None;
    }
    let keyed: Vec<(String, String, String)> = files
        .iter()
        .map(|file| {
            let (base, stem) = path_keys(file);
            (file.to_lowercase(), base, stem)
        })
        .collect();

    // Tier 1: basename/stem equals a separator-aware join ("daemon" "rs" or
    // "daemon" "dot" "rs" -> "daemon.rs"; "agents" "md" -> "agents.md").
    let mut joins = [
        toks.join(""),
        toks.join("-"),
        toks.join("_"),
        toks.join("."),
    ];
    joins.sort();
    if let Some(found) = unique_where(files, &keyed, |_, base, stem| {
        joins
            .iter()
            .any(|join| join.as_str() == base || join.as_str() == stem)
    }) {
        return Some(found);
    }
    // Tier 2: the last token names the file; earlier tokens locate it in the
    // path ("polish" "lib" -> the lib.rs under sunoto-polish).
    let (head, quals) = toks.split_last().unwrap();
    if let Some(found) = unique_where(files, &keyed, |path, base, stem| {
        (base == head.as_str() || stem == head.as_str())
            && quals.iter().all(|qualifier| path.contains(qualifier.as_str()))
    }) {
        return Some(found);
    }
    // Tier 3: every token appears in the basename ("integration" "plan" "md").
    if let Some(found) = unique_where(files, &keyed, |_, base, _| {
        toks.iter().all(|token| base.contains(token.as_str()))
    }) {
        return Some(found);
    }
    // Tier 4: the basename containing the most tokens, given a strict winner
    // with at least two hits (tolerates one ASR slip, e.g. "cloud" for
    // "claude").
    let mut best: Option<&'a str> = None;
    let mut best_score = 1usize;
    let mut tie = false;
    for (file, (_, base, _)) in files.iter().zip(keyed.iter()) {
        let score = toks
            .iter()
            .filter(|token| base.contains(token.as_str()))
            .count();
        if score > best_score {
            best = Some(file.as_str());
            best_score = score;
            tie = false;
        } else if score == best_score && best.is_some() {
            tie = true;
        }
    }
    if best_score >= 2 && !tie { best } else { None }
}

/// Rewrite spoken file references in `text` to `@path` mentions using `files`
/// (repo-relative paths). See the module note above for the safety contract.
pub fn resolve_file_references(
    text: &str,
    files: &[String],
    config: &FileReferenceConfig,
) -> String {
    if files.is_empty() {
        return text.to_string();
    }
    let nouns: Vec<String> = config
        .trigger_nouns
        .iter()
        .map(|word| word.to_lowercase())
        .collect();
    let verbs: Vec<String> = config
        .trigger_verbs
        .iter()
        .map(|word| word.to_lowercase())
        .collect();
    if nouns.is_empty() && verbs.is_empty() {
        return text.to_string();
    }
    let words = ref_words(text);
    let is_name = |word: &str| -> bool {
        word.len() >= 2
            && word.chars().any(|character| character.is_alphanumeric())
            && !is_determiner(word)
            && !REF_STOPWORDS.contains(&word)
            && !nouns.iter().any(|noun| noun.as_str() == word)
            && !verbs.iter().any(|verb| verb.as_str() == word)
    };

    // Pending rewrites as (start, end, replacement) byte spans.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let current = word.lower.as_str();
        // Noun-suffix pattern: "<name...> <noun>" (e.g. "the daemon file").
        if nouns.iter().any(|noun| noun.as_str() == current) {
            let mut run_start = index;
            while run_start > 0 && index - run_start < RUN_CAP && is_name(&words[run_start - 1].lower)
            {
                run_start -= 1;
            }
            if run_start < index {
                let mut span_start = words[run_start].start;
                if run_start > 0 && is_determiner(&words[run_start - 1].lower) {
                    span_start = words[run_start - 1].start;
                }
                let query: Vec<String> = words[run_start..index]
                    .iter()
                    .map(|word| word.lower.clone())
                    .collect();
                if let Some(path) = resolve_query(&query, files) {
                    edits.push((span_start, word.end, format!("@{path}")));
                }
            }
        }
        // Verb-prefix pattern: "<verb> <name...>" (e.g. "open daemon").
        if verbs.iter().any(|verb| verb.as_str() == current) {
            let mut cursor = index + 1;
            let mut span_start = None;
            if cursor < words.len() && is_determiner(&words[cursor].lower) {
                span_start = Some(words[cursor].start);
                cursor += 1;
            }
            let run_begin = cursor;
            while cursor < words.len() && cursor - run_begin < RUN_CAP && is_name(&words[cursor].lower)
            {
                cursor += 1;
            }
            if cursor > run_begin {
                let span_start = span_start.unwrap_or(words[run_begin].start);
                let query: Vec<String> = words[run_begin..cursor]
                    .iter()
                    .map(|word| word.lower.clone())
                    .collect();
                if let Some(path) = resolve_query(&query, files) {
                    edits.push((span_start, words[cursor - 1].end, format!("@{path}")));
                }
            }
        }
    }
    if edits.is_empty() {
        return text.to_string();
    }
    // Apply right-to-left; for equal starts the longer span wins, so a noun
    // pattern that absorbs the trailing noun beats an overlapping verb pattern.
    edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut result = text.to_string();
    let mut next_boundary = result.len();
    for (start, end, replacement) in edits {
        if end > next_boundary {
            continue; // overlaps an already-applied (rightward) edit
        }
        result.replace_range(start..end, &replacement);
        next_boundary = start;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> PolishConfig {
        PolishConfig::default()
    }

    fn run(text: &str) -> String {
        polish(text, &defaults()).text
    }

    fn repo_files() -> Vec<String> {
        [
            "apps/daemon/src/daemon.rs",
            "apps/daemon/src/settings.rs",
            "crates/sunoto-polish/src/lib.rs",
            "crates/sunoto-core/src/lib.rs",
            "docs/claude-code-integration-plan.md",
            "Cargo.toml",
            "Makefile",
            "README.md",
            "AGENTS.md",
        ]
        .map(str::to_string)
        .to_vec()
    }

    fn refs(text: &str) -> String {
        resolve_file_references(text, &repo_files(), &FileReferenceConfig::default())
    }

    #[test]
    fn file_references_resolve_unique_matches() {
        assert_eq!(
            refs("look at the daemon file"),
            "look at @apps/daemon/src/daemon.rs"
        );
        assert_eq!(refs("open settings"), "open @apps/daemon/src/settings.rs");
        // A qualifier disambiguates an otherwise ambiguous stem ("lib").
        assert_eq!(
            refs("edit the polish lib file"),
            "edit @crates/sunoto-polish/src/lib.rs"
        );
        // Spoken extension: "daemon dot rs" -> daemon.rs.
        assert_eq!(refs("the daemon dot rs file"), "@apps/daemon/src/daemon.rs");
        // Spoken compound: "make file" -> Makefile.
        assert_eq!(refs("go through the make file"), "go through @Makefile");
        // Multi-word name reconstructs the hyphenated stem.
        assert_eq!(
            refs("open the claude code integration plan file"),
            "open @docs/claude-code-integration-plan.md"
        );
        // Two references in one utterance.
        assert_eq!(
            refs("open the daemon file and the settings file"),
            "open @apps/daemon/src/daemon.rs and @apps/daemon/src/settings.rs"
        );
    }

    #[test]
    fn file_references_tolerate_asr_noise() {
        // "agents.md" survives polish splitting it into "agents. md".
        assert_eq!(refs("go to the agents. md file"), "go to @AGENTS.md");
        // Spoken hyphens between the name parts.
        assert_eq!(
            refs("open the claude hyphen code hyphen integration hyphen plan file"),
            "open @docs/claude-code-integration-plan.md"
        );
        // One slipped word ("cloud" for "claude") still resolves by overlap.
        assert_eq!(
            refs("the cloud code integration plan file"),
            "@docs/claude-code-integration-plan.md"
        );
    }

    #[test]
    fn file_references_stay_conservative() {
        // Ambiguous stem with no qualifier: untouched.
        assert_eq!(refs("open the lib file"), "open the lib file");
        // A trigger with no name is not a file request (would not pick Makefile).
        assert_eq!(refs("can you open the file"), "can you open the file");
        // No matching file: untouched.
        assert_eq!(refs("open the door"), "open the door");
        // No trigger word: untouched.
        assert_eq!(refs("the daemon is fast"), "the daemon is fast");
        // Empty index: untouched.
        assert_eq!(
            resolve_file_references("open settings", &[], &FileReferenceConfig::default()),
            "open settings"
        );
        // Idempotent: a resolved mention is not rewritten again.
        let once = refs("look at the daemon file");
        assert_eq!(refs(&once), once);
    }

    #[test]
    fn normalize_fixes_whitespace_and_punctuation_spacing() {
        assert_eq!(normalize("  hello   world "), "hello world");
        assert_eq!(normalize("hello , world ."), "hello, world.");
        assert_eq!(normalize("hello,world"), "hello, world");
        assert_eq!(normalize("one\ntwo\tthree"), "one two three");
        assert_eq!(normalize("a , , b"), "a, b");
        assert_eq!(normalize("a,,b"), "a, b");
    }

    #[test]
    fn normalize_preserves_numbers_and_ellipses() {
        assert_eq!(normalize("pi is 3.14"), "pi is 3.14");
        assert_eq!(normalize("1,000 items"), "1,000 items");
        assert_eq!(normalize("well..."), "well...");
    }

    #[test]
    fn canonical_swap_correction() {
        assert_eq!(run("Tuesday, actually Wednesday."), "Wednesday.");
    }

    #[test]
    fn swap_corrections_must_pass_examples() {
        assert_eq!(
            run("next Tuesday, actually next Wednesday."),
            "Next Wednesday."
        );
        assert_eq!(run("Send it to Bob, I mean Alice."), "Send it to Alice.");
        assert_eq!(run("I will go, actually, run."), "I will run.");
        assert_eq!(
            run("meet on Tuesday, actually Wednesday works better for me too."),
            "Meet on Tuesday, actually Wednesday works better for me too."
        );
        assert_eq!(run("Actually, send it now."), "Actually, send it now.");
    }

    #[test]
    fn swap_requires_shape_compatibility() {
        assert_eq!(
            run("send it Tuesday, actually wednesday morning."),
            "Send it Tuesday, actually wednesday morning."
        );
        assert_eq!(run("page 12, actually 14."), "Page 14.");
    }

    #[test]
    fn no_wait_marker_swaps() {
        assert_eq!(run("at five, no wait six."), "At six.");
    }

    #[test]
    fn restart_marker_drops_the_previous_sentence() {
        assert_eq!(
            run("Send it to Bob. Scratch that, send it to Alice."),
            "Send it to Alice."
        );
        assert_eq!(run("I want pizza scratch that salad."), "Salad.");
        assert_eq!(
            run("Keep this. Bad draft. Strike that. Better one."),
            "Keep this. Better one."
        );
    }

    #[test]
    fn fillers_are_removed_with_adjacent_commas() {
        assert_eq!(run("Um, send the email."), "Send the email.");
        assert_eq!(run("I, uh, think so."), "I think so.");
        assert_eq!(run("It was, um, fine."), "It was fine.");
    }

    #[test]
    fn fillers_do_not_match_inside_words() {
        assert_eq!(
            run("my umbrella, Erica's hammer."),
            "My umbrella, Erica's hammer."
        );
    }

    #[test]
    fn multi_word_fillers_are_supported() {
        let mut config = defaults();
        config.fillers.push("you know".into());
        assert_eq!(polish("It's, you know, fine.", &config).text, "It's fine.");
    }

    #[test]
    fn dictionary_replaces_whole_phrases_case_insensitively() {
        let mut config = defaults();
        config.dictionary.push(DictionaryEntry {
            spoken: "git hub".into(),
            written: "GitHub".into(),
        });
        config.dictionary.push(DictionaryEntry {
            spoken: "antash".into(),
            written: "Antash".into(),
        });
        assert_eq!(
            polish("I pushed to git hub for antash.", &config).text,
            "I pushed to GitHub for Antash."
        );
        assert_eq!(polish("Git Hub, git hub!", &config).text, "GitHub, GitHub!");
    }

    #[test]
    fn dictionary_does_not_match_partial_words() {
        let mut config = defaults();
        config.dictionary.push(DictionaryEntry {
            spoken: "cat".into(),
            written: "CAT".into(),
        });
        assert_eq!(
            polish("concatenate cats.", &config).text,
            "Concatenate cats."
        );
    }

    #[test]
    fn snippet_expands_whole_utterance_and_preserves_newlines() {
        let mut config = defaults();
        config.snippets.push(Snippet {
            trigger: "sign off".into(),
            expansion: "Best regards,\nAntash".into(),
        });
        let outcome = polish("Sign off.", &config);
        assert_eq!(outcome.text, "Best regards,\nAntash");
        assert_eq!(polish("sign off", &config).text, "Best regards,\nAntash");
        assert_eq!(
            polish("please sign off now.", &config).text,
            "Please sign off now."
        );
    }

    #[test]
    fn styles_adjust_only_the_edges() {
        let mut config = defaults();
        config.style = StyleProfile::Terminal;
        assert_eq!(polish("ls dash l.", &config).text, "ls dash l");
        assert_eq!(polish("waiting...", &config).text, "waiting...");
        config.style = StyleProfile::Prose;
        assert_eq!(polish("hello there.", &config).text, "Hello there.");
        assert_eq!(polish("hello there", &config).text, "Hello there.");
        assert_eq!(
            polish("how can you help me", &config).text,
            "How can you help me."
        );
        assert_eq!(polish("can you send it", &config).text, "Can you send it.");
        assert_eq!(polish("send it now", &config).text, "Send it now.");
        assert_eq!(
            polish("\"quoted words.\"", &config).text,
            "\"Quoted words.\""
        );
        assert_eq!(
            polish("\"quoted words\"", &config).text,
            "\"Quoted words.\""
        );
        assert_eq!(polish("already excited!", &config).text, "Already excited!");
        assert_eq!(polish("is this ready?", &config).text, "Is this ready?");
        assert_eq!(polish("keep this comma,", &config).text, "Keep this comma,");
        assert_eq!(
            polish("unfinished thought...", &config).text,
            "Unfinished thought..."
        );
        assert_eq!(polish("version 3.14", &config).text, "Version 3.14.");
        assert_eq!(
            polish("first sentence. second sentence", &config).text,
            "First sentence. Second sentence."
        );
        assert_eq!(polish("...", &config).text, "...");
    }

    #[test]
    fn dotted_tokens_survive_polish() {
        // Reported bug: prose split and capitalized a domain ("Google. Com.").
        assert_eq!(run("Google.com"), "Google.com");
        assert_eq!(normalize("google.com"), "google.com");
        let mut config = defaults();
        // File names and dotted identifiers stay literal in prose.
        assert_eq!(polish("foo.rs", &config).text, "foo.rs");
        assert_eq!(polish("os.path.join", &config).text, "os.path.join");
        // Inside a sentence: dot kept, next word not capitalized, period added.
        assert_eq!(
            polish("check out google.com", &config).text,
            "Check out google.com."
        );
        // Numbers still behave; a bare number is left literal.
        assert_eq!(polish("version 3.14", &config).text, "Version 3.14.");
        // Terminal style leaves the domain intact too.
        config.style = StyleProfile::Terminal;
        assert_eq!(polish("Google.com", &config).text, "Google.com");
    }

    #[test]
    fn disabled_stages_leave_normalized_text() {
        let config = PolishConfig {
            resolve_corrections: false,
            remove_fillers: false,
            fillers: Vec::new(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
            style: StyleProfile::Default,
            app_styles: Vec::new(),
            file_references: FileReferenceConfig::default(),
        };
        assert_eq!(
            polish("um, Tuesday , actually Wednesday", &config).text,
            "um, Tuesday, actually Wednesday"
        );
    }

    #[test]
    fn trace_records_only_changing_stages_in_order() {
        let outcome = polish("Um, Tuesday, actually Wednesday.", &defaults());
        let stages: Vec<&str> = outcome.trace.iter().map(|t| t.stage).collect();
        assert_eq!(stages, vec!["corrections", "fillers"]);
        assert_eq!(outcome.text, "Wednesday.");
        let unchanged = polish("Hello world.", &defaults());
        assert!(unchanged.trace.is_empty());
        assert_eq!(unchanged.text, "Hello world.");
    }

    #[test]
    fn pipeline_is_idempotent() {
        let mut config = defaults();
        config.dictionary.push(DictionaryEntry {
            spoken: "git hub".into(),
            written: "GitHub".into(),
        });
        config.snippets.push(Snippet {
            trigger: "sign off".into(),
            expansion: "Best regards,\nAntash".into(),
        });
        // Snippet triggers are excluded: a multi-line expansion cannot be
        // idempotent under whitespace normalization, and expanded text is
        // never fed back through the pipeline in the product.
        let inputs = [
            "Um, Tuesday, actually Wednesday.",
            "Send it to Bob. Scratch that, send it to Alice.",
            "I pushed to git hub.",
            "hello   world , again .",
            "page 12, actually 14.",
            "",
            "...",
        ];
        for input in inputs {
            let once = polish(input, &config).text;
            let twice = polish(&once, &config).text;
            assert_eq!(once, twice, "polish must be idempotent for {input:?}");
        }
    }

    #[test]
    fn never_panics_on_strange_inputs() {
        let inputs = [
            "",
            " ",
            ",",
            ", , ,",
            "...!?",
            "é à ü ß",
            "日本語のテキスト、actually 中文。",
            "🦀, actually 🚀.",
            "um",
            "Um.",
            "actually",
            ", actually",
            "scratch that",
            "Scratch that.",
            "a, I mean",
            "'''",
            "---",
            "\u{0000}\u{0007}",
        ];
        let config = defaults();
        for input in inputs {
            let _ = polish(input, &config);
        }
        let long = "word ".repeat(2000) + ", actually replacement.";
        let _ = polish(&long, &config);
    }

    #[test]
    fn style_rules_match_window_classes() {
        let rules = default_app_styles();
        // Real-world WM_CLASS pairs: (instance, class).
        assert_eq!(
            resolve_style(
                Some(("gnome-terminal-server", "Gnome-terminal")),
                &rules,
                StyleProfile::Default
            ),
            StyleProfile::Terminal
        );
        assert_eq!(
            resolve_style(Some(("xterm", "XTerm")), &rules, StyleProfile::Default),
            StyleProfile::Terminal
        );
        assert_eq!(
            resolve_style(
                Some(("Navigator", "firefox")),
                &rules,
                StyleProfile::Default
            ),
            StyleProfile::Default
        );
        // No readable class falls back to the base style.
        assert_eq!(
            resolve_style(None, &rules, StyleProfile::Prose),
            StyleProfile::Prose
        );
    }

    #[test]
    fn style_rules_first_match_wins_and_empty_needles_are_skipped() {
        let rules = vec![
            StyleRule {
                class_contains: "  ".into(),
                style: StyleProfile::Prose,
            },
            StyleRule {
                class_contains: "code".into(),
                style: StyleProfile::Prose,
            },
            StyleRule {
                class_contains: "co".into(),
                style: StyleProfile::Terminal,
            },
        ];
        assert_eq!(
            resolve_style(Some(("code", "Code")), &rules, StyleProfile::Default),
            StyleProfile::Prose
        );
    }

    #[test]
    fn serde_round_trip_for_config() {
        let mut config = defaults();
        config.dictionary.push(DictionaryEntry {
            spoken: "git hub".into(),
            written: "GitHub".into(),
        });
        config.style = StyleProfile::Terminal;
        let encoded = serde_json::to_string(&config).unwrap();
        let decoded: PolishConfig = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
        // Missing fields fall back to defaults so old settings files keep working.
        let partial: PolishConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(partial, defaults());
    }
}

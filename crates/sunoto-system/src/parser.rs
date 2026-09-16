use crate::{RouteOutcome, RouteRejection, SystemIntent, TargetHint};

/// Route common System commands without inference.
///
/// Patterns are ordered from most specific to most general. Each matcher
/// extracts user text into a typed, unresolved intent; it never constructs a
/// path or executable command. A clean no-match explicitly requests the LLM
/// router, while unsafe/multi-action input is rejected before that fallback.
pub fn route_deterministically(transcript: &str) -> RouteOutcome {
    let normalized = normalize(transcript);
    if normalized.is_empty() {
        return rejected(normalized, RouteRejection::Empty);
    }
    if has_control_characters(transcript) {
        return rejected(normalized, RouteRejection::ControlCharacters);
    }
    if let Some((browser_query, spoken_url)) = compound_browser_navigation(&normalized) {
        return matched(
            normalized,
            SystemIntent::OpenBrowserAndUrl {
                browser_query,
                spoken_url,
            },
        );
    }
    if is_multiple_action_request(&normalized) {
        return rejected(normalized, RouteRejection::MultipleActions);
    }

    let lower = normalized.to_ascii_lowercase();

    if let Some(query) = first_slot(
        &normalized,
        &lower,
        &["search the web for ", "search web for ", "google "],
    ) {
        return matched(normalized, SystemIntent::WebSearch { query });
    }
    if let Some(query) = suffix_slot(&normalized, &lower, "look up ", " online") {
        return matched(normalized, SystemIntent::WebSearch { query });
    }

    if let Some(query) = first_slot(
        &normalized,
        &lower,
        &["open website ", "open url ", "go to website ", "go to "],
    ) {
        return matched(normalized, SystemIntent::OpenUrl { spoken_url: query });
    }

    if let Some(query) = compound_file_open(&normalized, &lower) {
        return matched(normalized, SystemIntent::OpenFileByQuery { query });
    }

    if let Some(query) = reveal_file(&normalized, &lower) {
        return matched(normalized, SystemIntent::RevealFileByQuery { query });
    }

    if let Some(query) = first_slot(&normalized, &lower, &["open the file ", "open file "]) {
        return matched(normalized, SystemIntent::OpenFileByQuery { query });
    }
    if let Some(query) = first_slot(
        &normalized,
        &lower,
        &["find the file ", "find file ", "locate file "],
    ) {
        return matched(normalized, SystemIntent::FindFile { query });
    }

    if let Some(query) = first_slot(
        &normalized,
        &lower,
        &["open the folder ", "open folder ", "show folder "],
    ) {
        return matched(normalized, SystemIntent::OpenFolder { query });
    }

    // Keep this ahead of the generic `open` route. Both slots remain
    // unresolved data; the provider must resolve the target and editor and
    // the user must explicitly select the combined palette row.
    if let Some((query, application_query)) = open_target_with_application(&normalized, &lower) {
        return matched(
            normalized,
            SystemIntent::OpenTargetWithApplication {
                query,
                application_query,
            },
        );
    }

    if let Some(query) = first_slot(
        &normalized,
        &lower,
        &[
            "launch application ",
            "launch app ",
            "launch ",
            "start application ",
            "start app ",
            "start ",
        ],
    ) {
        return matched(
            normalized,
            SystemIntent::OpenTarget {
                query,
                hint: TargetHint::Application,
            },
        );
    }
    if let Some(query) = first_slot(&normalized, &lower, &["open application ", "open app "]) {
        return matched(
            normalized,
            SystemIntent::OpenTarget {
                query,
                hint: TargetHint::Application,
            },
        );
    }

    if let Some(query) = first_slot(&normalized, &lower, &["find ", "search for ", "locate "]) {
        return matched(normalized, SystemIntent::FindFile { query });
    }

    if let Some(query) = first_slot(&normalized, &lower, &["open "]) {
        let hint = if looks_like_spoken_url(&query) {
            TargetHint::Url
        } else {
            TargetHint::Auto
        };
        return matched(normalized, SystemIntent::OpenTarget { query, hint });
    }

    RouteOutcome::NeedsLlm {
        normalized_text: normalized,
    }
}

fn open_target_with_application(original: &str, lower: &str) -> Option<(String, String)> {
    let rest = lower.strip_prefix("open ")?;
    let marker = " in ";
    let index = rest.rfind(marker)?;
    let query = nonempty_slot(original, 5, index)?;
    let application_start = 5 + index + marker.len();
    let application_query = original.get(application_start..)?.trim();
    if application_query.is_empty() || looks_like_spoken_url(&query) {
        return None;
    }
    Some((query, application_query.into()))
}

fn compound_browser_navigation(input: &str) -> Option<(String, String)> {
    let lower = input.to_ascii_lowercase();
    let rest = lower.strip_prefix("open ")?;
    let marker = " and go to ";
    let index = rest.find(marker)?;
    let browser = input.get(5..5 + index)?.trim();
    let url = input.get(5 + index + marker.len()..)?.trim();
    (!browser.is_empty() && looks_like_spoken_url(url)).then(|| (browser.into(), url.into()))
}

fn compound_file_open(original: &str, lower: &str) -> Option<String> {
    for prefix in ["find ", "search for ", "locate "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        for suffix in [" and open it", " then open it"] {
            if let Some(query) = rest.strip_suffix(suffix) {
                return nonempty_slot(original, prefix.len(), query.len());
            }
        }
    }
    None
}

fn reveal_file(original: &str, lower: &str) -> Option<String> {
    if let Some(query) = first_slot(original, lower, &["reveal "]) {
        return Some(query);
    }
    for prefix in ["show ", "reveal "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        for suffix in [" in finder", " in files", " in file manager"] {
            if let Some(query) = rest.strip_suffix(suffix) {
                return nonempty_slot(original, prefix.len(), query.len());
            }
        }
    }
    None
}

fn first_slot(original: &str, lower: &str, prefixes: &[&str]) -> Option<String> {
    for prefix in prefixes {
        if let Some(rest) = lower.strip_prefix(prefix) {
            return nonempty_slot(original, prefix.len(), rest.len());
        }
    }
    None
}

fn suffix_slot(original: &str, lower: &str, prefix: &str, suffix: &str) -> Option<String> {
    let rest = lower.strip_prefix(prefix)?;
    let query = rest.strip_suffix(suffix)?;
    nonempty_slot(original, prefix.len(), query.len())
}

fn nonempty_slot(original: &str, start: usize, byte_len: usize) -> Option<String> {
    let slot = original.get(start..start + byte_len)?.trim();
    (!slot.is_empty()).then(|| slot.to_string())
}

fn normalize(input: &str) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|ch: char| matches!(ch, '.' | '?' | '!' | ',' | ' '))
        .to_string()
}

fn looks_like_spoken_url(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains(" dot ")
        || lower.contains('.')
        || lower.starts_with("http://")
        || lower.starts_with("https://")
}

fn has_control_characters(input: &str) -> bool {
    // System transcripts represent one spoken request. Newlines and tabs are
    // therefore not useful formatting; accepting them would only create a
    // second, hidden command channel for callers outside ASR.
    input.chars().any(char::is_control)
}

fn is_multiple_action_request(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.contains(';')
        || lower.contains("&&")
        || lower.contains('|')
        || lower.contains(" and then ")
        || lower.contains(" after that ")
}

fn matched(normalized_text: String, intent: SystemIntent) -> RouteOutcome {
    RouteOutcome::Matched {
        normalized_text,
        intent,
    }
}

fn rejected(normalized_text: String, reason: RouteRejection) -> RouteOutcome {
    RouteOutcome::Rejected {
        normalized_text,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(text: &str) -> SystemIntent {
        match route_deterministically(text) {
            RouteOutcome::Matched { intent, .. } => intent,
            other => panic!("expected match for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn routes_every_version_one_intent_family() {
        assert_eq!(
            intent("Open Chrome"),
            SystemIntent::OpenTarget {
                query: "Chrome".into(),
                hint: TargetHint::Auto,
            }
        );
        assert_eq!(
            intent("Open Chrome and go to google dot com"),
            SystemIntent::OpenBrowserAndUrl {
                browser_query: "Chrome".into(),
                spoken_url: "google dot com".into(),
            }
        );
        assert_eq!(
            intent("Launch Visual Studio Code"),
            SystemIntent::OpenTarget {
                query: "Visual Studio Code".into(),
                hint: TargetHint::Application,
            }
        );
        assert_eq!(
            intent("Find the file quarterly report"),
            SystemIntent::FindFile {
                query: "quarterly report".into(),
            }
        );
        assert_eq!(
            intent("Search for quarterly report and open it"),
            SystemIntent::OpenFileByQuery {
                query: "quarterly report".into(),
            }
        );
        assert_eq!(
            intent("Show quarterly report in Finder"),
            SystemIntent::RevealFileByQuery {
                query: "quarterly report".into(),
            }
        );
        assert_eq!(
            intent("Open folder Downloads"),
            SystemIntent::OpenFolder {
                query: "Downloads".into(),
            }
        );
        assert_eq!(
            intent("Open who-else-is-free in VS Code"),
            SystemIntent::OpenTargetWithApplication {
                query: "who-else-is-free".into(),
                application_query: "VS Code".into(),
            }
        );
        assert_eq!(
            intent("Search the web for Rust async traits"),
            SystemIntent::WebSearch {
                query: "Rust async traits".into(),
            }
        );
        assert_eq!(
            intent("Open website example dot com"),
            SystemIntent::OpenUrl {
                spoken_url: "example dot com".into(),
            }
        );
    }

    #[test]
    fn preserves_target_spelling_while_matching_case_insensitively() {
        assert_eq!(
            intent("OPEN iTerm2"),
            SystemIntent::OpenTarget {
                query: "iTerm2".into(),
                hint: TargetHint::Auto,
            }
        );
    }

    #[test]
    fn clean_no_match_requests_the_llm_router() {
        assert!(matches!(
            route_deterministically("Could you bring up my usual browser"),
            RouteOutcome::NeedsLlm { .. }
        ));
    }

    #[test]
    fn unsafe_or_multi_action_input_fails_before_llm() {
        assert!(matches!(
            route_deterministically("open Chrome; remove everything"),
            RouteOutcome::Rejected {
                reason: RouteRejection::MultipleActions,
                ..
            }
        ));
        assert!(matches!(
            route_deterministically("open Chrome and then open Terminal"),
            RouteOutcome::Rejected {
                reason: RouteRejection::MultipleActions,
                ..
            }
        ));
        assert_eq!(
            route_deterministically("open Chrome\nopen Terminal"),
            RouteOutcome::Rejected {
                normalized_text: "open Chrome open Terminal".into(),
                reason: RouteRejection::ControlCharacters,
            }
        );
    }

    #[test]
    fn empty_slots_do_not_match() {
        assert!(matches!(
            route_deterministically("open"),
            RouteOutcome::NeedsLlm { .. }
        ));
        assert!(matches!(
            route_deterministically("   "),
            RouteOutcome::Rejected {
                reason: RouteRejection::Empty,
                ..
            }
        ));
    }
}

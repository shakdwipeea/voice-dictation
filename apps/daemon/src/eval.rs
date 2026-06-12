use std::error::Error;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sunoto_polish::{DictionaryEntry, PolishConfig, Snippet, polish};

use crate::logging;
use crate::settings::repo_root;

pub struct EvalArgs {
    pub corpus: PathBuf,
    pub output: Option<PathBuf>,
}

impl Default for EvalArgs {
    fn default() -> Self {
        Self {
            corpus: repo_root().join("tests/corpus/phase2-text-cases.json"),
            output: None,
        }
    }
}

/// An evaluation corpus: raw ASR transcripts paired with the gold final text
/// a user would accept without editing. `raw` entries are either scripted
/// (text-only cases) or produced by running recorded audio through the ASR
/// sidecar; the manifest format is the same for both.
#[derive(Debug, Deserialize)]
struct Corpus {
    #[serde(default)]
    description: String,
    /// Whether `raw` came from real recordings ("recorded") or was written by
    /// hand ("scripted"). The Phase 2 exit gate requires a recorded corpus.
    #[serde(default = "default_kind")]
    kind: String,
    /// Dictionary entries the cases rely on (names, code terms); merged into
    /// the pipeline configuration for the run.
    #[serde(default)]
    dictionary: Vec<DictionaryEntry>,
    #[serde(default)]
    snippets: Vec<Snippet>,
    cases: Vec<Case>,
}

fn default_kind() -> String {
    "scripted".to_string()
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    /// Transcript as the ASR backend would emit it.
    raw: String,
    /// Gold final text: what the user would accept with zero edits.
    expected: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CaseResult {
    id: String,
    tags: Vec<String>,
    raw: String,
    expected: String,
    polished: String,
    raw_matches: bool,
    polished_matches: bool,
    /// Digit sequences agree between polished and expected text. A mismatch
    /// means the pipeline changed numbers — the worst kind of edit, and an
    /// exit-gate violation even when the rest of the text is acceptable.
    digits_preserved: bool,
}

#[derive(Debug, Serialize)]
struct TagSummary {
    tag: String,
    cases: usize,
    polished_zero_edit: usize,
}

#[derive(Debug, Serialize)]
struct EvalSummary {
    corpus_kind: String,
    cases: usize,
    raw_zero_edit: usize,
    polished_zero_edit: usize,
    raw_zero_edit_rate: f64,
    polished_zero_edit_rate: f64,
    /// Cases the pipeline broke: raw already matched, polished does not.
    regressions: Vec<String>,
    /// Cases where polished output changed digit content versus the gold text.
    digit_violations: Vec<String>,
    by_tag: Vec<TagSummary>,
}

fn evaluate(corpus: &Corpus, config: &PolishConfig) -> Vec<CaseResult> {
    corpus
        .cases
        .iter()
        .map(|case| {
            let polished = polish(&case.raw, config).text;
            CaseResult {
                id: case.id.clone(),
                tags: case.tags.clone(),
                raw: case.raw.clone(),
                expected: case.expected.clone(),
                raw_matches: case.raw == case.expected,
                polished_matches: polished == case.expected,
                digits_preserved: digit_content(&polished) == digit_content(&case.expected),
                polished,
            }
        })
        .collect()
}

fn digit_content(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

fn summarize(corpus_kind: &str, results: &[CaseResult]) -> EvalSummary {
    let cases = results.len();
    let raw_zero_edit = results.iter().filter(|r| r.raw_matches).count();
    let polished_zero_edit = results.iter().filter(|r| r.polished_matches).count();
    let rate = |count: usize| {
        if cases == 0 {
            0.0
        } else {
            count as f64 / cases as f64
        }
    };
    let mut by_tag: Vec<TagSummary> = Vec::new();
    for result in results {
        for tag in &result.tags {
            let entry = match by_tag.iter_mut().find(|summary| &summary.tag == tag) {
                Some(entry) => entry,
                None => {
                    by_tag.push(TagSummary {
                        tag: tag.clone(),
                        cases: 0,
                        polished_zero_edit: 0,
                    });
                    by_tag.last_mut().expect("just pushed")
                }
            };
            entry.cases += 1;
            entry.polished_zero_edit += usize::from(result.polished_matches);
        }
    }
    EvalSummary {
        corpus_kind: corpus_kind.to_string(),
        cases,
        raw_zero_edit,
        polished_zero_edit,
        raw_zero_edit_rate: rate(raw_zero_edit),
        polished_zero_edit_rate: rate(polished_zero_edit),
        regressions: results
            .iter()
            .filter(|r| r.raw_matches && !r.polished_matches)
            .map(|r| r.id.clone())
            .collect(),
        digit_violations: results
            .iter()
            .filter(|r| !r.digits_preserved)
            .map(|r| r.id.clone())
            .collect(),
        by_tag,
    }
}

/// Measure the zero-edit rate of the deterministic pipeline over a corpus.
/// Runs with the default pipeline configuration plus the corpus's own
/// dictionary and snippets, so results are reproducible across machines and
/// independent of the local settings file.
pub fn run(args: EvalArgs) -> Result<(), Box<dyn Error>> {
    let contents = std::fs::read_to_string(&args.corpus)
        .map_err(|error| format!("cannot read {}: {error}", args.corpus.display()))?;
    let corpus: Corpus = serde_json::from_str(&contents)
        .map_err(|error| format!("invalid corpus {}: {error}", args.corpus.display()))?;
    let mut config = PolishConfig::default();
    config.dictionary.extend(corpus.dictionary.iter().cloned());
    config.snippets.extend(corpus.snippets.iter().cloned());

    logging::info(&format!(
        "eval: {} ({} cases, kind={}) {}",
        args.corpus.display(),
        corpus.cases.len(),
        corpus.kind,
        corpus.description
    ));

    let results = evaluate(&corpus, &config);
    let summary = summarize(&corpus.kind, &results);

    for result in &results {
        if !result.polished_matches {
            logging::warn(&format!(
                "case {}: raw      {:?}\n         expected {:?}\n         polished {:?}",
                result.id, result.raw, result.expected, result.polished
            ));
        }
    }
    logging::info(&format!(
        "zero-edit rate: raw {}/{} ({:.0}%), polished {}/{} ({:.0}%)",
        summary.raw_zero_edit,
        summary.cases,
        summary.raw_zero_edit_rate * 100.0,
        summary.polished_zero_edit,
        summary.cases,
        summary.polished_zero_edit_rate * 100.0
    ));

    let report = serde_json::json!({
        "corpus": args.corpus.display().to_string(),
        "summary": summary,
        "results": results,
    });
    let rendered = serde_json::to_string_pretty(&report)?;
    println!("{rendered}");
    if let Some(output) = &args.output {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output, rendered + "\n")?;
        logging::info(&format!("eval report written to {}", output.display()));
    }

    // The pipeline must help, never hurt: breaking already-correct input or
    // changing digits is a failure even while the overall rate is still
    // being tuned upward.
    if !summary.regressions.is_empty() {
        return Err(format!("pipeline regressed cases: {:?}", summary.regressions).into());
    }
    if !summary.digit_violations.is_empty() {
        return Err(format!("pipeline changed digits in: {:?}", summary.digit_violations).into());
    }
    if summary.polished_zero_edit < summary.raw_zero_edit {
        return Err("polished zero-edit rate fell below the raw rate".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(cases: Vec<Case>) -> Corpus {
        Corpus {
            description: String::new(),
            kind: "scripted".into(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
            cases,
        }
    }

    fn case(id: &str, raw: &str, expected: &str) -> Case {
        Case {
            id: id.into(),
            raw: raw.into(),
            expected: expected.into(),
            tags: vec!["t".into()],
        }
    }

    #[test]
    fn evaluation_counts_matches_and_improvements() {
        let corpus = corpus(vec![
            case("filler", "Um, send it.", "Send it."),
            case("clean", "Already fine.", "Already fine."),
        ]);
        let results = evaluate(&corpus, &PolishConfig::default());
        let summary = summarize(&corpus.kind, &results);
        assert_eq!(summary.cases, 2);
        assert_eq!(summary.raw_zero_edit, 1);
        assert_eq!(summary.polished_zero_edit, 2);
        assert!(summary.regressions.is_empty());
        assert!(summary.digit_violations.is_empty());
        assert_eq!(summary.by_tag.len(), 1);
        assert_eq!(summary.by_tag[0].cases, 2);
        assert_eq!(summary.by_tag[0].polished_zero_edit, 2);
    }

    #[test]
    fn regressions_and_digit_changes_are_detected() {
        // A filler word configured to match a real word breaks correct input.
        let mut config = PolishConfig::default();
        config.fillers.push("well".into());
        let corpus = corpus(vec![
            case("broken", "The well is deep.", "The well is deep."),
            case(
                "digits",
                "Pay 12 dollars, actually 14 dollars.",
                "Pay 12 dollars.",
            ),
        ]);
        let results = evaluate(&corpus, &config);
        let summary = summarize(&corpus.kind, &results);
        assert_eq!(summary.regressions, vec!["broken".to_string()]);
        // The swap rewrites "12 dollars" to "14 dollars": digits diverge from
        // the (deliberately contradictory) gold text and must be flagged.
        assert_eq!(summary.digit_violations, vec!["digits".to_string()]);
    }

    #[test]
    fn corpus_manifest_parses_with_defaults() {
        let parsed: Corpus = serde_json::from_str(
            r#"{
                "cases": [
                    {"id": "a", "raw": "x", "expected": "x"}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.kind, "scripted");
        assert!(parsed.dictionary.is_empty());
        assert!(parsed.cases[0].tags.is_empty());
    }
}

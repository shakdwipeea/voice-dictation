//! Product-level routing examples live in data rather than being buried in
//! parser implementation tests. Adding a supported phrase or regression case
//! should normally start by extending `fixtures/routing.json`.

use serde::Deserialize;
use sunoto_system::{RouteOutcome, RouteRejection, route_deterministically};

#[derive(Debug, Deserialize)]
struct Corpus {
    matched: Vec<MatchedCase>,
    needs_llm: Vec<String>,
    rejected: Vec<RejectedCase>,
}

#[derive(Debug, Deserialize)]
struct MatchedCase {
    transcript: String,
    intent_type: String,
    query: String,
}

#[derive(Debug, Deserialize)]
struct RejectedCase {
    transcript: String,
    reason: RouteRejection,
}

fn corpus() -> Corpus {
    serde_json::from_str(include_str!("fixtures/routing.json")).expect("routing corpus is valid")
}

#[test]
fn supported_phrases_produce_the_expected_typed_intent() {
    for case in corpus().matched {
        let RouteOutcome::Matched { intent, .. } = route_deterministically(&case.transcript) else {
            panic!("expected deterministic match for {:?}", case.transcript);
        };
        let serialized = serde_json::to_value(&intent).expect("intent serializes");
        assert_eq!(
            serialized["type"], case.intent_type,
            "{:?}",
            case.transcript
        );
        assert_eq!(intent.query(), case.query, "{:?}", case.transcript);
    }
}

#[test]
fn natural_no_matches_are_the_only_inputs_offered_to_the_llm() {
    for transcript in corpus().needs_llm {
        assert!(
            matches!(
                route_deterministically(&transcript),
                RouteOutcome::NeedsLlm { .. }
            ),
            "{transcript:?}"
        );
    }
}

#[test]
fn unsafe_inputs_fail_closed_before_the_llm() {
    for case in corpus().rejected {
        assert!(
            matches!(
                route_deterministically(&case.transcript),
                RouteOutcome::Rejected { reason, .. } if reason == case.reason
            ),
            "{:?}",
            case.transcript
        );
    }
}

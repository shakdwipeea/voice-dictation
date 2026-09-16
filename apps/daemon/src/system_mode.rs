//! Read-only planning and native-resolution entry points for System mode.
//!
//! `plan` makes the transcript-to-intent boundary observable. `resolve` also
//! inspects installed applications but deliberately drops the executable
//! action map after producing UI-safe suggestion tokens.

use std::error::Error;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use serde::Serialize;
use sunoto_desktop::SystemPlatform;
use sunoto_system::{
    CapabilityDispatcher, CapabilityInput, NativeCapabilityCall, NativeCapabilityDispatcher,
    PendingSuggestionSet, PolicyDecision, RiskClass, RouteOutcome, SystemIntent, SystemSuggestion,
    TargetHint, ValidatedHttpUrl, policy_for_intent, route_deterministically,
};

#[derive(Debug, Serialize)]
struct PlanPolicy {
    risk: RiskClass,
    decision: PolicyDecision,
}

/// Stable JSON response shared by the local CLI and daemon control protocol.
///
/// `execution_allowed` is deliberately always false. The local `resolve`
/// command can fill UI-safe suggestions, but it drops their daemon-only action
/// map before returning, so its tokens can never authorize execution.
#[derive(Debug, Serialize)]
struct SystemPlan {
    schema_version: u8,
    #[serde(rename = "type")]
    message_type: &'static str,
    route: RouteOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<PlanPolicy>,
    next_step: &'static str,
    suggestions: Vec<SystemSuggestion>,
    steps: Vec<DryRunStep>,
    observations: Vec<sunoto_system::TypedObservation>,
    execution_allowed: bool,
}

#[derive(Debug, Serialize)]
struct DryRunStep {
    id: &'static str,
    capability: &'static str,
    arguments: CapabilityInput,
    depends_on: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_from_step: Option<&'static str>,
    policy: PolicyDecision,
}

/// Small presentation contract handed from routing to the live resolver.
pub struct LivePlanPresentation {
    pub message: String,
    pub intent: Option<SystemIntent>,
}

pub fn run_cli(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some((subcommand, transcript)) = args.split_first() else {
        return Err(system_usage().into());
    };
    let payload = match subcommand.as_str() {
        "plan" => {
            let (through_daemon, transcript) = match transcript.split_first() {
                Some((flag, rest)) if flag == "--daemon" => (true, rest),
                _ => (false, transcript),
            };
            if transcript.is_empty() {
                return Err(system_usage().into());
            }
            if through_daemon {
                plan_via_daemon(&transcript.join(" "))?
            } else {
                plan_json(&transcript.join(" "))?
            }
        }
        "resolve" if !transcript.is_empty() => resolve_json(&transcript.join(" "))?,
        _ => return Err(system_usage().into()),
    };
    println!("{payload}");
    Ok(())
}

fn system_usage() -> &'static str {
    "usage: sunoto-daemon system plan [--daemon] TEXT | system resolve TEXT"
}

fn resolve_json(transcript: &str) -> Result<String, Box<dyn Error>> {
    let mut planned = plan(transcript, true);
    let intent = match &planned.route {
        RouteOutcome::Matched { intent, .. } if is_target_find_intent(intent) => {
            Some(intent.clone())
        }
        _ => None,
    };

    if let Some(intent) = intent {
        let mut dispatcher = NativeCapabilityDispatcher::new(SystemPlatform::default());
        let input = if matches!(
            intent,
            SystemIntent::OpenTarget {
                hint: TargetHint::Application,
                ..
            }
        ) {
            CapabilityInput::Native(NativeCapabilityCall::FindApplication {
                query: intent.query().into(),
            })
        } else {
            CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                intent: intent.clone(),
            })
        };
        let observation = dispatcher.dispatch(&input);
        let candidates = dispatcher.take_last_candidates();
        planned.observations.push(observation);
        let pending = PendingSuggestionSet::build(0, &intent, candidates, 5);
        let mut value = serde_json::to_value(&planned)?;
        value["suggestions"] = serde_json::to_value(pending.suggestions())?;
        value["next_step"] = serde_json::Value::String(
            if pending.suggestions().is_empty() {
                "none"
            } else {
                "await_selection"
            }
            .into(),
        );
        // This command deliberately drops `pending` here. Its tokens cannot
        // be selected later because the executable action map no longer exists.
        return Ok(serde_json::to_string_pretty(&value)?);
    }

    planned.next_step = "none";
    Ok(serde_json::to_string_pretty(&planned)?)
}

fn plan_via_daemon(transcript: &str) -> Result<String, Box<dyn Error>> {
    let path = crate::settings::control_socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("cannot connect to {}: {error}", path.display()))?;
    serde_json::to_writer(
        &mut stream,
        &serde_json::json!({
            "type": "plan_system",
            "text": transcript,
            "dry_run": true,
        }),
    )?;
    stream.write_all(b"\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response.trim_end().to_string())
}

pub fn plan_json(transcript: &str) -> Result<String, serde_json::Error> {
    plan_json_with_llm_fallback(transcript, true)
}

pub fn plan_json_with_llm_fallback(
    transcript: &str,
    llm_fallback_enabled: bool,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&plan(transcript, llm_fallback_enabled))
}

fn plan(transcript: &str, llm_fallback_enabled: bool) -> SystemPlan {
    let route = route_deterministically(transcript);
    let (policy, next_step) = match &route {
        RouteOutcome::Matched { intent, .. } => {
            let (risk, decision) = policy_for_intent(intent);
            (Some(PlanPolicy { risk, decision }), "resolve_candidates")
        }
        RouteOutcome::NeedsLlm { .. } if llm_fallback_enabled => (None, "llm_router"),
        RouteOutcome::NeedsLlm { .. } => (None, "none"),
        RouteOutcome::Rejected { .. } => (None, "none"),
    };

    let steps = match &route {
        RouteOutcome::Matched {
            intent:
                SystemIntent::OpenTarget {
                    query,
                    hint: TargetHint::Application,
                },
            ..
        } => vec![DryRunStep {
            id: "find-application",
            capability: "application.find",
            arguments: CapabilityInput::Native(NativeCapabilityCall::FindApplication {
                query: query.clone(),
            }),
            depends_on: vec![],
            browser_from_step: None,
            policy: PolicyDecision::PresentSuggestions,
        }],
        RouteOutcome::Matched { intent, .. } if is_target_find_intent(intent) => {
            vec![DryRunStep {
                id: "find-target",
                capability: "target.find",
                arguments: CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                    intent: intent.clone(),
                }),
                depends_on: vec![],
                browser_from_step: None,
                policy: PolicyDecision::PresentSuggestions,
            }]
        }
        RouteOutcome::Matched {
            intent: SystemIntent::OpenUrl { spoken_url },
            ..
        } => match ValidatedHttpUrl::parse_spoken(spoken_url) {
            Ok(url) => vec![DryRunStep {
                id: "open-url",
                capability: "url.open",
                arguments: CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
                    url,
                    browser: None,
                }),
                depends_on: vec![],
                browser_from_step: None,
                policy: PolicyDecision::PresentSuggestions,
            }],
            Err(_) => Vec::new(),
        },
        RouteOutcome::Matched {
            intent:
                SystemIntent::OpenTarget {
                    query: spoken_url,
                    hint: TargetHint::Url,
                },
            ..
        } => match ValidatedHttpUrl::parse_spoken(spoken_url) {
            Ok(url) => vec![DryRunStep {
                id: "open-url",
                capability: "url.open",
                arguments: CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
                    url,
                    browser: None,
                }),
                depends_on: vec![],
                browser_from_step: None,
                policy: PolicyDecision::PresentSuggestions,
            }],
            Err(_) => Vec::new(),
        },
        RouteOutcome::Matched {
            intent: SystemIntent::WebSearch { query },
            ..
        } => vec![DryRunStep {
            id: "web-search",
            capability: "web.search",
            arguments: CapabilityInput::Native(NativeCapabilityCall::WebSearch {
                query: query.clone(),
                browser: None,
            }),
            depends_on: vec![],
            browser_from_step: None,
            policy: PolicyDecision::PresentSuggestions,
        }],
        RouteOutcome::Matched {
            intent:
                SystemIntent::OpenBrowserAndUrl {
                    browser_query,
                    spoken_url,
                },
            ..
        } => match ValidatedHttpUrl::parse_spoken(spoken_url) {
            Ok(url) => vec![
                DryRunStep {
                    id: "find-browser",
                    capability: "application.find",
                    arguments: CapabilityInput::Native(NativeCapabilityCall::FindApplication {
                        query: browser_query.clone(),
                    }),
                    depends_on: vec![],
                    browser_from_step: None,
                    policy: PolicyDecision::PresentSuggestions,
                },
                DryRunStep {
                    id: "open-url-in-browser",
                    capability: "url.open",
                    arguments: CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
                        url,
                        browser: None,
                    }),
                    depends_on: vec!["find-browser"],
                    browser_from_step: Some("find-browser"),
                    policy: PolicyDecision::PresentSuggestions,
                },
            ],
            Err(_) => Vec::new(),
        },
        _ => Vec::new(),
    };
    SystemPlan {
        schema_version: 1,
        message_type: "system_plan",
        route,
        policy,
        next_step,
        suggestions: Vec::new(),
        steps,
        observations: Vec::new(),
        execution_allowed: false,
    }
}

pub fn present_live_plan(transcript: &str, llm_fallback_enabled: bool) -> LivePlanPresentation {
    let plan = plan(transcript, llm_fallback_enabled);
    let (message, intent) = match &plan.route {
        RouteOutcome::Matched { intent, .. } => (
            format!("System matched: {}", intent_label(intent)),
            Some(intent.clone()),
        ),
        RouteOutcome::NeedsLlm { .. } if llm_fallback_enabled => {
            ("System LLM router is not connected yet".into(), None)
        }
        RouteOutcome::NeedsLlm { .. } => ("Unsupported System request".into(), None),
        RouteOutcome::Rejected { .. } => ("System request rejected".into(), None),
    };
    LivePlanPresentation { message, intent }
}

fn intent_label(intent: &SystemIntent) -> String {
    match intent {
        SystemIntent::OpenTarget { query, .. } => format!("Open {query}"),
        SystemIntent::OpenTargetWithApplication {
            query,
            application_query,
        } => format!("Open {query} in {application_query}"),
        SystemIntent::FindFile { query } => format!("Find {query}"),
        SystemIntent::OpenFileByQuery { query } => format!("Open file {query}"),
        SystemIntent::RevealFileByQuery { query } => format!("Reveal {query}"),
        SystemIntent::OpenFolder { query } => format!("Open folder {query}"),
        SystemIntent::WebSearch { query } => format!("Search web for {query}"),
        SystemIntent::OpenUrl { spoken_url } => format!("Open URL {spoken_url}"),
        SystemIntent::OpenBrowserAndUrl {
            browser_query,
            spoken_url,
        } => {
            format!("Open {spoken_url} in {browser_query}")
        }
    }
}

fn is_target_find_intent(intent: &SystemIntent) -> bool {
    matches!(
        intent,
        SystemIntent::OpenTarget {
            hint: TargetHint::Auto | TargetHint::File | TargetHint::Folder,
            ..
        } | SystemIntent::OpenTargetWithApplication { .. }
            | SystemIntent::FindFile { .. }
            | SystemIntent::OpenFileByQuery { .. }
            | SystemIntent::RevealFileByQuery { .. }
            | SystemIntent::OpenFolder { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(transcript: &str) -> serde_json::Value {
        serde_json::from_str(&plan_json(transcript).unwrap()).unwrap()
    }

    #[test]
    fn matched_command_stops_at_candidate_resolution() {
        let response = value("open Chrome");
        assert_eq!(response["route"]["status"], "matched");
        assert_eq!(response["route"]["intent"]["type"], "open_target");
        assert_eq!(response["next_step"], "resolve_candidates");
        assert_eq!(response["policy"]["decision"], "present_suggestions");
        assert_eq!(response["execution_allowed"], false);
        assert_eq!(response["suggestions"], serde_json::json!([]));
        assert_eq!(response["steps"][0]["capability"], "target.find");
        assert_eq!(response["steps"][0]["depends_on"], serde_json::json!([]));
        assert_eq!(response["observations"], serde_json::json!([]));
    }

    #[test]
    fn project_editor_plan_uses_unified_target_find_without_native_ids() {
        let response = value("open who-else-is-free in VS Code");
        assert_eq!(response["steps"][0]["capability"], "target.find");
        assert_eq!(
            response["steps"][0]["arguments"]["intent"]["application_query"],
            "VS Code"
        );
        assert_eq!(response["execution_allowed"], false);
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains("/Applications/"));
        assert!(!serialized.contains("/Users/"));
    }

    #[test]
    fn unknown_open_query_returns_unified_no_match_without_execution() {
        let response: serde_json::Value = serde_json::from_str(
            &resolve_json("open sunoto-definitely-missing-target-9284").unwrap(),
        )
        .unwrap();
        assert_eq!(response["next_step"], "none");
        assert_eq!(response["suggestions"], serde_json::json!([]));
        assert_eq!(response["execution_allowed"], false);
        assert_eq!(response["observations"][0]["capability"], "target_find");
    }

    #[test]
    fn clean_no_match_hands_off_to_llm_without_executing() {
        let response = value("bring up my usual browser");
        assert_eq!(response["route"]["status"], "needs_llm");
        assert_eq!(response["next_step"], "llm_router");
        assert_eq!(response["execution_allowed"], false);
    }

    #[test]
    fn unsafe_input_is_rejected_instead_of_reinterpreted() {
        let response = value("open Chrome; delete everything");
        assert_eq!(response["route"]["status"], "rejected");
        assert_eq!(response["next_step"], "none");
        assert_eq!(response["execution_allowed"], false);
    }

    #[test]
    fn disabled_llm_fallback_leaves_a_clean_no_match_inert() {
        let response: serde_json::Value = serde_json::from_str(
            &plan_json_with_llm_fallback("bring up my usual browser", false).unwrap(),
        )
        .unwrap();
        assert_eq!(response["route"]["status"], "needs_llm");
        assert_eq!(response["next_step"], "none");
        assert_eq!(response["execution_allowed"], false);
    }

    #[test]
    fn web_routes_are_read_only_validated_capability_steps() {
        let url = value("go to example dot com");
        assert_eq!(url["steps"][0]["capability"], "url.open");
        assert_eq!(url["steps"][0]["arguments"]["type"], "open_url");
        assert_eq!(url["steps"][0]["arguments"]["url"], "https://example.com/");
        assert_eq!(url["execution_allowed"], false);

        let search = value("search the web for rust async traits");
        assert_eq!(search["steps"][0]["capability"], "web.search");
        assert_eq!(search["execution_allowed"], false);

        let open = value("open example dot com");
        assert_eq!(open["steps"][0]["capability"], "url.open");

        let compound = value("open Chrome and go to google dot com");
        assert_eq!(compound["steps"].as_array().unwrap().len(), 2);
        assert_eq!(compound["steps"][0]["capability"], "application.find");
        assert_eq!(compound["steps"][1]["capability"], "url.open");
        assert_eq!(compound["steps"][1]["browser_from_step"], "find-browser");
        assert_eq!(compound["execution_allowed"], false);
    }

    #[test]
    fn live_preview_reports_a_match_without_claiming_execution() {
        let presentation = present_live_plan("open Chrome", true);
        assert_eq!(presentation.message, "System matched: Open Chrome");
        assert!(presentation.intent.is_some());
        assert_eq!(value("open Chrome")["execution_allowed"], false);
    }
}

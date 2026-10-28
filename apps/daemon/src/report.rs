//! "This was wrong" reports (docs/accuracy-reports-plan-2026-10-28.md,
//! step 2). The user flags a dictation in the menu bar's Recent Dictations;
//! the overlay's panel lists what a report holds, built here from the
//! dictation's history record. Send saves the report to
//! `accuracy-reports/pending/`, which `report_send` uploads (step 3). A
//! build with nowhere to send says Save and writes `accuracy-reports/kept/`
//! instead. Either way the panel shows a short code the user can quote.
//!
//! A report is built from an allowlist of record fields: the version, the
//! speech engine, timings and the `diag` counts. The dictation's text goes
//! in only when the user ticks Include the text, and then only under
//! `attachments`. Window titles, app names, word-list words, file paths and
//! free-form error strings never do.

use std::collections::hash_map::RandomState;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// What the report says about this Mac, besides the dictation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Environment {
    pub(crate) macos: Option<String>,
    pub(crate) mac_model: Option<String>,
}

impl Environment {
    pub(crate) fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                macos: sunoto_desktop::macos_version(),
                mac_model: sunoto_desktop::mac_model(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }
}

/// `accuracy-reports` next to the history folder; `SUNOTO_REPORTS_DIR` wins.
pub(crate) fn reports_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SUNOTO_REPORTS_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    crate::history::history_dir().with_file_name("accuracy-reports")
}

/// "SUN-7F3A": short enough to read out, random enough not to repeat often
/// (Sentry's own event id tells reports apart).
pub(crate) fn new_code() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(crate::history::now_ms());
    format!("SUN-{:04X}", hasher.finish() as u16)
}

/// Sentry's event id: 32 random hex digits. Saved with the report, so a
/// resend after a lost answer is the same event, not a second one.
pub(crate) fn new_event_id() -> String {
    let mut bytes = [0u8; 16];
    if fs::File::open("/dev/urandom")
        .and_then(|mut file| io::Read::read_exact(&mut file, &mut bytes))
        .is_err()
    {
        for half in bytes.chunks_mut(8) {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(crate::history::now_ms());
            half.copy_from_slice(&hasher.finish().to_le_bytes());
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The panel's list of what the report holds, in plain words.
pub(crate) fn included_lines(record: &Value, env: &Environment) -> Vec<String> {
    let get = |path: &str| record.pointer(path);
    let num = |path: &str| get(path).and_then(Value::as_u64);
    let text = |path: &str| get(path).and_then(Value::as_str);
    let mut lines = Vec::new();

    let mut version = format!("Sunoto {}", text("/app_version").unwrap_or("?"));
    if let Some(macos) = &env.macos {
        version.push_str(&format!(" on macOS {macos}"));
    }
    if let Some(model) = &env.mac_model {
        version.push_str(&format!(", {model}"));
    }
    lines.push(version);

    let mut engine = format!("Speech engine: {}", text("/asr/backend").unwrap_or("?"));
    if let Some(model) = text("/asr/model") {
        engine.push_str(&format!(" ({model})"));
    }
    lines.push(engine);

    if let Some(ms) = num("/sound/ms") {
        let mut line = format!("Recording: {:.1} s", ms as f64 / 1000.0);
        if let Some(decode) = num("/diag/asr/decode_ms") {
            line.push_str(&format!(
                ", decoded in {decode} ms ({} piece{})",
                num("/diag/asr/pieces").unwrap_or(1),
                if num("/diag/asr/pieces") == Some(1) {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if let Some(total) = num("/timing_ms/release_to_insertion") {
            line.push_str(&format!(", pasted {total} ms after the tap"));
        }
        lines.push(line);
    }

    if get("/diag/words").is_some() {
        let mut line = format!(
            "Word list (counts, never the words): {} from the screen, {} from the project, {} others",
            num("/diag/words/screen").unwrap_or(0),
            num("/diag/words/project").unwrap_or(0),
            num("/diag/words/everywhere").unwrap_or(0),
        );
        if get("/diag/words/window_read").and_then(Value::as_bool) == Some(true) {
            line.push_str(&format!(
                "; the whole window was read ({} elements)",
                num("/diag/words/elements").unwrap_or(0)
            ));
        }
        lines.push(line);
    } else {
        lines.push("Word list: off".to_string());
    }

    if get("/diag/audio").is_some() {
        let mut line = format!(
            "Microphone: {}",
            match text("/diag/audio/mic") {
                Some("built_in") => "built-in",
                Some("usb") => "USB",
                Some("bluetooth") => "Bluetooth",
                Some(_) => "other",
                None => "unknown",
            }
        );
        if let Some(rate) = num("/diag/audio/sample_rate") {
            line.push_str(&format!(", {} kHz", rate as f64 / 1000.0));
        }
        line.push_str(&format!(
            ", {} gaps in the input",
            num("/diag/audio/gaps").unwrap_or(0)
        ));
        if let Some(apps) = num("/diag/audio/other_apps_on_mic") {
            line.push_str(&format!(", {apps} other apps recording"));
        }
        lines.push(line);
    }

    if get("/diag/system").is_some() {
        let unknown = || "?".to_string();
        lines.push(format!(
            "This Mac at the time: {} windows open, GPU {}%, memory {}, temperature {}",
            num("/diag/system/windows").map_or_else(unknown, |n| n.to_string()),
            num("/diag/system/gpu_percent").map_or_else(unknown, |n| n.to_string()),
            text("/diag/system/memory_pressure").unwrap_or("?"),
            text("/diag/system/thermal").unwrap_or("?"),
        ));
    }
    lines
}

/// The report for one flagged dictation. `include_text` adds what the
/// speech engine heard and what was pasted, under `attachments` only.
pub(crate) fn build(
    record: &Value,
    code: &str,
    event_id: &str,
    env: &Environment,
    include_text: bool,
    created_ms: u64,
) -> Value {
    const KEPT: [&str; 9] = [
        "id",
        "ts",
        "app_version",
        "status",
        "asr",
        "sound",
        "timing_ms",
        "insertion",
        "diag",
    ];
    let mut dictation: Map<String, Value> = KEPT
        .iter()
        .filter_map(|key| Some((key.to_string(), record.get(*key)?.clone())))
        .collect();
    if let Some(status) = record.pointer("/llm/status") {
        dictation.insert("llm_status".into(), status.clone());
    }
    let mut report = json!({
        "code": code,
        "event_id": event_id,
        "kind": "accuracy",
        "subkind": "flagged",
        "created_ms": created_ms,
        "tags": {
            "sunoto_version": env!("CARGO_PKG_VERSION"),
            "macos_version": env.macos,
            "mac_model": env.mac_model,
            "asr_backend": record.pointer("/asr/backend"),
            "mic": record.pointer("/diag/audio/mic"),
            "word_list": record.pointer("/diag/words").is_some(),
        },
        "dictation": dictation,
    });
    if include_text {
        report["attachments"] = json!({
            "text": {
                "heard": record.pointer("/text/raw"),
                "pasted": record.pointer("/text/inserted"),
            }
        });
    }
    report
}

/// Write `report` to `dir/<folder>/<code>.json` (private to this user,
/// written whole or not at all) and return its path. `folder` is
/// `pending` (to send) or `kept` (a build with nowhere to send).
pub(crate) fn save(dir: &Path, folder: &str, report: &Value) -> io::Result<PathBuf> {
    let pending = dir.join(folder);
    fs::create_dir_all(&pending)?;
    for path in [dir, pending.as_path()] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let code = report["code"].as_str().unwrap_or("SUN-0000");
    let path = pending.join(format!("{code}.json"));
    let partial = pending.join(format!(".{code}.json.partial"));
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(report).map_err(io::Error::other)?)?;
    file.sync_all()?;
    fs::rename(&partial, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({
            "v": 1, "type": "dictation", "id": "1a183ebbf61-2", "ts": 1_793_000_000_000u64,
            "app_version": "0.1.14", "platform": "macos", "status": "inserted",
            "error": "free-form text that must not travel",
            "target": {"id": "com.apple.TextEdit", "name": "TextEdit"},
            "style": "prose",
            "asr": {"backend": "parakeet_mlx_offline", "model": "parakeet-tdt-0.6b-v2-bf16", "profile_ms": 560},
            "sound": {"ms": 5_200, "rms": 900, "peak": 20_000},
            "text": {"raw": "send the mini sign key", "polished": "Send the mini sign key.",
                     "llm": null, "inserted": "Send the mini sign key."},
            "polish_trace": [{"stage": "punctuation", "before": "a", "after": "b"}],
            "llm": {"status": "off"},
            "insertion": {"method": "pasted", "streamed": false},
            "timing_ms": {"recorded": 5_300, "release_to_insertion": 540},
            "diag": {
                "words": {"screen": 7, "project": 0, "everywhere": 25, "window_read": true,
                          "elements": 48, "chars": 480, "read_ms": 46, "near_miss_fixes": 0},
                "audio": {"mic": "built_in", "sample_rate": 48_000, "clipped_samples": 0, "gaps": 0,
                          "gap_ms": 0, "mic_changes": 0, "other_apps_on_mic": 0},
                "system": {"windows": 7, "gpu_percent": 47, "memory_pressure": "normal", "thermal": "nominal"},
                "asr": {"decode_ms": 909, "pieces": 1, "unk_retries": 0, "pause_reused": false, "boosted_tokens": 1}
            }
        })
    }

    const EVENT_ID: &str = "0123456789abcdef0123456789abcdef";

    fn env() -> Environment {
        Environment {
            macos: Some("26.1".into()),
            mac_model: Some("MacBookPro18,3".into()),
        }
    }

    #[test]
    fn a_report_without_the_box_ticked_holds_no_text() {
        let report = build(&record(), "SUN-7F3A", EVENT_ID, &env(), false, 1);
        let encoded = report.to_string();
        for private in ["mini sign", "TextEdit", "free-form", "punctuation", "prose"] {
            assert!(!encoded.contains(private), "{private} leaked: {encoded}");
        }
        assert!(report.get("attachments").is_none());
        assert!(report["dictation"].get("text").is_none());
        assert_eq!(report["dictation"]["diag"], record()["diag"]);
        assert_eq!(report["dictation"]["llm_status"], "off");
        assert_eq!(report["tags"]["mic"], "built_in");
        assert_eq!(report["tags"]["word_list"], true);
        assert_eq!(report["kind"], "accuracy");
        assert_eq!(report["subkind"], "flagged");
        assert_eq!(report["event_id"], EVENT_ID);
    }

    #[test]
    fn ticking_the_box_adds_the_text_as_an_attachment_only() {
        let report = build(&record(), "SUN-7F3A", EVENT_ID, &env(), true, 1);
        assert_eq!(
            report["attachments"]["text"],
            json!({"heard": "send the mini sign key", "pasted": "Send the mini sign key."})
        );
        let mut without = report.clone();
        without.as_object_mut().unwrap().remove("attachments");
        assert!(!without.to_string().contains("mini sign"));
    }

    #[test]
    fn the_panel_lists_counts_in_plain_words() {
        assert_eq!(
            included_lines(&record(), &env()),
            [
                "Sunoto 0.1.14 on macOS 26.1, MacBookPro18,3",
                "Speech engine: parakeet_mlx_offline (parakeet-tdt-0.6b-v2-bf16)",
                "Recording: 5.2 s, decoded in 909 ms (1 piece), pasted 540 ms after the tap",
                "Word list (counts, never the words): 7 from the screen, 0 from the project, 25 others; the whole window was read (48 elements)",
                "Microphone: built-in, 48 kHz, 0 gaps in the input, 0 other apps recording",
                "This Mac at the time: 7 windows open, GPU 47%, memory normal, temperature nominal",
            ]
        );
        // A record from before diag existed still reads.
        let old = json!({"app_version": "0.1.12", "asr": {"backend": "parakeet_mlx_offline"}});
        assert_eq!(
            included_lines(&old, &Environment::default()),
            [
                "Sunoto 0.1.12",
                "Speech engine: parakeet_mlx_offline",
                "Word list: off",
            ]
        );
    }

    #[test]
    fn saved_reports_are_private_files_named_by_code() {
        let dir = std::env::temp_dir().join(format!(
            "sunoto-reports-{}-{}",
            std::process::id(),
            crate::history::now_ms()
        ));
        let report = build(&record(), "SUN-00AB", EVENT_ID, &env(), false, 1);
        let path = save(&dir, "pending", &report).unwrap();
        assert_eq!(path, dir.join("pending/SUN-00AB.json"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, report);
        assert_eq!(fs::read_dir(dir.join("pending")).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn event_ids_are_32_random_hex_digits() {
        let (a, b) = (new_event_id(), new_event_id());
        assert_eq!(a.len(), 32);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
        assert_ne!(a, b);
    }

    #[test]
    fn codes_look_like_sun_and_four_hex_digits() {
        let code = new_code();
        assert_eq!(code.len(), 8);
        assert!(code.starts_with("SUN-"));
        assert!(
            code[4..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase())
        );
    }
}

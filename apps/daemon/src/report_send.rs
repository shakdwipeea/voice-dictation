//! Upload the accuracy reports waiting in `accuracy-reports/pending/` to
//! Sentry (docs/accuracy-reports-plan-2026-10-28.md, step 3).
//!
//! Each report is turned into one Sentry envelope: an event whose tags and
//! `contexts.sunoto` come from the report's allowlisted fields, plus a
//! `text.json` attachment only when the user ticked Include the text. A
//! `sunoto-reports` thread sends them with `curl`, oldest first: when the
//! app starts with reports waiting, whenever the panel's Send saves one,
//! and after a failed send: 1 minute later, then 2, 4, 8 ... up to
//! once an hour, so a report filed offline goes soon after the network is
//! back. Nothing here runs on the dictation path.
//!
//! What happens to a file after a send: 2xx deletes it; 429 keeps it and
//! waits for `Retry-After`; another 4xx moves it to `failed/` (sending it
//! again will not help, and the user's report stays on this Mac); 5xx or no
//! network keeps it for the next try.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::Thread;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::logging;

/// A test or self-hosted DSN; wins over the one compiled in.
const DSN_ENV: &str = "SUNOTO_REPORT_DSN";
/// Release builds get the project's DSN from the release workflow. A DSN
/// only lets you send events, so it is not a secret.
const BUILT_IN_DSN: Option<&str> = option_env!("SUNOTO_SENTRY_DSN");
/// Wait after launch before the first send, off the startup path.
const FIRST_SEND_AFTER: Duration = Duration::from_secs(30);
/// After a failed send (no network, server error) the wait starts here and
/// doubles up to `RETRY_EVERY`. With no network, curl fails at once, so a
/// try costs next to nothing.
const RETRY_FIRST: Duration = Duration::from_secs(60);
/// The longest wait between tries, and the wait when nothing failed.
const RETRY_EVERY: Duration = Duration::from_secs(60 * 60);
/// More than this many uploads in a day wait for the next day.
const MAX_PER_DAY: u32 = 20;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Where to send: the envelope endpoint and the public key for its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dsn {
    raw: String,
    key: String,
    envelope_url: String,
}

impl Dsn {
    /// `https://<key>@<host>[/<path>]/<project>`, as Sentry shows it.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (scheme, rest) = raw.split_once("://")?;
        if !matches!(scheme, "http" | "https") {
            return None;
        }
        let (key, rest) = rest.split_once('@')?;
        let key = key.split(':').next()?;
        let (host, path) = rest.split_once('/')?;
        let (prefix, project) = match path.trim_end_matches('/').rsplit_once('/') {
            Some((prefix, project)) => (format!("/{prefix}"), project),
            None => (String::new(), path.trim_end_matches('/')),
        };
        if key.is_empty() || host.is_empty() || project.is_empty() {
            return None;
        }
        Some(Self {
            raw: raw.to_string(),
            key: key.to_string(),
            envelope_url: format!("{scheme}://{host}{prefix}/api/{project}/envelope/"),
        })
    }

    /// `SUNOTO_REPORT_DSN`, else the release build's own; `None` in builds
    /// that have nowhere to send.
    pub(crate) fn current() -> Option<Self> {
        let from_env = std::env::var(DSN_ENV).ok().filter(|v| !v.trim().is_empty());
        let raw = from_env
            .as_deref()
            .or(BUILT_IN_DSN.filter(|v| !v.is_empty()))?;
        let dsn = Self::parse(raw);
        if dsn.is_none() {
            logging::warn("accuracy reports: the report DSN is not a valid Sentry DSN");
        }
        dsn
    }
}

/// One saved report as a Sentry envelope. Only the report's own fields go
/// in, each in a fixed place, so nothing new can slip through.
pub(crate) fn envelope(report: &Value, dsn: &Dsn) -> Vec<u8> {
    let field = |path: &str| report.pointer(path).cloned().unwrap_or(Value::Null);
    let event_id = report["event_id"].as_str().unwrap_or_default();
    let code = report["code"].as_str().unwrap_or("SUN-0000");
    let version = report
        .pointer("/tags/sunoto_version")
        .and_then(Value::as_str);

    let mut tags = Map::new();
    let mut tag = |name: &str, value: Option<String>| {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            tags.insert(name.to_string(), Value::String(value));
        }
    };
    let text = |path: &str| {
        report
            .pointer(path)
            .and_then(Value::as_str)
            .map(String::from)
    };
    tag("kind", text("/kind"));
    tag("subkind", text("/subkind"));
    tag("report_code", Some(code.to_string()));
    tag("sunoto_version", version.map(String::from));
    tag("macos_version", text("/tags/macos_version"));
    tag("mac_model", text("/tags/mac_model"));
    tag("asr_backend", text("/tags/asr_backend"));
    tag("mic", text("/tags/mic"));
    tag(
        "word_list",
        report
            .pointer("/tags/word_list")
            .and_then(Value::as_bool)
            .map(|on| if on { "on" } else { "off" }.to_string()),
    );
    tag(
        "text_attached",
        Some(
            if report.pointer("/attachments/text").is_some() {
                "yes"
            } else {
                "no"
            }
            .into(),
        ),
    );

    let mut dictation = field("/dictation");
    // Sentry's default data scrubber blanks any field whose name holds
    // "token", so this count would arrive as null.
    if let Some(asr) = dictation
        .pointer_mut("/diag/asr")
        .and_then(Value::as_object_mut)
        && let Some(count) = asr.remove("boosted_tokens")
    {
        asr.insert("boost_changed_picks".into(), count);
    }
    let mut contexts = json!({ "sunoto": dictation });
    if let Some(macos) = text("/tags/macos_version") {
        contexts["os"] = json!({ "name": "macOS", "version": macos });
    }
    if let Some(model) = text("/tags/mac_model") {
        contexts["device"] = json!({ "model": model });
    }
    let event = json!({
        "event_id": event_id,
        "timestamp": report["created_ms"].as_u64().unwrap_or_default() as f64 / 1000.0,
        "platform": "other",
        "level": "info",
        "logger": "accuracy",
        "release": version.map(|v| format!("sunoto@{v}")),
        "environment": "release",
        // One title for the one issue every report groups into; the code is
        // the `report_code` tag.
        "message": { "formatted": "A dictation was reported wrong" },
        "fingerprint": [field("/kind"), field("/subkind")],
        "tags": tags,
        "contexts": contexts,
    });

    // The envelope header, then each item's header and body, one line each.
    let mut out = json!({ "event_id": event_id, "dsn": dsn.raw })
        .to_string()
        .into_bytes();
    let mut item = |header: Value, body: &[u8]| {
        out.push(b'\n');
        out.extend_from_slice(header.to_string().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(body);
    };
    let body = event.to_string().into_bytes();
    item(json!({ "type": "event", "length": body.len() }), &body);
    if let Some(text) = report.pointer("/attachments/text") {
        let body = text.to_string().into_bytes();
        item(
            json!({
                "type": "attachment",
                "length": body.len(),
                "filename": "text.json",
                "content_type": "application/json",
            }),
            &body,
        );
    }
    out.push(b'\n');
    out
}

/// How Sentry answered one envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    /// Rejected for good (bad DSN, too large): stop resending this file.
    Rejected(u16),
    /// Rate-limited: send nothing for this many seconds.
    Backoff(u64),
    /// Server error or no network: try again later.
    Retry,
}

fn classify(status: u16, retry_after: Option<u64>) -> Outcome {
    match status {
        200..=299 => Outcome::Sent,
        429 => Outcome::Backoff(retry_after.unwrap_or(RETRY_EVERY.as_secs())),
        400..=499 => Outcome::Rejected(status),
        _ => Outcome::Retry,
    }
}

/// POST one envelope with curl. The reply's headers come back on stdout,
/// then the status code on a line of its own (`000` with no network).
fn post(envelope: &[u8], dsn: &Dsn) -> Outcome {
    let version = env!("CARGO_PKG_VERSION");
    let child = Command::new("curl")
        .args(["--silent", "--max-time", "15", "-o", "/dev/null", "-D", "-"])
        .args(["-w", "\n%{http_code}", "-A", &format!("Sunoto/{version}")])
        .args(["-H", "Content-Type: application/x-sentry-envelope"])
        .arg("-H")
        .arg(format!(
            "X-Sentry-Auth: Sentry sentry_version=7, sentry_key={}, sentry_client=sunoto/{version}",
            dsn.key
        ))
        .args(["--data-binary", "@-"])
        .arg(&dsn.envelope_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let output = child.and_then(|mut child| {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(envelope)?;
        }
        child.wait_with_output()
    });
    let Ok(output) = output else {
        return Outcome::Retry;
    };
    let reply = String::from_utf8_lossy(&output.stdout);
    let status = reply
        .lines()
        .last()
        .and_then(|line| line.trim().parse().ok());
    let retry_after = reply.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("retry-after")
            .then(|| value.trim().parse().ok())?
    });
    classify(status.unwrap_or(0), retry_after)
}

/// Kept in `state.json`: today's upload count and any rate-limit wait.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct State {
    day: u64,
    sent: u32,
    wait_until_ms: u64,
}

impl State {
    fn load(dir: &Path) -> Self {
        fs::read(dir.join("state.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) {
        let _ = fs::write(dir.join("state.json"), json!(self).to_string());
    }

    /// May one more report go now?
    fn allows(&mut self, now_ms: u64) -> bool {
        if self.day != now_ms / DAY_MS {
            self.day = now_ms / DAY_MS;
            self.sent = 0;
        }
        now_ms >= self.wait_until_ms && self.sent < MAX_PER_DAY
    }
}

/// The reports in `pending/`, oldest first.
fn pending(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.join("pending")) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    files.sort();
    files.into_iter().map(|(_, path)| path).collect()
}

/// Send what is waiting in `dir/pending/`, stopping at the first report
/// that cannot go now. `send` is `post` outside tests. True when a send
/// failed and the report waits for the next try.
fn send_pending(dir: &Path, dsn: &Dsn, now_ms: u64, send: impl Fn(&[u8], &Dsn) -> Outcome) -> bool {
    let mut state = State::load(dir);
    let mut failed = false;
    for path in pending(dir) {
        if !state.allows(now_ms) {
            break;
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut report: Value = match fs::read(&path).map(|bytes| serde_json::from_slice(&bytes)) {
            Ok(Ok(report)) => report,
            _ => {
                logging::warn(&format!(
                    "accuracy report {name} is unreadable; moved to failed/"
                ));
                move_to_failed(dir, &path);
                continue;
            }
        };
        // Reports saved before step 3 have no event id yet.
        if report["event_id"].as_str().is_none_or(str::is_empty) {
            report["event_id"] = crate::report::new_event_id().into();
            if let Ok(bytes) = serde_json::to_vec_pretty(&report) {
                let _ = fs::write(&path, bytes);
            }
        }
        match send(&envelope(&report, dsn), dsn) {
            Outcome::Sent => {
                state.sent += 1;
                let _ = fs::remove_file(&path);
                logging::info(&format!("accuracy report {name} sent"));
            }
            Outcome::Rejected(status) => {
                logging::warn(&format!(
                    "accuracy report {name} was refused (HTTP {status}); kept in failed/"
                ));
                move_to_failed(dir, &path);
            }
            Outcome::Backoff(secs) => {
                state.wait_until_ms = now_ms + secs * 1000;
                logging::info(&format!("accuracy reports: rate-limited, waiting {secs} s"));
                break;
            }
            Outcome::Retry => {
                logging::info(&format!(
                    "accuracy report {name} could not be sent; trying again later"
                ));
                failed = true;
                break;
            }
        }
    }
    state.save(dir);
    failed
}

/// The wait before the next run: `retry` after a failure, which then
/// doubles up to an hour; an hour (and `retry` back to a minute) otherwise.
fn next_wait(failed: bool, retry: &mut Duration) -> Duration {
    if failed {
        let wait = *retry;
        *retry = (*retry * 2).min(RETRY_EVERY);
        wait
    } else {
        *retry = RETRY_FIRST;
        RETRY_EVERY
    }
}

fn move_to_failed(dir: &Path, path: &Path) {
    let failed = dir.join("failed");
    let moved = fs::create_dir_all(&failed)
        .and_then(|()| fs::rename(path, failed.join(path.file_name().unwrap_or_default())));
    if moved.is_err() {
        let _ = fs::remove_file(path);
    }
}

/// The daemon's handle on the `sunoto-reports` thread, which starts only
/// when there is somewhere to send and a report the user chose to Send.
pub(crate) struct Sender {
    dsn: Option<Dsn>,
    thread: Option<Thread>,
}

impl Sender {
    pub(crate) fn new() -> Self {
        let mut sender = Self {
            dsn: Dsn::current(),
            thread: None,
        };
        // Reports left from an earlier run (offline, or quit before sending).
        if !pending(&crate::report::reports_dir()).is_empty() {
            sender.start(FIRST_SEND_AFTER);
        }
        sender
    }

    /// This build has a DSN, so Send can upload.
    pub(crate) fn available(&self) -> bool {
        self.dsn.is_some()
    }

    /// A report was just queued: send it now.
    pub(crate) fn send_now(&mut self) {
        match &self.thread {
            Some(thread) => thread.unpark(),
            None => self.start(Duration::ZERO),
        }
    }

    fn start(&mut self, first_after: Duration) {
        let Some(dsn) = self.dsn.clone() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("sunoto-reports".to_string())
            .spawn(move || {
                std::thread::park_timeout(first_after);
                let mut retry = RETRY_FIRST;
                loop {
                    let dir = crate::report::reports_dir();
                    let failed = send_pending(&dir, &dsn, crate::history::now_ms(), post);
                    std::thread::park_timeout(next_wait(failed, &mut retry));
                }
            });
        match spawned {
            Ok(handle) => self.thread = Some(handle.thread().clone()),
            Err(error) => {
                logging::warn(&format!("could not start the report sender: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn dsn() -> Dsn {
        Dsn::parse("https://abc123@o42.ingest.sentry.io/4507").unwrap()
    }

    fn report(with_text: bool) -> Value {
        let mut report = json!({
            "code": "SUN-7F3A",
            "event_id": "0123456789abcdef0123456789abcdef",
            "kind": "accuracy",
            "subkind": "flagged",
            "created_ms": 1_793_000_000_000u64,
            "tags": {
                "sunoto_version": "0.1.15", "macos_version": "26.1",
                "mac_model": "MacBookPro18,3", "asr_backend": "parakeet_mlx_offline",
                "mic": "built_in", "word_list": true,
            },
            "dictation": {"id": "1a1-3", "diag": {"words": {"screen": 7}}},
        });
        if with_text {
            report["attachments"] =
                json!({"text": {"heard": "canary words", "pasted": "Canary words."}});
        }
        report
    }

    fn items(envelope: &[u8]) -> Vec<Value> {
        let text = String::from_utf8(envelope.to_vec()).unwrap();
        let mut lines = text.split('\n');
        let mut items = vec![serde_json::from_str(lines.next().unwrap()).unwrap()];
        while let Some(header) = lines.next().filter(|line| !line.is_empty()) {
            let header: Value = serde_json::from_str(header).unwrap();
            let length = header["length"].as_u64().unwrap() as usize;
            let body = lines.next().unwrap();
            assert_eq!(body.len(), length, "item length must match its body");
            items.push(header);
            items.push(serde_json::from_str(body).unwrap());
        }
        items
    }

    #[test]
    fn dsns_parse_into_the_envelope_endpoint() {
        assert_eq!(
            dsn().envelope_url,
            "https://o42.ingest.sentry.io/api/4507/envelope/"
        );
        assert_eq!(dsn().key, "abc123");
        let local = Dsn::parse("http://key:secret@127.0.0.1:9000/sentry/7/").unwrap();
        assert_eq!(
            local.envelope_url,
            "http://127.0.0.1:9000/sentry/api/7/envelope/"
        );
        assert_eq!(local.key, "key");
        for bad in [
            "",
            "abc",
            "ftp://k@h/1",
            "https://h/1",
            "https://k@h",
            "https://k@h/",
        ] {
            assert_eq!(Dsn::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_event_holds_tags_and_counts_and_no_text() {
        let items = items(&envelope(&report(false), &dsn()));
        assert_eq!(items.len(), 3, "envelope header, event header, event");
        assert_eq!(items[0]["event_id"], "0123456789abcdef0123456789abcdef");
        let event = &items[2];
        assert_eq!(event["event_id"], items[0]["event_id"]);
        assert_eq!(event["release"], "sunoto@0.1.15");
        assert_eq!(event["fingerprint"], json!(["accuracy", "flagged"]));
        assert_eq!(
            event["tags"],
            json!({
                "kind": "accuracy", "subkind": "flagged", "report_code": "SUN-7F3A",
                "sunoto_version": "0.1.15", "macos_version": "26.1",
                "mac_model": "MacBookPro18,3", "asr_backend": "parakeet_mlx_offline",
                "mic": "built_in", "word_list": "on", "text_attached": "no",
            })
        );
        assert_eq!(event["contexts"]["sunoto"], report(false)["dictation"]);
        assert_eq!(
            event["contexts"]["os"],
            json!({"name": "macOS", "version": "26.1"})
        );
        let keys: Vec<&str> = event
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for absent in ["user", "server_name", "request", "extra", "breadcrumbs"] {
            assert!(!keys.contains(&absent), "{absent} must not be sent");
        }
    }

    #[test]
    fn counts_named_like_secrets_are_renamed_for_the_scrubber() {
        let mut sample = report(false);
        sample["dictation"]["diag"]["asr"] = json!({"decode_ms": 120, "boosted_tokens": 0});
        let event = &items(&envelope(&sample, &dsn()))[2];
        assert_eq!(
            event["contexts"]["sunoto"]["diag"]["asr"],
            json!({"decode_ms": 120, "boost_changed_picks": 0})
        );
        assert!(!event.to_string().contains("token"));
    }

    #[test]
    fn ticked_text_travels_only_as_an_attachment() {
        let envelope = envelope(&report(true), &dsn());
        let items = items(&envelope);
        assert_eq!(items.len(), 5);
        assert_eq!(items[3]["type"], "attachment");
        assert_eq!(items[3]["filename"], "text.json");
        assert_eq!(items[4]["heard"], "canary words");
        assert!(
            !items[2].to_string().contains("anary"),
            "no text in the event"
        );
        assert_eq!(items[2]["tags"]["text_attached"], "yes");
    }

    #[test]
    fn answers_map_to_what_happens_to_the_file() {
        assert_eq!(classify(200, None), Outcome::Sent);
        assert_eq!(classify(429, Some(120)), Outcome::Backoff(120));
        assert_eq!(classify(429, None), Outcome::Backoff(3600));
        assert_eq!(classify(403, None), Outcome::Rejected(403));
        assert_eq!(classify(413, None), Outcome::Rejected(413));
        assert_eq!(classify(503, None), Outcome::Retry);
        assert_eq!(classify(0, None), Outcome::Retry);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sunoto-send-{name}-{}-{}",
            std::process::id(),
            crate::history::now_ms()
        ));
        fs::create_dir_all(dir.join("pending")).unwrap();
        dir
    }

    fn put(dir: &Path, code: &str) {
        let mut report = report(false);
        report["code"] = code.into();
        fs::write(dir.join(format!("pending/{code}.json")), report.to_string()).unwrap();
        // Distinct modification times keep the order oldest first.
        std::thread::sleep(Duration::from_millis(15));
    }

    #[test]
    fn failed_sends_are_tried_again_soon_then_less_often() {
        let mut retry = RETRY_FIRST;
        let waits: Vec<u64> = (0..8)
            .map(|_| next_wait(true, &mut retry).as_secs() / 60)
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60], "minutes");
        assert_eq!(next_wait(false, &mut retry), RETRY_EVERY);
        assert_eq!(
            next_wait(true, &mut retry),
            RETRY_FIRST,
            "a success starts over"
        );
    }

    #[test]
    fn sending_deletes_sent_files_and_keeps_the_rest_in_order() {
        let dir = temp_dir("order");
        for code in ["SUN-0001", "SUN-0002", "SUN-0003", "SUN-0004"] {
            put(&dir, code);
        }
        let answers = RefCell::new(vec![Outcome::Sent, Outcome::Rejected(400), Outcome::Retry]);
        let seen = RefCell::new(Vec::new());
        let failed = send_pending(&dir, &dsn(), 10 * DAY_MS, |envelope, _| {
            let event = &items(envelope)[2];
            seen.borrow_mut()
                .push(event["tags"]["report_code"].as_str().unwrap().to_string());
            answers.borrow_mut().remove(0)
        });
        assert!(failed, "the retry is reported, so the next try comes soon");
        assert_eq!(*seen.borrow(), ["SUN-0001", "SUN-0002", "SUN-0003"]);
        let left: Vec<_> = pending(&dir)
            .iter()
            .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["SUN-0003", "SUN-0004"], "a retry stops the run");
        assert!(dir.join("failed/SUN-0002.json").exists());
        assert_eq!(State::load(&dir).sent, 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rate_limit_waits_and_the_daily_cap_holds() {
        let dir = temp_dir("limit");
        put(&dir, "SUN-0001");
        let now = 10 * DAY_MS;
        send_pending(&dir, &dsn(), now, |_, _| Outcome::Backoff(60));
        assert_eq!(State::load(&dir).wait_until_ms, now + 60_000);
        send_pending(&dir, &dsn(), now + 30_000, |_, _| panic!("must wait"));
        send_pending(&dir, &dsn(), now + 61_000, |_, _| Outcome::Sent);
        assert!(pending(&dir).is_empty());

        State {
            day: 10,
            sent: MAX_PER_DAY,
            wait_until_ms: 0,
        }
        .save(&dir);
        put(&dir, "SUN-0002");
        send_pending(&dir, &dsn(), now + 62_000, |_, _| {
            panic!("over the daily cap")
        });
        send_pending(&dir, &dsn(), now + DAY_MS, |_, _| Outcome::Sent);
        assert!(pending(&dir).is_empty(), "a new day resets the cap");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_reports_get_an_event_id_kept_for_resends() {
        let dir = temp_dir("eventid");
        let mut old = report(false);
        old.as_object_mut().unwrap().remove("event_id");
        fs::write(dir.join("pending/SUN-0009.json"), old.to_string()).unwrap();
        let ids = RefCell::new(Vec::new());
        for _ in 0..2 {
            send_pending(&dir, &dsn(), 10 * DAY_MS, |envelope, _| {
                ids.borrow_mut()
                    .push(items(envelope)[0]["event_id"].as_str().unwrap().to_string());
                Outcome::Retry
            });
        }
        let ids = ids.borrow();
        assert_eq!(ids[0].len(), 32);
        assert_eq!(ids[0], ids[1]);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One request to a local socket that answers `reply`; returns what
    /// `post` made of it and the request as received.
    fn post_to_local(reply: &'static str) -> (Outcome, String) {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            request.push_str(&String::from_utf8(body).unwrap());
            reader.get_mut().write_all(reply.as_bytes()).unwrap();
            request
        });
        let dsn = Dsn::parse(&format!("http://pubkey@127.0.0.1:{port}/7")).unwrap();
        let outcome = post(&envelope(&report(false), &dsn), &dsn);
        (outcome, server.join().unwrap())
    }

    #[test]
    fn curl_posts_the_envelope_and_reads_the_answer() {
        let (outcome, request) =
            post_to_local("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert_eq!(outcome, Outcome::Sent);
        assert!(
            request.starts_with("POST /api/7/envelope/ HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("sentry_key=pubkey"));
        assert!(request.contains("Content-Type: application/x-sentry-envelope"));
        assert!(request.contains(r#""report_code":"SUN-7F3A""#));

        let (outcome, _) = post_to_local(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 42\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(outcome, Outcome::Backoff(42));
    }

    #[test]
    fn no_server_means_try_again_later() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dsn = Dsn::parse(&format!("http://k@127.0.0.1:{port}/7")).unwrap();
        assert_eq!(post(&envelope(&report(false), &dsn), &dsn), Outcome::Retry);
    }

    /// Sends one sample report to the project in `SUNOTO_REPORT_DSN`:
    /// `SUNOTO_REPORT_DSN=… cargo test --offline -p sunoto-daemon
    /// send_a_sample_report -- --ignored --nocapture`, then look for its
    /// code in Sentry.
    #[test]
    #[ignore = "sends to a real Sentry project"]
    fn send_a_sample_report() {
        let dsn = Dsn::current().expect("set SUNOTO_REPORT_DSN");
        let mut sample = report(false);
        sample["code"] = crate::report::new_code().into();
        sample["event_id"] = crate::report::new_event_id().into();
        sample["created_ms"] = crate::history::now_ms().into();
        sample["tags"]["sunoto_version"] = env!("CARGO_PKG_VERSION").into();
        let outcome = post(&envelope(&sample, &dsn), &dsn);
        println!(
            "{} (event {}): {outcome:?}",
            sample["code"], sample["event_id"]
        );
        assert_eq!(outcome, Outcome::Sent);
    }
}

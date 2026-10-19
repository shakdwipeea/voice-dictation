//! Local dictation history (docs/local-history-plan-2026-10-16.md).
//!
//! With `history_enabled` on, every dictation becomes one JSON line in a
//! daily file on this machine: what the speech engine heard, each polish
//! stage, what was inserted, where, and how long it took. Nothing leaves the
//! machine. The event loop only fills in a draft and hands the finished
//! record to a writer thread with `try_send`, so saving never delays a
//! paste; when the writer falls behind, the record is dropped instead.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sunoto_ipc::RecentDictation;
use sunoto_polish::{StageTrace, StyleProfile};

use crate::logging;
use crate::session::{SessionAudioStats, TimingBreakdown};
use crate::settings::Settings;

pub(crate) const SCHEMA_VERSION: u32 = 1;
const CHANNEL_CAPACITY: usize = 16;
/// Dictations in the menu bar's Recent Dictations.
pub(crate) const RECENT_COUNT: usize = 10;
const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Password managers: dictations into them keep their timings, never text.
pub(crate) fn default_exclude_apps() -> Vec<String> {
    [
        "com.1password.1password",
        "com.agilebits.onepassword7",
        "com.bitwarden.desktop",
        "com.apple.Passwords",
        "com.apple.keychainaccess",
    ]
    .map(str::to_string)
    .to_vec()
}

/// `~/Library/Application Support/sunoto/history` on macOS,
/// `$XDG_DATA_HOME/sunoto/history` elsewhere; `SUNOTO_HISTORY_DIR` wins.
pub(crate) fn history_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SUNOTO_HISTORY_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        return PathBuf::from(home).join("Library/Application Support/sunoto/history");
    }
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home).join(".local/share"))
        .join("sunoto/history")
}

pub(crate) fn dictations_dir(root: &Path) -> PathBuf {
    root.join("dictations")
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// (year, month, day) of a day count since 1970-01-01, proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Inverse of `civil_from_days`.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn day_of(ts_ms: u64) -> i64 {
    (ts_ms / DAY_MS) as i64
}

/// `2026-10-16.jsonl` for a timestamp, in UTC like the daemon log.
pub(crate) fn day_file_name(ts_ms: u64) -> String {
    let (year, month, day) = civil_from_days(day_of(ts_ms));
    format!("{year:04}-{month:02}-{day:02}.jsonl")
}

/// The day number of a `YYYY-MM-DD.jsonl` name; `None` for anything else.
pub(crate) fn parse_day_file(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".jsonl")?;
    let mut parts = stem.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if stem.len() != 10 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// `2026-10-16 10:42Z`
pub(crate) fn format_utc(ts_ms: u64) -> String {
    let (year, month, day) = civil_from_days(day_of(ts_ms));
    let minutes = ts_ms % DAY_MS / 60_000;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        minutes / 60,
        minutes % 60
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Inserted,
    ClipboardOnly,
    SecureField,
    EmptyTranscript,
    EmptyResult,
    Cancelled,
    AsrTimeout,
    AsrError,
    InsertError,
    /// A new dictation started while this one was still open: a path that
    /// does not finish its record. Should not happen; visible if it does.
    Abandoned,
}

/// Why nothing was inserted: the speech engine heard nothing, or polishing
/// left nothing of what it heard.
pub(crate) fn empty_status(raw: &str) -> Status {
    if raw.trim().is_empty() {
        Status::EmptyTranscript
    } else {
        Status::EmptyResult
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Target {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Asr {
    pub(crate) backend: String,
    pub(crate) model: Option<String>,
    pub(crate) profile_ms: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Sound {
    pub(crate) ms: u64,
    pub(crate) rms: u32,
    pub(crate) peak: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Texts {
    pub(crate) raw: Option<String>,
    pub(crate) polished: Option<String>,
    pub(crate) llm: Option<String>,
    pub(crate) inserted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Stage {
    pub(crate) stage: &'static str,
    pub(crate) before: String,
    pub(crate) after: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LlmStatus {
    #[default]
    Off,
    Accepted,
    Skipped,
    /// The streamed text was kept as typed after the call failed.
    StreamedPartial,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Llm {
    pub(crate) status: LlmStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Insertion {
    pub(crate) method: &'static str,
    pub(crate) streamed: bool,
}

/// One dictation, as written to the day file (schema v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Record {
    pub(crate) v: u32,
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) id: String,
    pub(crate) ts: u64,
    pub(crate) app_version: &'static str,
    pub(crate) platform: &'static str,
    pub(crate) status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// Text was left out on purpose: a password field or an excluded app.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) redacted: bool,
    pub(crate) target: Option<Target>,
    pub(crate) style: Option<StyleProfile>,
    pub(crate) asr: Asr,
    pub(crate) sound: Option<Sound>,
    pub(crate) text: Texts,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) polish_trace: Vec<Stage>,
    pub(crate) llm: Llm,
    pub(crate) insertion: Option<Insertion>,
    pub(crate) timing_ms: Option<TimingBreakdown>,
}

impl Record {
    fn new(session_id: u64, ts: u64, asr: Asr) -> Self {
        Self {
            v: SCHEMA_VERSION,
            kind: "dictation",
            // The daemon's session ids restart at 1 on every launch; the
            // timestamp makes the id unique across launches.
            id: format!("{ts:x}-{session_id}"),
            ts,
            app_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
            status: Status::Abandoned,
            error: None,
            redacted: false,
            target: None,
            style: None,
            asr,
            sound: None,
            text: Texts::default(),
            polish_trace: Vec::new(),
            llm: Llm::default(),
            insertion: None,
            timing_ms: None,
        }
    }

    fn redact(&mut self) {
        self.redacted = true;
        self.text = Texts::default();
        self.polish_trace.clear();
        self.llm.error = None;
    }
}

struct Draft {
    session_id: u64,
    record: Record,
    /// The final transcript arrived; from here the session ends through an
    /// explicit `finish`, never through `reap`.
    transcribed: bool,
    /// The text went out through streaming LLM insertion.
    streamed: bool,
}

/// The event loop's handle on history. Every method is a cheap no-op when
/// history is off, so the loop calls them unconditionally.
pub(crate) struct HistoryRecorder {
    writer: Option<SyncSender<Box<Record>>>,
    exclude_apps: Vec<String>,
    asr: Asr,
    draft: Option<Draft>,
    dropped: u64,
    /// The newest dictations that kept their text, newest first, for the
    /// menu bar.
    recent: VecDeque<RecentDictation>,
    recent_changed: bool,
}

impl HistoryRecorder {
    pub(crate) fn start(settings: &Settings) -> Self {
        let mut recorder = Self::new(None, settings);
        if !settings.history_enabled {
            return recorder;
        }
        let root = history_dir();
        match spawn_writer(&root, settings.history_retention_days) {
            Ok(writer) => {
                let kept = match settings.history_retention_days {
                    0 => "kept until deleted".to_string(),
                    days => format!("kept {days} days"),
                };
                logging::info(&format!(
                    "dictation history on: {} ({kept})",
                    dictations_dir(&root).display()
                ));
                recorder.writer = Some(writer);
                match crate::history_cli::recent_dictations(&dictations_dir(&root), RECENT_COUNT) {
                    Ok(recent) => recorder.recent = recent.into(),
                    Err(error) => logging::warn(&format!(
                        "dictation history: cannot read recent dictations: {error}"
                    )),
                }
            }
            Err(error) => logging::warn(&format!(
                "dictation history disabled: cannot use {}: {error}",
                root.display()
            )),
        }
        recorder
    }

    fn new(writer: Option<SyncSender<Box<Record>>>, settings: &Settings) -> Self {
        let model = settings.backend.starts_with("parakeet").then(|| {
            // A local folder path carries the home directory; its name is
            // enough to tell models apart.
            let model = settings.parakeet_model();
            Path::new(&model)
                .file_name()
                .filter(|_| model.starts_with('/'))
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or(model)
        });
        Self {
            writer,
            exclude_apps: settings.history_exclude_apps.clone(),
            asr: Asr {
                backend: settings.backend.clone(),
                model,
                profile_ms: settings.profile_ms,
            },
            draft: None,
            dropped: 0,
            recent: VecDeque::new(),
            recent_changed: false,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.writer.is_some()
    }

    /// The newest dictations kept with their text, newest first.
    pub(crate) fn recent(&self) -> Vec<RecentDictation> {
        self.recent.iter().cloned().collect()
    }

    /// Whether `recent` changed since the last call.
    pub(crate) fn take_recent_changed(&mut self) -> bool {
        std::mem::take(&mut self.recent_changed)
    }

    /// Delete every saved dictation, as `sunoto history clear --yes` does.
    /// Returns the number of day files removed.
    pub(crate) fn clear(&mut self) -> io::Result<usize> {
        self.recent.clear();
        self.recent_changed = true;
        crate::history_cli::clear(&dictations_dir(&history_dir()))
    }

    /// A dictation started. System mode is never recorded, so the loop calls
    /// this for dictation sessions only.
    pub(crate) fn begin(&mut self, session_id: u64) {
        if self.writer.is_none() {
            return;
        }
        if let Some(stale) = self.draft.take() {
            self.send(stale.record);
        }
        self.draft = Some(Draft {
            session_id,
            record: Record::new(session_id, now_ms(), self.asr.clone()),
            transcribed: false,
            streamed: false,
        });
    }

    fn draft(&mut self, session_id: u64) -> Option<&mut Record> {
        self.draft
            .as_mut()
            .filter(|draft| draft.session_id == session_id)
            .map(|draft| &mut draft.record)
    }

    /// The window focused at release: `(bundle id, name)` on macOS,
    /// `(instance, class)` from WM_CLASS on X11.
    pub(crate) fn target(&mut self, class: Option<&(String, String)>) {
        if let Some(draft) = self.draft.as_mut()
            && let Some((id, name)) = class
        {
            draft.record.target = Some(Target {
                id: id.clone(),
                name: name.clone(),
            });
        }
    }

    pub(crate) fn transcript(
        &mut self,
        session_id: u64,
        text: &str,
        audio: Option<&SessionAudioStats>,
    ) {
        if let Some(draft) = self.draft.as_mut().filter(|d| d.session_id == session_id) {
            draft.transcribed = true;
            draft.record.text.raw = Some(text.to_string());
            draft.record.sound = audio.map(|stats| Sound {
                ms: stats.duration_ms() as u64,
                rms: stats.rms().round() as u32,
                peak: stats.peak,
            });
        }
    }

    pub(crate) fn polished(
        &mut self,
        session_id: u64,
        style: StyleProfile,
        trace: &[StageTrace],
        text: &str,
    ) {
        if let Some(record) = self.draft(session_id) {
            record.style = Some(style);
            record.polish_trace = trace
                .iter()
                .map(|stage| Stage {
                    stage: stage.stage,
                    before: stage.before.clone(),
                    after: stage.after.clone(),
                })
                .collect();
            record.text.polished = Some(text.to_string());
        }
    }

    pub(crate) fn llm(&mut self, session_id: u64, llm: Llm, text: Option<&str>) {
        if let Some(record) = self.draft(session_id) {
            record.llm = llm;
            record.text.llm = text.map(str::to_string);
        }
    }

    /// The exact text handed to insertion, after sanitizing.
    pub(crate) fn inserted(&mut self, session_id: u64, text: &str, streamed: bool) {
        if let Some(draft) = self.draft.as_mut().filter(|d| d.session_id == session_id) {
            draft.streamed = streamed;
            draft.record.text.inserted = Some(text.to_string());
        }
    }

    /// The dictation is over: complete the record and queue it for writing.
    /// `method` is how the text reached the app, when it got that far.
    pub(crate) fn finish(
        &mut self,
        session_id: u64,
        status: Status,
        error: Option<String>,
        method: Option<&'static str>,
        timing: Option<TimingBreakdown>,
    ) {
        if self.draft.as_ref().map(|draft| draft.session_id) != Some(session_id) {
            return;
        }
        let Some(Draft {
            mut record,
            streamed,
            ..
        }) = self.draft.take()
        else {
            return;
        };
        record.status = status;
        record.error = error;
        record.insertion = method.map(|method| Insertion { method, streamed });
        record.timing_ms = timing;
        let excluded = record.target.as_ref().is_some_and(|target| {
            self.exclude_apps.iter().any(|app| {
                app.eq_ignore_ascii_case(&target.id) || app.eq_ignore_ascii_case(&target.name)
            })
        });
        if status == Status::SecureField || excluded {
            record.redact();
        }
        self.send(record);
    }

    /// Ends a draft whose session the machine dropped before a transcript
    /// arrived without passing through an explicit `finish`: the ASR
    /// sidecar was lost. Called at the top of every loop turn.
    pub(crate) fn reap(&mut self, current_session: Option<u64>) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        if draft.transcribed || current_session == Some(draft.session_id) {
            return;
        }
        let session_id = draft.session_id;
        self.finish(
            session_id,
            Status::AsrError,
            Some("the speech engine stopped".to_string()),
            None,
            None,
        );
    }

    fn send(&mut self, record: Record) {
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        let recent = recent_entry(&record);
        match writer.try_send(Box::new(record)) {
            Ok(()) => {
                if let Some(entry) = recent {
                    self.recent.push_front(entry);
                    self.recent.truncate(RECENT_COUNT);
                    self.recent_changed = true;
                }
            }
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                logging::warn(&format!(
                    "dictation history: writer busy, record not saved ({} dropped so far)",
                    self.dropped
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                logging::warn("dictation history: writer stopped; history is off until restart");
                self.writer = None;
                self.draft = None;
            }
        }
    }
}

/// What Recent Dictations shows for a record: the pasted text, unless it
/// was left out (a password field or an excluded app) or empty.
fn recent_entry(record: &Record) -> Option<RecentDictation> {
    let text = record.text.inserted.as_deref()?.trim();
    (!record.redacted && !text.is_empty()).then(|| RecentDictation {
        text: text.to_string(),
        at_ms: record.ts,
    })
}

/// Create `root/dictations` (mode 0700) and start the writer thread, which
/// also deletes expired day files at start and once a day.
fn spawn_writer(root: &Path, retention_days: u32) -> io::Result<SyncSender<Box<Record>>> {
    let dir = prepare_dir(root)?;
    let (tx, rx) = mpsc::sync_channel::<Box<Record>>(CHANNEL_CAPACITY);
    std::thread::Builder::new()
        .name("history-writer".into())
        .spawn(move || {
            prune_logged(&dir, retention_days);
            let mut next_prune = Instant::now() + PRUNE_INTERVAL;
            loop {
                match rx.recv_timeout(next_prune.saturating_duration_since(Instant::now())) {
                    Ok(record) => {
                        if let Err(error) = append_record(&dir, &record) {
                            logging::warn(&format!(
                                "dictation history: cannot write {}: {error}",
                                dir.display()
                            ));
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        prune_logged(&dir, retention_days);
                        next_prune = Instant::now() + PRUNE_INTERVAL;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;
    Ok(tx)
}

/// `root/dictations`, created private to this user.
pub(crate) fn prepare_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = dictations_dir(root);
    fs::create_dir_all(&dir)?;
    for path in [root, dir.as_path()] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

pub(crate) fn append_record(dir: &Path, record: &Record) -> io::Result<()> {
    let line = serde_json::to_string(record).map_err(io::Error::other)?;
    append_line(&dir.join(day_file_name(record.ts)), &line)
}

/// One `write_all` of the whole line in append mode, so a concurrent reader
/// or a crash sees at most one partial last line. The file is opened per
/// line: the CLI may delete or rewrite day files while the daemon runs.
pub(crate) fn append_line(path: &Path, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let mut buffer = String::with_capacity(line.len() + 1);
    buffer.push_str(line);
    buffer.push('\n');
    file.write_all(buffer.as_bytes())
}

fn prune_logged(dir: &Path, retention_days: u32) {
    match prune(dir, retention_days, day_of(now_ms())) {
        Ok(0) => {}
        Ok(removed) => logging::info(&format!(
            "dictation history: deleted {removed} day file(s) older than {retention_days} days"
        )),
        Err(error) => logging::warn(&format!(
            "dictation history: clean-up of {} failed: {error}",
            dir.display()
        )),
    }
}

/// Delete day files more than `retention_days` before `today`; 0 keeps all.
pub(crate) fn prune(dir: &Path, retention_days: u32, today: i64) -> io::Result<usize> {
    if retention_days == 0 {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(day) = entry.file_name().to_str().and_then(parse_day_file) else {
            continue;
        };
        if today - day > i64::from(retention_days) {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sunoto-history-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn recorder(settings: &Settings) -> (HistoryRecorder, mpsc::Receiver<Box<Record>>) {
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        (HistoryRecorder::new(Some(tx), settings), rx)
    }

    fn audio() -> SessionAudioStats {
        let mut stats = SessionAudioStats::default();
        stats.observe(&[1000, -1000, 500, -500].repeat(4000));
        stats
    }

    #[test]
    fn dates_round_trip_across_month_ends_leap_days_and_years() {
        for (days, expected) in [
            (0, (1970, 1, 1)),
            (20_742, (2026, 10, 16)),
            (20_818, (2026, 12, 31)),
            (20_819, (2027, 1, 1)),
            (21_242, (2028, 2, 28)),
            (21_243, (2028, 2, 29)),
            (21_244, (2028, 3, 1)),
            (-1, (1969, 12, 31)),
        ] {
            assert_eq!(civil_from_days(days), expected, "day {days}");
            assert_eq!(
                days_from_civil(expected.0, expected.1, expected.2),
                days,
                "{expected:?}"
            );
        }
    }

    #[test]
    fn day_files_are_named_in_utc_and_parse_back() {
        // 2026-10-16 23:59:59.999Z and one millisecond later.
        let last_ms = 20_743 * DAY_MS - 1;
        assert_eq!(day_file_name(last_ms), "2026-10-16.jsonl");
        assert_eq!(day_file_name(last_ms + 1), "2026-10-17.jsonl");
        assert_eq!(format_utc(last_ms), "2026-10-16 23:59Z");
        assert_eq!(parse_day_file("2026-10-16.jsonl"), Some(20_742));
        for name in [
            "2026-13-01.jsonl",
            "2026-10-16.json",
            "notes.jsonl",
            "2026-1-16.jsonl",
        ] {
            assert_eq!(parse_day_file(name), None, "{name}");
        }
    }

    #[test]
    fn inserted_dictation_records_every_stage() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);
        history.begin(7);
        history.target(Some(&("com.apple.TextEdit".into(), "TextEdit".into())));
        history.transcript(
            7,
            "Um, send it Tuesday, actually Wednesday.",
            Some(&audio()),
        );
        let trace = [StageTrace {
            stage: "fillers",
            before: "Um, send it Wednesday.".into(),
            after: "Send it Wednesday.".into(),
        }];
        history.polished(7, StyleProfile::Prose, &trace, "Send it Wednesday.");
        history.inserted(7, "Send it Wednesday.", false);
        let timing = TimingBreakdown {
            recorded: 2000,
            insert: Some(30),
            release_to_insertion: Some(280),
            ..TimingBreakdown::default()
        };
        history.finish(7, Status::Inserted, None, Some("pasted"), Some(timing));
        let record = rx.try_recv().expect("record queued");
        assert_eq!(record.status, Status::Inserted);
        assert!(record.id.ends_with("-7"));
        assert_eq!(record.target.as_ref().unwrap().name, "TextEdit");
        assert_eq!(record.sound.unwrap().ms, 1000);
        assert_eq!(record.text.inserted.as_deref(), Some("Send it Wednesday."));
        assert_eq!(record.polish_trace.len(), 1);

        let json: serde_json::Value = serde_json::to_value(&*record).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["type"], "dictation");
        assert_eq!(json["status"], "inserted");
        assert_eq!(json["style"], "prose");
        assert_eq!(json["llm"]["status"], "off");
        assert_eq!(json["timing_ms"]["release_to_insertion"], 280);
        assert_eq!(json["insertion"]["method"], "pasted");
        assert_eq!(json["insertion"]["streamed"], false);
        assert!(json.get("redacted").is_none());
        assert!(history.draft.is_none());
    }

    #[test]
    fn password_fields_and_excluded_apps_keep_no_text() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);

        history.begin(1);
        history.target(Some(&("com.apple.Safari".into(), "Safari".into())));
        history.transcript(1, "my secret", None);
        history.polished(1, StyleProfile::Prose, &[], "My secret.");
        history.inserted(1, "My secret.", false);
        history.finish(1, Status::SecureField, None, None, None);
        let record = rx.try_recv().unwrap();
        assert!(record.redacted);
        assert_eq!(record.text, Texts::default());
        assert!(!serde_json::to_string(&*record).unwrap().contains("secret"));

        history.begin(2);
        history.target(Some(&(
            "com.1password.1password".into(),
            "1Password".into(),
        )));
        history.transcript(2, "vault words", None);
        history.inserted(2, "Vault words.", true);
        history.finish(2, Status::Inserted, None, None, None);
        let record = rx.try_recv().unwrap();
        assert!(record.redacted);
        assert_eq!(record.status, Status::Inserted);
        assert!(
            !serde_json::to_string(&*record)
                .unwrap()
                .contains("ault words")
        );
    }

    #[test]
    fn recent_dictations_follow_what_was_saved_with_text() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);
        assert!(history.recent().is_empty());
        assert!(!history.take_recent_changed());

        history.begin(1);
        history.transcript(1, "hello there", None);
        history.inserted(1, "Hello there.", false);
        history.finish(1, Status::Inserted, None, None, None);
        history.begin(2);
        history.transcript(2, "my secret", None);
        history.inserted(2, "My secret.", false);
        history.finish(2, Status::SecureField, None, None, None);
        history.begin(3);
        history.finish(3, Status::Cancelled, None, None, None);

        let texts: Vec<String> = history
            .recent()
            .into_iter()
            .map(|entry| entry.text)
            .collect();
        assert_eq!(texts, vec!["Hello there.".to_string()]);
        assert!(history.take_recent_changed());
        assert!(!history.take_recent_changed());

        for session in 10..10 + RECENT_COUNT as u64 + 2 {
            history.begin(session);
            history.inserted(session, &format!("text {session}"), false);
            history.finish(session, Status::Inserted, None, None, None);
            let _ = rx.try_recv();
        }
        let recent = history.recent();
        assert_eq!(recent.len(), RECENT_COUNT);
        assert_eq!(recent[0].text, format!("text {}", 10 + RECENT_COUNT + 1));
    }

    #[test]
    fn each_session_produces_exactly_one_record() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);

        // Cancelled while recording.
        history.begin(1);
        history.finish(1, Status::Cancelled, None, None, None);
        // A finish for another session never touches the open draft.
        history.begin(2);
        history.finish(99, Status::Inserted, None, None, None);
        history.transcript(2, "", None);
        history.finish(2, Status::EmptyTranscript, None, None, None);
        history.finish(2, Status::Inserted, None, None, None);

        let statuses: Vec<_> = rx.try_iter().map(|record| record.status).collect();
        assert_eq!(statuses, [Status::Cancelled, Status::EmptyTranscript]);
    }

    #[test]
    fn a_new_press_writes_an_open_draft_as_abandoned() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);
        history.begin(1);
        history.transcript(1, "hello", None);
        history.begin(2);
        let record = rx.try_recv().unwrap();
        assert_eq!(record.status, Status::Abandoned);
        assert!(record.id.ends_with("-1"));
        assert_eq!(history.draft.as_ref().unwrap().session_id, 2);
    }

    #[test]
    fn reap_ends_a_session_lost_before_its_transcript() {
        let settings = Settings::default();
        let (mut history, rx) = recorder(&settings);
        history.begin(3);
        history.reap(Some(3));
        assert!(rx.try_recv().is_err(), "still recording");
        history.reap(None);
        let record = rx.try_recv().unwrap();
        assert_eq!(record.status, Status::AsrError);

        // After the transcript, only an explicit finish ends it.
        history.begin(4);
        history.transcript(4, "hello", None);
        history.reap(None);
        assert!(rx.try_recv().is_err());
        assert!(history.draft.is_some());
    }

    #[test]
    fn a_full_writer_drops_records_without_blocking() {
        let settings = Settings::default();
        let (tx, rx) = mpsc::sync_channel(1);
        let mut history = HistoryRecorder::new(Some(tx), &settings);
        for session in 1..=3 {
            history.begin(session);
            history.finish(session, Status::Cancelled, None, None, None);
        }
        assert_eq!(history.dropped, 2);
        assert_eq!(rx.try_iter().count(), 1);
        assert!(history.is_enabled());
    }

    #[test]
    fn disabled_history_records_nothing() {
        let settings = Settings {
            history_enabled: false,
            ..Settings::default()
        };
        let mut history = HistoryRecorder::start(&settings);
        assert!(!history.is_enabled());
        history.begin(1);
        history.transcript(1, "hello", None);
        assert!(history.draft.is_none());
    }

    #[test]
    fn records_append_to_private_day_files() {
        let root = temp_dir("append");
        let dir = prepare_dir(&root).unwrap();
        let asr = Asr {
            backend: "mock".into(),
            model: None,
            profile_ms: 160,
        };
        let mut record = Record::new(1, 20_742 * DAY_MS + 1_000, asr);
        record.status = Status::Cancelled;
        append_record(&dir, &record).unwrap();
        record.id = "second".into();
        append_record(&dir, &record).unwrap();

        let path = dir.join("2026-10-16.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["id"], "second");
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&root), 0o700);
        fs::remove_dir_all(&root).unwrap();
    }

    /// What history costs the event loop per dictation: filling the draft
    /// and handing it over. `cargo test -p sunoto-daemon --offline
    /// history_overhead -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn history_overhead_per_dictation() {
        let settings = Settings::default();
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let mut history = HistoryRecorder::new(Some(tx), &settings);
        let stats = audio();
        let trace = [StageTrace {
            stage: "corrections",
            before: "Book it for Friday, I mean Saturday.".into(),
            after: "Book it for Saturday.".into(),
        }];
        let class = ("com.apple.TextEdit".to_string(), "TextEdit".to_string());
        let rounds = 20_000u64;
        let started = Instant::now();
        for session in 0..rounds {
            history.begin(session);
            history.target(Some(&class));
            history.transcript(session, &trace[0].before, Some(&stats));
            history.polished(session, StyleProfile::Prose, &trace, &trace[0].after);
            history.inserted(session, &trace[0].after, false);
            let timing = TimingBreakdown {
                insert: Some(5),
                ..TimingBreakdown::default()
            };
            history.finish(
                session,
                Status::Inserted,
                None,
                Some("pasted"),
                Some(timing),
            );
            rx.try_recv().unwrap();
        }
        let per = started.elapsed() / rounds as u32;
        println!("history work on the event loop: {per:?} per dictation");
        assert!(per < Duration::from_millis(1));
    }

    #[test]
    fn the_writer_thread_saves_finished_dictations() {
        let root = temp_dir("writer");
        let settings = Settings::default();
        let writer = spawn_writer(&root, settings.history_retention_days).unwrap();
        let mut history = HistoryRecorder::new(Some(writer), &settings);
        history.begin(5);
        history.transcript(5, "hello there", None);
        history.inserted(5, "Hello there.", false);
        history.finish(5, Status::Inserted, None, Some("typed"), None);

        let path = dictations_dir(&root).join(day_file_name(now_ms()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let saved: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(saved["text"]["inserted"], "Hello there.");
        assert_eq!(saved["insertion"]["method"], "typed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn prune_deletes_only_day_files_past_the_limit() {
        let root = temp_dir("prune");
        let dir = prepare_dir(&root).unwrap();
        for name in [
            "2026-07-17.jsonl", // 91 days before 2026-10-16
            "2026-07-18.jsonl", // 90 days before: kept
            "2026-10-16.jsonl",
            "notes.txt",
        ] {
            fs::write(dir.join(name), "{}\n").unwrap();
        }
        assert_eq!(prune(&dir, 0, 20_742).unwrap(), 0);
        assert_eq!(prune(&dir, 90, 20_742).unwrap(), 1);
        let mut left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["2026-07-18.jsonl", "2026-10-16.jsonl", "notes.txt"]);
        fs::remove_dir_all(&root).unwrap();
    }
}

//! `sunoto-daemon history ...`: read and manage the local dictation history
//! (see history.rs). Works on the day files directly, so the daemon does not
//! need to be running.

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::history::{dictations_dir, format_utc, history_dir, parse_day_file};
use crate::settings::{self, Settings};

const USAGE: &str = "usage: sunoto-daemon history status | list [-n N] | show ID|last | forget ID|last | clear [--yes]";
const LIST_DEFAULT: usize = 20;
const LIST_TEXT_CHARS: usize = 60;

pub fn run_cli(args: &[String]) -> Result<(), Box<dyn Error>> {
    let dir = dictations_dir(&history_dir());
    match args.first().map(String::as_str) {
        Some("status") | None => status(&dir),
        Some("list") => {
            let count = match args.get(1).map(String::as_str) {
                Some("-n") => args
                    .get(2)
                    .ok_or(USAGE)?
                    .parse()
                    .map_err(|_| "-n needs a number")?,
                Some(_) => return Err(USAGE.into()),
                None => LIST_DEFAULT,
            };
            list(&dir, count)
        }
        Some("show") if args.len() == 2 => show(&dir, &args[1]),
        Some("forget") if args.len() == 2 => {
            let id = resolve_id(&dir, &args[1])?;
            if forget(&dir, &id)? {
                println!("forgot {id}");
                Ok(())
            } else {
                Err(format!("no dictation {id}").into())
            }
        }
        Some("clear") => {
            let entries = read_entries(&dir)?;
            let dictations = entries.iter().filter(|value| is_dictation(value)).count();
            if !args.iter().any(|arg| arg == "--yes") {
                return Err(format!(
                    "this deletes all {dictations} dictation(s) in {}; run `sunoto-daemon history clear --yes` to confirm",
                    dir.display()
                )
                .into());
            }
            let removed = clear(&dir)?;
            println!("deleted {dictations} dictation(s) in {removed} day file(s)");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn is_dictation(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("dictation")
}

fn day_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(parse_day_file)
                    .is_some()
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    files.sort();
    Ok(files)
}

/// Every parseable line, oldest first. A partial last line (a crash mid
/// write) or a line from a newer, unknown format is skipped, never fatal.
fn read_entries(dir: &Path) -> io::Result<Vec<Value>> {
    let mut entries = Vec::new();
    for file in day_files(dir)? {
        for line in fs::read_to_string(&file)?.lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && value.is_object()
            {
                entries.push(value);
            }
        }
    }
    Ok(entries)
}

fn resolve_id(dir: &Path, requested: &str) -> Result<String, Box<dyn Error>> {
    if requested != "last" {
        return Ok(requested.to_string());
    }
    read_entries(dir)?
        .iter()
        .rev()
        .find(|value| is_dictation(value))
        .and_then(|value| value.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| "the history is empty".into())
}

fn status(dir: &Path) -> Result<(), Box<dyn Error>> {
    let settings = Settings::load(&settings::config_path())?;
    let entries = read_entries(dir)?;
    let dictations = entries.iter().filter(|value| is_dictation(value)).count();
    let files = day_files(dir)?;
    let bytes: u64 = files
        .iter()
        .filter_map(|file| fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    if settings.history_enabled {
        println!("history:      on");
    } else {
        println!("history:      off (set \"history_enabled\": true in the config to turn it on)");
    }
    match settings.history_retention_days {
        0 => println!("kept:         until deleted"),
        days => println!("kept:         {days} days"),
    }
    println!("folder:       {}", dir.display());
    println!("dictations:   {dictations}");
    let name = |path: Option<&PathBuf>| {
        path.and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
    };
    if let (Some(first), Some(last)) = (name(files.first()), name(files.last())) {
        println!("days:         {} ({first} to {last})", files.len());
    }
    println!("size:         {:.1} KB", bytes as f64 / 1024.0);
    Ok(())
}

/// One line per dictation: when, id, outcome, app, release-to-insertion,
/// and the start of the inserted text.
fn list_line(value: &Value) -> String {
    let ts = value.get("ts").and_then(Value::as_u64).unwrap_or_default();
    let id = value.get("id").and_then(Value::as_str).unwrap_or("?");
    let status = value.get("status").and_then(Value::as_str).unwrap_or("?");
    let app = value
        .pointer("/target/name")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let ms = value
        .pointer("/timing_ms/release_to_insertion")
        .and_then(Value::as_u64)
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "-".to_string());
    let text = if value.get("redacted").and_then(Value::as_bool) == Some(true) {
        "(text not saved)".to_string()
    } else if status == "empty_transcript" {
        "(nothing heard)".to_string()
    } else {
        ["/text/inserted", "/text/raw"]
            .iter()
            .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .map(|text| format!("{:?}", shorten(text, LIST_TEXT_CHARS)))
            .unwrap_or_default()
    };
    format!(
        "{}  {id:<18} {status:<16} {:<14} {ms:>8}  {text}",
        format_utc(ts),
        shorten(app, 14)
    )
    .trim_end()
    .to_string()
}

fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn list(dir: &Path, count: usize) -> Result<(), Box<dyn Error>> {
    let entries = read_entries(dir)?;
    let dictations: Vec<&Value> = entries.iter().filter(|value| is_dictation(value)).collect();
    if dictations.is_empty() {
        println!("no dictations recorded in {}", dir.display());
        return Ok(());
    }
    for value in &dictations[dictations.len().saturating_sub(count)..] {
        println!("{}", list_line(value));
    }
    Ok(())
}

fn show(dir: &Path, requested: &str) -> Result<(), Box<dyn Error>> {
    let id = resolve_id(dir, requested)?;
    let entries = read_entries(dir)?;
    let matching: Vec<&Value> = entries
        .iter()
        .filter(|value| value.get("id").and_then(Value::as_str) == Some(id.as_str()))
        .collect();
    if matching.is_empty() {
        return Err(format!("no dictation {id}").into());
    }
    // The dictation itself, then any later notes about it (fixes, clues).
    for value in matching {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// Remove every line about `id` from the day file that holds it, through a
/// temporary file and a rename. An emptied day file is deleted. A dictation
/// the daemon appends to the same file during the rewrite can be lost; the
/// window is a few milliseconds of a manual command.
fn forget(dir: &Path, id: &str) -> io::Result<bool> {
    let mut found = false;
    for file in day_files(dir)? {
        let contents = fs::read_to_string(&file)?;
        let kept: Vec<&str> = contents
            .lines()
            .filter(|line| {
                let about_id = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_string))
                    .is_some_and(|line_id| line_id == id);
                !about_id
            })
            .collect();
        if kept.len() == contents.lines().count() {
            continue;
        }
        found = true;
        if kept.is_empty() {
            fs::remove_file(&file)?;
            continue;
        }
        let temp = file.with_extension("jsonl.tmp");
        {
            let mut out = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&temp)?;
            for line in kept {
                out.write_all(line.as_bytes())?;
                out.write_all(b"\n")?;
            }
            out.sync_all()?;
        }
        fs::rename(&temp, &file)?;
    }
    Ok(found)
}

/// Delete every day file; returns how many.
fn clear(dir: &Path) -> io::Result<usize> {
    let files = day_files(dir)?;
    for file in &files {
        fs::remove_file(file)?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sunoto-history-cli-{name}-{}-{}",
            std::process::id(),
            crate::history::now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn line(id: &str, ts: u64, text: &str) -> String {
        serde_json::json!({
            "v": 1, "type": "dictation", "id": id, "ts": ts, "status": "inserted",
            "target": {"id": "com.apple.TextEdit", "name": "TextEdit"},
            "text": {"raw": text, "inserted": text},
            "timing_ms": {"release_to_insertion": 284},
        })
        .to_string()
    }

    #[test]
    fn forget_removes_exactly_one_dictation() {
        let dir = temp_dir("forget");
        let day = dir.join("2026-10-16.jsonl");
        let note = r#"{"type":"correction","id":"b","text":"fixed"}"#;
        fs::write(
            &day,
            format!(
                "{}\n{}\n{note}\n{}\n",
                line("a", 1, "one"),
                line("b", 2, "two"),
                line("c", 3, "three")
            ),
        )
        .unwrap();
        fs::write(
            dir.join("2026-10-15.jsonl"),
            format!("{}\n", line("z", 0, "x")),
        )
        .unwrap();

        assert!(forget(&dir, "b").unwrap());
        assert!(!forget(&dir, "missing").unwrap());
        let ids: Vec<String> = read_entries(&dir)
            .unwrap()
            .iter()
            .map(|value| value["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, ["z", "a", "c"]);
        assert_eq!(
            fs::metadata(&day).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // Forgetting the only dictation of a day removes the file.
        assert!(forget(&dir, "z").unwrap());
        assert!(!dir.join("2026-10-15.jsonl").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_lines_and_other_files_are_skipped() {
        let dir = temp_dir("skip");
        fs::write(
            dir.join("2026-10-16.jsonl"),
            format!("{}\n{{\"v\":1,\"type\":\"dicta", line("a", 1, "one")),
        )
        .unwrap();
        fs::write(dir.join("notes.txt"), "not history\n").unwrap();
        let entries = read_entries(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(resolve_id(&dir, "last").unwrap(), "a");
        assert_eq!(clear(&dir).unwrap(), 1);
        assert!(dir.join("notes.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_lines_show_outcome_app_time_and_text() {
        let value: Value = serde_json::from_str(&line(
            "19a0f1c2b3d-42",
            20_742 * 86_400_000 + 38_520_000,
            "Send the email to priya on Wednesday.",
        ))
        .unwrap();
        let line = list_line(&value);
        assert!(
            line.starts_with("2026-10-16 10:42Z  19a0f1c2b3d-42"),
            "{line}"
        );
        assert!(line.contains("inserted"));
        assert!(line.contains("TextEdit"));
        assert!(line.contains("284 ms"));
        assert!(line.contains("\"Send the email to priya on Wednesday.\""));

        let redacted =
            serde_json::json!({"type": "dictation", "redacted": true, "status": "secure_field"});
        assert!(list_line(&redacted).ends_with("(text not saved)"));
        assert_eq!(shorten("abcdef", 4), "abc…");
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

/// Timestamped structured-ish logging to stderr; stdout stays free for
/// machine-readable command output (bench JSON, check results).
pub fn log(level: &str, message: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds_of_day = now.as_secs() % 86_400;
    eprintln!(
        "[{:02}:{:02}:{:02}.{:03}Z] [{level}] {message}",
        seconds_of_day / 3600,
        seconds_of_day % 3600 / 60,
        seconds_of_day % 60,
        now.subsec_millis(),
    );
}

pub fn info(message: &str) {
    log("info", message);
}

pub fn warn(message: &str) {
    log("warn", message);
}

pub fn error(message: &str) {
    log("error", message);
}

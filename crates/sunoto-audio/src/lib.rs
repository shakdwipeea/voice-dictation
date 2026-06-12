//! Persistent PulseAudio microphone capture (Phase 1).
//!
//! A capture thread keeps a `parec` child process streaming raw 16 kHz mono
//! s16le PCM from the configured source. When the child exits (mic unplugged,
//! PulseAudio restart, ...) the thread emits [`AudioEvent::Stopped`], waits a
//! backoff, and respawns. A device configured as "auto" is re-resolved on
//! every (re)start so a reconnected microphone is picked up.

use std::fmt;
use std::io::{self, Read};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SAMPLE_RATE_HZ: usize = 16_000;
const BYTES_PER_SAMPLE: usize = 2;
// stop() must be able to interrupt a pending restart backoff promptly, so
// backoff sleeps are sliced into chunks no longer than this.
const STOP_POLL_MS: u64 = 25;

#[derive(Debug, Clone, PartialEq)]
pub struct CaptureConfig {
    /// "auto" or a PulseAudio source name.
    pub device: String,
    pub frame_ms: u32,
    /// Command prefix for `parec`; the standard capture arguments are
    /// appended after these elements. Tests override with a fake.
    pub parec_program: Vec<String>,
    /// Command prefix for `pactl`; subcommand arguments are appended.
    pub pactl_program: Vec<String>,
    /// Delays between respawns; the last value repeats for later attempts.
    pub restart_backoff_ms: Vec<u64>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device: "auto".to_string(),
            frame_ms: 20,
            parec_program: vec!["parec".to_string()],
            pactl_program: vec!["pactl".to_string()],
            restart_backoff_ms: vec![250, 500, 1000, 2000, 4000],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    Started {
        device: String,
        description: Option<String>,
    },
    /// Exactly `frame_ms` worth of samples: 16000 * frame_ms / 1000.
    Frame(Vec<i16>),
    /// The capture process ended; a restart follows unless stopping.
    Stopped {
        reason: String,
    },
}

#[derive(Debug)]
pub enum AudioError {
    NoMicrophone,
    Spawn(std::io::Error),
    Resolve(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoMicrophone => {
                f.write_str("no physical microphone source is available")
            }
            AudioError::Spawn(error) => {
                write!(f, "failed to spawn capture command: {error}")
            }
            AudioError::Resolve(message) => {
                write!(f, "failed to resolve audio source: {message}")
            }
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AudioError::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

/// Pick a physical (non-monitor) PulseAudio source, preferring the default.
pub fn resolve_source(pactl_program: &[String]) -> Result<String, AudioError> {
    let default = run_pactl(pactl_program, &["get-default-source"])?;
    let default = default.trim();
    if !default.is_empty() && !default.ends_with(".monitor") {
        return Ok(default.to_string());
    }
    // Monitors mirror playback streams; only a non-monitor source is a mic.
    let listing = run_pactl(pactl_program, &["list", "short", "sources"])?;
    for line in listing.lines() {
        if let Some(name) = line.split('\t').nth(1) {
            if !name.is_empty() && !name.ends_with(".monitor") {
                return Ok(name.to_string());
            }
        }
    }
    Err(AudioError::NoMicrophone)
}

/// Return PulseAudio's human-readable description for a source name.
pub fn source_description(
    pactl_program: &[String],
    device: &str,
) -> Result<Option<String>, AudioError> {
    let listing = run_pactl(pactl_program, &["list", "sources"])?;
    Ok(parse_source_description(&listing, device))
}

fn parse_source_description(listing: &str, device: &str) -> Option<String> {
    let mut matching_source = false;
    for line in listing.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Name:") {
            matching_source = name.trim() == device;
        } else if matching_source {
            if let Some(description) = line.strip_prefix("Description:") {
                let description = description.trim();
                return (!description.is_empty()).then(|| description.to_string());
            }
        }
    }
    None
}

fn run_pactl(pactl_program: &[String], args: &[&str]) -> Result<String, AudioError> {
    let (program, leading) = pactl_program
        .split_first()
        .ok_or_else(|| AudioError::Resolve("pactl_program is empty".to_string()))?;
    let output = Command::new(program)
        .args(leading)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(AudioError::Spawn)?;
    if !output.status.success() {
        return Err(AudioError::Resolve(format!(
            "{program} {} failed: {}",
            args.join(" "),
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn resolve_configured_device(config: &CaptureConfig) -> Result<String, AudioError> {
    if config.device == "auto" {
        resolve_source(&config.pactl_program)
    } else {
        Ok(config.device.clone())
    }
}

fn spawn_parec(config: &CaptureConfig, device: &str) -> Result<Child, AudioError> {
    let (program, leading) = config
        .parec_program
        .split_first()
        .ok_or_else(|| AudioError::Resolve("parec_program is empty".to_string()))?;
    Command::new(program)
        .args(leading)
        .arg(format!("--device={device}"))
        .args([
            "--rate=16000",
            "--format=s16le",
            "--channels=1",
            "--latency-msec=20",
            "--process-time-msec=20",
            "--raw",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(AudioError::Spawn)
}

#[derive(Debug)]
pub struct CaptureHandle {
    events: Receiver<AudioEvent>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureHandle {
    pub fn events(&self) -> &Receiver<AudioEvent> {
        &self.events
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        // Kill the child so the capture thread's blocking read unblocks; the
        // capture thread itself wait()s the child to avoid leaving a zombie.
        if let Some(child) = lock_child(&self.child).as_mut() {
            let _ = child.kill();
        }
        let _ = thread.join();
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn start_capture(config: CaptureConfig) -> Result<CaptureHandle, AudioError> {
    if config.frame_ms == 0 {
        return Err(AudioError::Resolve("frame_ms must be at least 1".to_string()));
    }
    // Resolve and spawn synchronously so a missing mic or broken command
    // surfaces as an error here instead of a silent retry loop.
    let device = resolve_configured_device(&config)?;
    let child = spawn_parec(&config, &device)?;
    let stop = Arc::new(AtomicBool::new(false));
    let slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::channel();
    let thread = {
        let stop = Arc::clone(&stop);
        let slot = Arc::clone(&slot);
        thread::spawn(move || capture_loop(config, (child, device), tx, &stop, &slot))
    };
    Ok(CaptureHandle {
        events: rx,
        stop,
        child: slot,
        thread: Some(thread),
    })
}

fn capture_loop(
    config: CaptureConfig,
    first: (Child, String),
    tx: Sender<AudioEvent>,
    stop: &AtomicBool,
    slot: &Mutex<Option<Child>>,
) {
    let frame_bytes = frame_byte_len(config.frame_ms);
    let mut backoff_index = 0usize;
    let mut pending = Some(first);
    while !stop.load(Ordering::SeqCst) {
        let acquired = match pending.take() {
            Some(ready) => Some(ready),
            None => resolve_configured_device(&config)
                .and_then(|device| spawn_parec(&config, &device).map(|child| (child, device)))
                .ok(),
        };
        let Some((mut child, device)) = acquired else {
            // Failed respawn (mic still unplugged, pactl error, ...): the
            // stream already reported Stopped, so just retry after a backoff.
            sleep_unless_stopped(stop, next_backoff(&config.restart_backoff_ms, &mut backoff_index));
            continue;
        };
        let mut stdout = child.stdout.take();
        *lock_child(slot) = Some(child);
        // stop() may have scanned an empty slot just before the child landed
        // in it; re-check so the cleanup below reaps the new child.
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let description = source_description(&config.pactl_program, &device)
            .ok()
            .flatten();
        if !send_event(
            &tx,
            stop,
            AudioEvent::Started {
                device,
                description,
            },
        ) {
            break;
        }
        let read_result = match stdout.as_mut() {
            Some(out) => pump_frames(out, frame_bytes, &tx, stop, &mut backoff_index),
            None => Err(io::Error::other("parec stdout was not captured")),
        };
        drop(stdout);
        let exit_reason = reap(lock_child(slot).take());
        let reason = match read_result {
            Ok(()) => exit_reason,
            Err(error) => format!("read error: {error}; {exit_reason}"),
        };
        if !send_event(&tx, stop, AudioEvent::Stopped { reason }) {
            break;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        sleep_unless_stopped(stop, next_backoff(&config.restart_backoff_ms, &mut backoff_index));
    }
    // Never leak a child: reap whatever is still tracked.
    if let Some((child, _)) = pending.take() {
        reap(Some(child));
    }
    reap(lock_child(slot).take());
}

/// Read frame-sized chunks until EOF or error; a short final read is dropped.
fn pump_frames(
    out: &mut ChildStdout,
    frame_bytes: usize,
    tx: &Sender<AudioEvent>,
    stop: &AtomicBool,
    backoff_index: &mut usize,
) -> io::Result<()> {
    let mut buf = vec![0u8; frame_bytes];
    let mut delivered = false;
    loop {
        match out.read_exact(&mut buf) {
            Ok(()) => {
                if !delivered {
                    delivered = true;
                    // A stream that produced audio counts as a recovery, so
                    // the next failure starts from the shortest backoff.
                    *backoff_index = 0;
                }
                let frame = buf
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                if !send_event(tx, stop, AudioEvent::Frame(frame)) {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn send_event(tx: &Sender<AudioEvent>, stop: &AtomicBool, event: AudioEvent) -> bool {
    if tx.send(event).is_err() {
        // The receiver is gone, so nothing can observe events anymore; treat
        // it as a stop request instead of panicking the capture thread.
        stop.store(true, Ordering::SeqCst);
        return false;
    }
    true
}

/// Kill (in case it is still running) and wait the child so no zombie remains.
fn reap(child: Option<Child>) -> String {
    let Some(mut child) = child else {
        return "capture process already reaped".to_string();
    };
    let _ = child.kill();
    match child.wait() {
        Ok(status) => format!("parec exited: {status}"),
        Err(error) => format!("wait for parec failed: {error}"),
    }
}

fn next_backoff(backoff_ms: &[u64], index: &mut usize) -> u64 {
    let delay = backoff_ms
        .get(*index)
        .or_else(|| backoff_ms.last())
        .copied()
        .unwrap_or(0);
    *index = (*index + 1).min(backoff_ms.len().saturating_sub(1));
    delay
}

fn sleep_unless_stopped(stop: &AtomicBool, total_ms: u64) {
    let mut remaining_ms = total_ms;
    while remaining_ms > 0 && !stop.load(Ordering::SeqCst) {
        let slice_ms = remaining_ms.min(STOP_POLL_MS);
        thread::sleep(Duration::from_millis(slice_ms));
        remaining_ms -= slice_ms;
    }
}

fn frame_byte_len(frame_ms: u32) -> usize {
    SAMPLE_RATE_HZ * BYTES_PER_SAMPLE * frame_ms as usize / 1000
}

fn lock_child(slot: &Mutex<Option<Child>>) -> MutexGuard<'_, Option<Child>> {
    // A poisoned lock only means another thread panicked mid-update; the
    // Option<Child> inside is still structurally valid.
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), script.to_string()]
    }

    fn py(script: &str) -> Vec<String> {
        vec!["python3".to_string(), "-c".to_string(), script.to_string()]
    }

    /// Fake pactl. The appended subcommand lands in `$0` because the override
    /// elements come first, so the script branches on it.
    fn fake_pactl(default_source: &str, listing: &str) -> Vec<String> {
        sh(&format!(
            "if [ \"$0\" = get-default-source ]; then printf '%s' '{default_source}'; \
             else printf '%s' '{listing}'; fi"
        ))
    }

    fn test_config(parec_program: Vec<String>) -> CaptureConfig {
        CaptureConfig {
            device: "test-device".to_string(),
            parec_program,
            // The explicit device above means pactl must never run.
            pactl_program: vec!["false".to_string()],
            restart_backoff_ms: vec![10],
            ..CaptureConfig::default()
        }
    }

    fn expect_event(events: &Receiver<AudioEvent>) -> AudioEvent {
        events
            .recv_timeout(RECV_TIMEOUT)
            .expect("timed out waiting for audio event")
    }

    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sunoto-audio-test-{tag}-{}-{count}",
            std::process::id()
        ))
    }

    #[test]
    fn default_config_matches_phase0_capture_settings() {
        let config = CaptureConfig::default();
        assert_eq!(config.device, "auto");
        assert_eq!(config.frame_ms, 20);
        assert_eq!(config.parec_program, vec!["parec".to_string()]);
        assert_eq!(config.pactl_program, vec!["pactl".to_string()]);
        assert_eq!(config.restart_backoff_ms, vec![250, 500, 1000, 2000, 4000]);
    }

    #[test]
    fn errors_render_with_display_and_source() {
        assert_eq!(
            AudioError::NoMicrophone.to_string(),
            "no physical microphone source is available"
        );
        let spawn = AudioError::Spawn(io::Error::other("boom"));
        assert!(spawn.to_string().contains("boom"));
        assert!(std::error::Error::source(&spawn).is_some());
        let resolve = AudioError::Resolve("bad output".to_string());
        assert!(resolve.to_string().contains("bad output"));
        assert!(std::error::Error::source(&resolve).is_none());
    }

    #[test]
    fn resolve_uses_non_monitor_default_source() {
        let pactl = fake_pactl("alsa_input.usb_mic\n", "");
        assert_eq!(resolve_source(&pactl).unwrap(), "alsa_input.usb_mic");
    }

    #[test]
    fn resolve_skips_monitor_default_and_uses_source_list() {
        let listing = "0\talsa_output.pci.monitor\tmodule-alsa-card.c\n\
                       1\talsa_input.usb_mic\tmodule-alsa-card.c\n";
        let pactl = fake_pactl("alsa_output.pci.monitor\n", listing);
        assert_eq!(resolve_source(&pactl).unwrap(), "alsa_input.usb_mic");
    }

    #[test]
    fn resolve_reports_no_microphone_when_only_monitors_exist() {
        let listing = "0\talsa_output.pci.monitor\tmodule-alsa-card.c\n";
        let pactl = fake_pactl("", listing);
        assert!(matches!(
            resolve_source(&pactl),
            Err(AudioError::NoMicrophone)
        ));
    }

    #[test]
    fn resolve_errors_when_pactl_is_missing() {
        let pactl = vec!["/nonexistent/sunoto-test-pactl".to_string()];
        assert!(matches!(resolve_source(&pactl), Err(AudioError::Spawn(_))));
    }

    #[test]
    fn source_description_matches_the_named_source() {
        let listing = "Source #1\n\
            Name: alsa_input.builtin\n\
            Description: Built-in Audio Analog Stereo\n\
            Properties:\n\
                node.name = \"alsa_input.builtin\"\n\
            Source #2\n\
            Name: alsa_input.usb_mic\n\
            Description: Webcam C270 Mono\n";
        assert_eq!(
            parse_source_description(listing, "alsa_input.usb_mic"),
            Some("Webcam C270 Mono".to_string())
        );
        assert_eq!(parse_source_description(listing, "missing"), None);
    }

    #[test]
    fn frames_decode_little_endian_in_exact_sizes() {
        // Two 20 ms frames (320 samples each) as a signed ramp; the negative
        // half exercises little-endian sign handling.
        let script =
            "import struct,sys\nsys.stdout.buffer.write(struct.pack('<640h',*range(-320,320)))";
        let handle = start_capture(test_config(py(script))).unwrap();
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Started {
                device: "test-device".to_string(),
                description: None,
            }
        );
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Frame((-320..0).collect())
        );
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Frame((0..320).collect())
        );
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Stopped { .. }
        ));
        handle.stop();
    }

    #[test]
    fn partial_trailing_frame_is_dropped_at_eof() {
        // One full frame plus 10 stray bytes that must never become a Frame.
        let script = "import struct,sys\n\
            sys.stdout.buffer.write(struct.pack('<320h',*range(320))+b'\\x7f'*10)";
        let handle = start_capture(test_config(py(script))).unwrap();
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Started { .. }
        ));
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Frame((0..320).collect())
        );
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Stopped { .. }
        ));
        handle.stop();
    }

    #[test]
    fn capture_restarts_after_child_exit() {
        let marker = unique_temp_path("restart");
        let _ = fs::remove_file(&marker);
        // First run creates the marker and exits without output; later runs
        // stream one full frame.
        let script = format!(
            "import os,struct,sys\np={:?}\nif not os.path.exists(p):\n    open(p,'w').close()\n    sys.exit(0)\nsys.stdout.buffer.write(struct.pack('<320h',*range(320)))",
            marker.to_string_lossy()
        );
        let handle = start_capture(test_config(py(&script))).unwrap();
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Started { .. }
        ));
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Stopped { .. }
        ));
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Started { .. }
        ));
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Frame((0..320).collect())
        );
        handle.stop();
        let _ = fs::remove_file(&marker);
    }

    #[test]
    fn auto_device_is_resolved_with_pactl() {
        let config = CaptureConfig {
            device: "auto".to_string(),
            // `exec` so killing the child kills the writer itself, not a shell
            // parent that would orphan it with the pipe still open.
            parec_program: sh("exec cat /dev/zero"),
            pactl_program: fake_pactl("fake-mic\n", ""),
            restart_backoff_ms: vec![10],
            ..CaptureConfig::default()
        };
        let handle = start_capture(config).unwrap();
        assert_eq!(
            expect_event(handle.events()),
            AudioEvent::Started {
                device: "fake-mic".to_string(),
                description: None,
            }
        );
        assert!(matches!(
            expect_event(handle.events()),
            AudioEvent::Frame(_)
        ));
        handle.stop();
    }

    #[test]
    fn stop_interrupts_an_infinite_stream_promptly() {
        // The fake ignores the appended parec arguments and streams forever;
        // `exec` keeps the writer directly killable.
        let handle = start_capture(test_config(sh("exec cat /dev/zero"))).unwrap();
        loop {
            if let AudioEvent::Frame(_) = expect_event(handle.events()) {
                break;
            }
        }
        let begin = Instant::now();
        handle.stop();
        assert!(begin.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn drop_without_stop_interrupts_an_infinite_stream_promptly() {
        let handle = start_capture(test_config(sh("exec cat /dev/zero"))).unwrap();
        loop {
            if let AudioEvent::Frame(_) = expect_event(handle.events()) {
                break;
            }
        }
        let begin = Instant::now();
        drop(handle);
        assert!(begin.elapsed() < Duration::from_secs(2));
    }
}

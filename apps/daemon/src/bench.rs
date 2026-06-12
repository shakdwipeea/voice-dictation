use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use sunoto_ipc::{SidecarClient, SidecarEvent, SidecarMessage, SidecarRequest};
use sunoto_linux::x11::UiAdapter;
use sunoto_polish::polish;

use crate::logging;
use crate::settings::{Settings, repo_root, sanitize_for_insertion};

const FRAME_SAMPLES: usize = 320; // 20 ms at 16 kHz
const READY_TIMEOUT: Duration = Duration::from_secs(600); // model load can be slow
const FINAL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct BenchArgs {
    pub sessions: usize,
    pub audio: PathBuf,
    pub paced: bool,
    pub output: Option<PathBuf>,
}

impl Default for BenchArgs {
    fn default() -> Self {
        Self {
            sessions: 10,
            audio: repo_root().join("tests/corpus/hf-sample1.wav"),
            paced: true,
            output: None,
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct SessionMetrics {
    session: usize,
    time_to_first_partial_ms: Option<u128>,
    release_to_final_ms: u128,
    insertion_ms: u128,
    release_to_insertion_ms: u128,
    final_text: String,
    inserted_text: String,
}

#[derive(Debug, serde::Serialize)]
struct Percentiles {
    p50: u128,
    p95: u128,
    p99: u128,
}

fn percentiles(values: &mut [u128]) -> Option<Percentiles> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let pick = |p: f64| {
        let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
        values[rank.clamp(1, values.len()) - 1]
    };
    Some(Percentiles {
        p50: pick(50.0),
        p95: pick(95.0),
        p99: pick(99.0),
    })
}

/// End-to-end latency harness for the Phase 1 exit gate: pace recorded audio
/// through the configured sidecar as if it were spoken live, treat the last
/// chunk as the shortcut release, and measure until the final text has been
/// typed into a real focused X11 window.
pub fn run(settings: Settings, args: BenchArgs) -> Result<(), Box<dyn Error>> {
    let samples = read_wav_mono_16k(&args.audio)?;
    logging::info(&format!(
        "bench: {} sessions, {:.2}s audio, backend={}, profile={}ms, paced={}",
        args.sessions,
        samples.len() as f64 / 16_000.0,
        settings.backend,
        settings.profile_ms,
        args.paced
    ));

    let (events_tx, events) = mpsc::channel::<SidecarMessage>();
    let (python, sidecar_args) = settings.sidecar_command()?;
    let arg_refs: Vec<&str> = sidecar_args.iter().map(String::as_str).collect();
    let mut sidecar = SidecarClient::spawn(&python, &arg_refs, move |message| {
        events_tx.send(message).is_ok()
    })?;
    sidecar.send(&SidecarRequest::Health)?;
    let load_started = Instant::now();
    wait_for_ready(&events, READY_TIMEOUT)?;
    let warm_start_ms = load_started.elapsed().as_millis();
    logging::info(&format!("sidecar ready after {warm_start_ms}ms"));

    let mut ui = UiAdapter::open()?;
    let probe = ui.create_probe_window()?;

    let mut runs: Vec<SessionMetrics> = Vec::with_capacity(args.sessions);
    let mut failures: Vec<String> = Vec::new();
    for session in 1..=args.sessions {
        let session_id = session as u64;
        sidecar.send(&SidecarRequest::StartSession {
            session_id,
            profile_ms: settings.profile_ms,
        })?;

        let speech_started = Instant::now();
        let mut first_partial: Option<Instant> = None;
        for chunk in samples.chunks(FRAME_SAMPLES) {
            sidecar.send(&SidecarRequest::AudioChunk {
                session_id,
                samples: chunk.to_vec(),
            })?;
            drain_partials(&events, session_id, &mut first_partial);
            if args.paced {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let released_at = Instant::now();
        sidecar.send(&SidecarRequest::FinishSession { session_id })?;
        let final_text = wait_for_final(&events, session_id, &mut first_partial)?;
        let final_at = Instant::now();

        let output = if settings.polish_enabled {
            polish(&final_text, &settings.polish).text
        } else {
            final_text.clone()
        };
        let to_insert = sanitize_for_insertion(&output, settings.allow_enter_and_tab);
        let insertion = ui.type_and_confirm(&probe, &to_insert);
        let inserted_at = Instant::now();
        match insertion {
            Ok(_) => {}
            Err(error) => {
                failures.push(format!("session {session}: insertion failed: {error}"));
                continue;
            }
        }

        let metrics = SessionMetrics {
            session,
            time_to_first_partial_ms: first_partial
                .map(|at| at.duration_since(speech_started).as_millis()),
            release_to_final_ms: final_at.duration_since(released_at).as_millis(),
            insertion_ms: inserted_at.duration_since(final_at).as_millis(),
            release_to_insertion_ms: inserted_at.duration_since(released_at).as_millis(),
            final_text,
            inserted_text: to_insert,
        };
        logging::info(&format!(
            "session {session}: ttfp={:?}ms release-to-final={}ms insert={}ms total={}ms",
            metrics.time_to_first_partial_ms,
            metrics.release_to_final_ms,
            metrics.insertion_ms,
            metrics.release_to_insertion_ms
        ));
        runs.push(metrics);
    }
    ui.destroy_probe_window(probe);

    // Transcripts must be stable across sessions: state leaking between
    // dictations would surface here as growing or divergent text.
    let transcripts: Vec<&str> = runs.iter().map(|run| run.final_text.as_str()).collect();
    let stable = transcripts.windows(2).all(|pair| pair[0] == pair[1]);

    let mut ttfp: Vec<u128> = runs
        .iter()
        .filter_map(|run| run.time_to_first_partial_ms)
        .collect();
    let mut release_to_final: Vec<u128> =
        runs.iter().map(|run| run.release_to_final_ms).collect();
    let mut insertion: Vec<u128> = runs.iter().map(|run| run.insertion_ms).collect();
    let mut total: Vec<u128> = runs.iter().map(|run| run.release_to_insertion_ms).collect();

    let report = serde_json::json!({
        "backend": settings.backend,
        "profile_ms": settings.profile_ms,
        "paced": args.paced,
        "audio": args.audio.display().to_string(),
        "audio_seconds": samples.len() as f64 / 16_000.0,
        "sessions": args.sessions,
        "completed": runs.len(),
        "warm_start_ms": warm_start_ms,
        "transcripts_stable": stable,
        "failures": failures,
        "percentiles": {
            "time_to_first_partial_ms": percentiles(&mut ttfp),
            "release_to_final_ms": percentiles(&mut release_to_final),
            "insertion_ms": percentiles(&mut insertion),
            "release_to_insertion_ms": percentiles(&mut total),
        },
        "exit_gate": {
            "release_to_insertion_p95_target_ms": 600,
            "release_to_insertion_p95_ms": percentiles(&mut total.clone()).map(|p| p.p95),
            "passed": percentiles(&mut total.clone()).map(|p| p.p95 <= 600).unwrap_or(false),
        },
        "runs": runs,
    });
    let rendered = serde_json::to_string_pretty(&report)?;
    println!("{rendered}");
    if let Some(output) = args.output {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, rendered + "\n")?;
        logging::info(&format!("bench report written to {}", output.display()));
    }
    if runs.is_empty() {
        return Err("no bench session completed".into());
    }
    Ok(())
}

fn wait_for_ready(
    events: &Receiver<SidecarMessage>,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("timed out waiting for the sidecar to become ready")?;
        match events.recv_timeout(remaining)? {
            SidecarMessage::Event(SidecarEvent::Ready { .. }) => return Ok(()),
            SidecarMessage::Closed => return Err("sidecar exited during startup".into()),
            _ => {}
        }
    }
}

fn drain_partials(
    events: &Receiver<SidecarMessage>,
    session_id: u64,
    first_partial: &mut Option<Instant>,
) {
    while let Ok(message) = events.try_recv() {
        if let SidecarMessage::Event(SidecarEvent::Partial {
            session_id: event_session,
            ..
        }) = message
        {
            if event_session == session_id && first_partial.is_none() {
                *first_partial = Some(Instant::now());
            }
        }
    }
}

fn wait_for_final(
    events: &Receiver<SidecarMessage>,
    session_id: u64,
    first_partial: &mut Option<Instant>,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + FINAL_TIMEOUT;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("timed out waiting for the final transcript")?;
        match events.recv_timeout(remaining)? {
            SidecarMessage::Event(SidecarEvent::Partial {
                session_id: event_session,
                ..
            }) => {
                if event_session == session_id && first_partial.is_none() {
                    *first_partial = Some(Instant::now());
                }
            }
            SidecarMessage::Event(SidecarEvent::Final {
                session_id: event_session,
                text,
            }) if event_session == session_id => return Ok(text),
            SidecarMessage::Event(SidecarEvent::Error { message, .. }) => {
                return Err(format!("sidecar error: {message}").into());
            }
            SidecarMessage::Closed => return Err("sidecar exited mid-session".into()),
            _ => {}
        }
    }
}

/// Minimal RIFF/WAVE reader for the only format the pipeline uses:
/// 16 kHz mono 16-bit PCM.
fn read_wav_mono_16k(path: &Path) -> Result<Vec<i16>, Box<dyn Error>> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_wav_mono_16k(&bytes).map_err(|error| format!("{}: {error}", path.display()).into())
}

fn parse_wav_mono_16k(bytes: &[u8]) -> Result<Vec<i16>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut offset = 12;
    let mut format_ok = false;
    let mut samples: Option<Vec<i16>> = None;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body_start = offset + 8;
        let body_end = (body_start + chunk_size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match chunk_id {
            b"fmt " if body.len() >= 16 => {
                let audio_format = u16::from_le_bytes(body[0..2].try_into().unwrap());
                let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                if audio_format != 1 || channels != 1 || sample_rate != 16_000 || bits != 16 {
                    return Err(format!(
                        "expected 16kHz mono 16-bit PCM, found format={audio_format} channels={channels} rate={sample_rate} bits={bits}"
                    ));
                }
                format_ok = true;
            }
            b"data" => {
                samples = Some(
                    body.chunks_exact(2)
                        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                        .collect(),
                );
            }
            _ => {}
        }
        // Chunks are word-aligned.
        offset = body_start + chunk_size + (chunk_size & 1);
    }
    match (format_ok, samples) {
        (true, Some(samples)) if !samples.is_empty() => Ok(samples),
        (true, _) => Err("missing audio data".into()),
        (false, _) => Err("missing fmt chunk".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parses_valid_wav() {
        let samples: Vec<i16> = (-100..100).collect();
        let parsed = parse_wav_mono_16k(&wav_bytes(&samples, 16_000, 1)).unwrap();
        assert_eq!(parsed, samples);
    }

    #[test]
    fn rejects_wrong_formats() {
        let samples: Vec<i16> = vec![0; 64];
        assert!(parse_wav_mono_16k(&wav_bytes(&samples, 44_100, 1)).is_err());
        assert!(parse_wav_mono_16k(&wav_bytes(&samples, 16_000, 2)).is_err());
        assert!(parse_wav_mono_16k(b"junkdata").is_err());
    }

    #[test]
    fn percentiles_pick_expected_ranks() {
        let mut values: Vec<u128> = (1..=100).collect();
        let result = percentiles(&mut values).unwrap();
        assert_eq!(result.p50, 50);
        assert_eq!(result.p95, 95);
        assert_eq!(result.p99, 99);
        let mut single = vec![42];
        let result = percentiles(&mut single).unwrap();
        assert_eq!((result.p50, result.p95, result.p99), (42, 42, 42));
        assert!(percentiles(&mut Vec::new()).is_none());
    }
}

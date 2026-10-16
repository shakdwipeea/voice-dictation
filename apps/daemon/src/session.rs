//! Per-session bookkeeping: latency timestamps and audio statistics.

use std::time::{Duration, Instant};

use serde::Serialize;
use sunoto_core::SessionMode;

pub(crate) const SAMPLES_PER_MS: usize = 16;

pub(crate) struct SessionTiming {
    pub(crate) session_id: u64,
    pub(crate) pressed_at: Instant,
    pub(crate) released_at: Instant,
    pub(crate) finish_sent_at: Option<Instant>,
    pub(crate) final_at: Option<Instant>,
    pub(crate) polish_done_at: Option<Instant>,
    pub(crate) insert_dispatched_at: Option<Instant>,
}

/// Where the time went in one session, in milliseconds. The insertion
/// fields are `None` for sessions that ended before anything was inserted.
/// The log line and the history record both come from this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct TimingBreakdown {
    pub(crate) recorded: u64,
    pub(crate) release_to_finish_sent: Option<u64>,
    pub(crate) asr_turnaround: Option<u64>,
    pub(crate) release_to_final: Option<u64>,
    pub(crate) polish: Option<u64>,
    pub(crate) dispatch: Option<u64>,
    pub(crate) insert: Option<u64>,
    pub(crate) insert_wait: Option<u64>,
    pub(crate) release_to_insertion: Option<u64>,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

impl SessionTiming {
    /// The breakdown as of `now`. `insert` is how long the insertion itself
    /// took, when the session got that far.
    pub(crate) fn breakdown(&self, now: Instant, insert: Option<Duration>) -> TimingBreakdown {
        let since = |later: Option<Instant>, earlier: Option<Instant>| match (later, earlier) {
            (Some(later), Some(earlier)) => Some(millis(later.duration_since(earlier))),
            _ => None,
        };
        let inserted = insert.is_some();
        TimingBreakdown {
            recorded: millis(self.released_at.duration_since(self.pressed_at)),
            release_to_finish_sent: since(self.finish_sent_at, Some(self.released_at)),
            asr_turnaround: since(self.final_at, self.finish_sent_at),
            release_to_final: since(self.final_at, Some(self.released_at)),
            polish: since(self.polish_done_at, self.final_at),
            dispatch: since(self.insert_dispatched_at, self.polish_done_at),
            insert: insert.map(millis),
            insert_wait: since(Some(now), self.insert_dispatched_at).filter(|_| inserted),
            release_to_insertion: inserted.then(|| millis(now.duration_since(self.released_at))),
        }
    }
}

#[derive(Default)]
pub(crate) struct SessionAudioStats {
    pub(crate) samples: usize,
    pub(crate) sum_squares: f64,
    pub(crate) peak: u16,
}

impl SessionAudioStats {
    pub(crate) fn observe(&mut self, samples: &[i16]) {
        self.samples += samples.len();
        for &sample in samples {
            let sample_f64 = f64::from(sample);
            self.sum_squares += sample_f64 * sample_f64;
            self.peak = self.peak.max(sample.unsigned_abs());
        }
    }

    pub(crate) fn duration_ms(&self) -> usize {
        self.samples / SAMPLES_PER_MS
    }

    pub(crate) fn rms(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            (self.sum_squares / self.samples as f64).sqrt()
        }
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "{}ms audio, {} samples, rms={:.0}, peak={}",
            self.duration_ms(),
            self.samples,
            self.rms(),
            self.peak
        )
    }
}

/// Per-frame meter levels, normalized to 0..1 for the overlay.
pub(crate) fn frame_levels(samples: &[i16]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0u16;
    let mut sum_squares = 0.0f64;
    for &sample in samples {
        peak = peak.max(sample.unsigned_abs());
        let sample_f64 = f64::from(sample);
        sum_squares += sample_f64 * sample_f64;
    }
    let rms = (sum_squares / samples.len() as f64).sqrt();
    (f64::from(peak) / 32768.0, rms / 32768.0)
}

pub(crate) fn mode_label(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Dictation => "dictation",
        SessionMode::System => "system",
    }
}

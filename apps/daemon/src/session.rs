//! Per-session bookkeeping: latency timestamps and audio statistics.

use std::time::Instant;

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

    pub(crate) fn summary(&self) -> String {
        let duration_ms = self.samples / SAMPLES_PER_MS;
        let rms = if self.samples == 0 {
            0.0
        } else {
            (self.sum_squares / self.samples as f64).sqrt()
        };
        format!(
            "{duration_ms}ms audio, {} samples, rms={rms:.0}, peak={}",
            self.samples, self.peak
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

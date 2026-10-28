//! Per-dictation counts for accuracy reports
//! (docs/accuracy-reports-plan-2026-10-28.md, step 1).
//!
//! Each dictation's history record carries a `diag` object, and the daemon
//! log gets a one-line summary of it, so a wrong transcription can be laid
//! next to what the Mac was doing at the time: how much of the screen the
//! word list read, which microphone it was and whether input was lost, how
//! busy the machine was, and how the decode went. Counts and fixed names
//! only, never words, titles or paths.

use std::sync::mpsc::Sender;

use serde::Serialize;
use sunoto_audio::{InputGaps, InputInfo};
use sunoto_ipc::AsrDiag;

use crate::events::DaemonEvent;
use crate::vocabulary::SessionWords;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Diag {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) words: Option<WordsDiag>,
    pub(crate) audio: AudioDiag,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) system: Option<SystemDiag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) asr: Option<AsrDiag>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct WordsDiag {
    pub(crate) screen: u32,
    pub(crate) project: u32,
    pub(crate) everywhere: u32,
    /// The whole focused window was read because its text box was short.
    pub(crate) window_read: bool,
    pub(crate) elements: u32,
    pub(crate) chars: u32,
    pub(crate) read_ms: u32,
    /// Words the daemon changed after the final (`fix_near_misses`).
    pub(crate) near_miss_fixes: u32,
}

impl WordsDiag {
    pub(crate) fn new(words: &SessionWords, read_ms: u32) -> Self {
        Self {
            screen: words.screen.len() as u32,
            project: words.project.len() as u32,
            everywhere: words.everywhere.len() as u32,
            window_read: words.read.window,
            elements: words.read.elements,
            chars: words.read.chars,
            read_ms,
            near_miss_fixes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AudioDiag {
    /// "built_in", "usb", "bluetooth" or "other".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mic: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sample_rate: Option<u32>,
    /// Samples at full scale; the share is this over `sound.ms * 16`.
    pub(crate) clipped_samples: u64,
    /// Times the capture callback skipped input, and how much it lost.
    pub(crate) gaps: u64,
    pub(crate) gap_ms: u64,
    /// The microphone stopped or switched while recording.
    pub(crate) mic_changes: u32,
    /// Other apps recording from an input at the press.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) other_apps_on_mic: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SystemDiag {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) windows: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gpu_percent: Option<u32>,
    /// "normal", "warning" or "critical".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) memory_pressure: Option<&'static str>,
    /// "nominal", "fair", "serious" or "critical".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thermal: Option<&'static str>,
}

impl Diag {
    pub(crate) fn input(&mut self, info: InputInfo) {
        self.audio.mic = Some(info.kind.as_str());
        self.audio.sample_rate = Some(info.sample_rate);
        self.audio.other_apps_on_mic = info.other_apps;
    }

    pub(crate) fn gaps(&mut self, gaps: InputGaps) {
        self.audio.gaps = gaps.count;
        self.audio.gap_ms = gaps.lost_us / 1000;
    }

    /// One line for daemon.log. `recorded_ms` turns the clipped count into
    /// a share.
    pub(crate) fn summary(&self, recorded_ms: u64) -> String {
        let mut parts = Vec::new();
        if let Some(words) = &self.words {
            let window = if words.window_read {
                format!(", window read ({} elements)", words.elements)
            } else {
                String::new()
            };
            parts.push(format!(
                "words {}/{}/{}{window}, {} chars in {}ms, {} fixed",
                words.screen,
                words.project,
                words.everywhere,
                words.chars,
                words.read_ms,
                words.near_miss_fixes
            ));
        }
        let audio = &self.audio;
        let samples = recorded_ms * crate::session::SAMPLES_PER_MS as u64;
        let clipped = if samples == 0 {
            0.0
        } else {
            audio.clipped_samples as f64 * 100.0 / samples as f64
        };
        parts.push(format!(
            "mic {}{}, clipped {clipped:.2}%, {} gaps ({}ms), {} changes{}",
            audio.mic.unwrap_or("unknown"),
            audio
                .sample_rate
                .map(|rate| format!(" {rate}Hz"))
                .unwrap_or_default(),
            audio.gaps,
            audio.gap_ms,
            audio.mic_changes,
            audio
                .other_apps_on_mic
                .map(|apps| format!(", {apps} other apps on mic"))
                .unwrap_or_default(),
        ));
        if let Some(system) = &self.system {
            let known = |value: Option<String>| value.unwrap_or_else(|| "?".into());
            parts.push(format!(
                "{} windows, gpu {}%, memory {}, thermal {}",
                known(system.windows.map(|n| n.to_string())),
                known(system.gpu_percent.map(|n| n.to_string())),
                system.memory_pressure.unwrap_or("?"),
                system.thermal.unwrap_or("?"),
            ));
        }
        if let Some(asr) = &self.asr {
            parts.push(format!(
                "decode {}ms, {} piece(s), {} <unk> retries, pause decode {}, {} boosted tokens",
                asr.decode_ms,
                asr.pieces,
                asr.unk_retries,
                if asr.pause_reused {
                    "reused"
                } else {
                    "not used"
                },
                asr.boosted_tokens
            ));
        }
        parts.join("; ")
    }
}

/// Words in `fixed` that `before` did not have: what `fix_near_misses`
/// changed, counted without keeping either text.
pub(crate) fn changed_words(before: &str, fixed: &str) -> u32 {
    let mut left: Vec<&str> = before.split_whitespace().collect();
    fixed
        .split_whitespace()
        .filter(|word| match left.iter().position(|old| old == word) {
            Some(index) => {
                left.swap_remove(index);
                false
            }
            None => true,
        })
        .count() as u32
}

fn memory_pressure_name(level: u32) -> Option<&'static str> {
    match level {
        1 => Some("normal"),
        2 => Some("warning"),
        4 => Some("critical"),
        _ => None,
    }
}

fn thermal_name(state: u32) -> Option<&'static str> {
    match state {
        0 => Some("nominal"),
        1 => Some("fair"),
        2 => Some("serious"),
        3 => Some("critical"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn system_now() -> SystemDiag {
    SystemDiag {
        windows: sunoto_desktop::on_screen_window_count(),
        gpu_percent: sunoto_desktop::gpu_utilization_percent(),
        memory_pressure: sunoto_desktop::memory_pressure_level().and_then(memory_pressure_name),
        thermal: sunoto_desktop::thermal_state().and_then(thermal_name),
    }
}

#[cfg(not(target_os = "macos"))]
fn system_now() -> SystemDiag {
    let _ = (memory_pressure_name, thermal_name);
    SystemDiag::default()
}

/// Read the system and the microphone on a short-lived thread, off the
/// event loop (IOKit and the window list take a few milliseconds), and
/// report back as `DaemonEvent::Diag`.
pub(crate) fn spawn_snapshot(session_id: u64, device: Option<String>, events: Sender<DaemonEvent>) {
    let spawned = std::thread::Builder::new()
        .name("diag".into())
        .spawn(move || {
            let system = system_now();
            // Before capture has named its device (the first dictation
            // with the microphone off), it is opening the default input.
            let input = match device {
                Some(device) => sunoto_audio::input_info(&device),
                None => sunoto_audio::default_input_info(),
            };
            let _ = events.send(DaemonEvent::Diag {
                session_id,
                system,
                input,
            });
        });
    if let Err(error) = spawned {
        crate::logging::warn(&format!("session {session_id}: diag thread: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Diag {
        Diag {
            words: Some(WordsDiag {
                screen: 12,
                project: 3,
                everywhere: 40,
                window_read: true,
                elements: 812,
                chars: 5_400,
                read_ms: 37,
                near_miss_fixes: 1,
            }),
            audio: AudioDiag {
                mic: Some("usb"),
                sample_rate: Some(48_000),
                clipped_samples: 16,
                gaps: 1,
                gap_ms: 100,
                mic_changes: 0,
                other_apps_on_mic: Some(1),
            },
            system: Some(SystemDiag {
                windows: Some(23),
                gpu_percent: Some(41),
                memory_pressure: Some("normal"),
                thermal: Some("nominal"),
            }),
            asr: Some(AsrDiag {
                decode_ms: 118,
                pieces: 1,
                unk_retries: 0,
                pause_reused: true,
                boosted_tokens: 2,
            }),
        }
    }

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    /// The field names are the plan's (section 4); Sentry queries use them.
    #[test]
    fn diag_fields_match_the_plan() {
        let value = serde_json::to_value(full()).unwrap();
        assert_eq!(keys(&value), ["asr", "audio", "system", "words"]);
        assert_eq!(
            keys(&value["words"]),
            [
                "chars",
                "elements",
                "everywhere",
                "near_miss_fixes",
                "project",
                "read_ms",
                "screen",
                "window_read"
            ]
        );
        assert_eq!(
            keys(&value["audio"]),
            [
                "clipped_samples",
                "gap_ms",
                "gaps",
                "mic",
                "mic_changes",
                "other_apps_on_mic",
                "sample_rate"
            ]
        );
        assert_eq!(
            keys(&value["system"]),
            ["gpu_percent", "memory_pressure", "thermal", "windows"]
        );
        assert_eq!(
            keys(&value["asr"]),
            [
                "boosted_tokens",
                "decode_ms",
                "pause_reused",
                "pieces",
                "unk_retries"
            ]
        );
    }

    #[test]
    fn diag_holds_only_counts_and_fixed_names() {
        let text = serde_json::to_string(&full()).unwrap();
        for value in serde_json::to_value(full())
            .unwrap()
            .as_object()
            .unwrap()
            .values()
            .flat_map(|section| section.as_object().unwrap().values())
        {
            assert!(
                value.is_number()
                    || value.is_boolean()
                    || [
                        "usb",
                        "built_in",
                        "bluetooth",
                        "other",
                        "normal",
                        "warning",
                        "critical",
                        "nominal",
                        "fair",
                        "serious"
                    ]
                    .contains(&value.as_str().unwrap_or("")),
                "{value} in {text}"
            );
        }
    }

    #[test]
    fn summary_is_one_line_of_counts() {
        assert_eq!(
            full().summary(1_000),
            "words 12/3/40, window read (812 elements), 5400 chars in 37ms, 1 fixed; \
             mic usb 48000Hz, clipped 0.10%, 1 gaps (100ms), 0 changes, 1 other apps on mic; \
             23 windows, gpu 41%, memory normal, thermal nominal; \
             decode 118ms, 1 piece(s), 0 <unk> retries, pause decode reused, 2 boosted tokens"
        );
        assert_eq!(
            Diag::default().summary(0),
            "mic unknown, clipped 0.00%, 0 gaps (0ms), 0 changes"
        );
    }

    #[test]
    fn changed_words_counts_what_the_fix_replaced() {
        assert_eq!(changed_words("ship it to sue noto", "ship it to Sunoto"), 1);
        assert_eq!(changed_words("same words here", "same words here"), 0);
        assert_eq!(changed_words("a a b", "a b b"), 1);
    }
}

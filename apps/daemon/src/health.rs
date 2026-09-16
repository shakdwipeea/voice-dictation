//! Daemon health: the one place that decides whether Sunoto is really ready.
//!
//! Every input that can stop a key press from becoming a recording feeds
//! [`HealthInputs`]. The event loop refreshes the monitor on every pass and
//! publishes only transitions, so the log says "ready" exactly once per
//! actual readiness and the overlay shows the current blocker while idle.

use std::fmt;

use sunoto_core::SessionMode;
use sunoto_desktop::hotkey_block_reason;

use crate::logging;
use crate::overlay::UiFront;
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHealth {
    /// The global hotkey exists but macOS delivers no events to it.
    HotkeyBlocked,
    /// Microphone capture stopped (device lost, permission revoked).
    MicUnavailable,
    /// The ASR sidecar has not reported ready.
    LoadingAsr,
    /// ASR is up; the LLM polish sidecar is still running its warm-up.
    WarmingPolish,
    /// Everything else is up but the microphone has never delivered audio.
    /// On a fresh install this is the Microphone permission prompt.
    MicStarting,
    /// The microphone was released while idle and is reopening for a press.
    MicReopening,
    Ready,
}

/// What the capture thread is doing, as far as the loop knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicState {
    /// Capture requested, no `Started` event yet.
    Starting,
    Capturing,
    /// Released on purpose after an idle stretch; reopens on the next press.
    Idle,
    /// Capture stopped without us asking.
    Unavailable,
}

impl MicState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Capturing => "capturing",
            Self::Idle => "idle",
            Self::Unavailable => "unavailable",
        }
    }
}

impl DaemonHealth {
    /// Stable machine name used in the overlay protocol and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::HotkeyBlocked => "hotkey_blocked",
            Self::MicUnavailable => "mic_unavailable",
            Self::LoadingAsr => "loading_asr",
            Self::WarmingPolish => "warming_polish",
            Self::MicStarting => "mic_starting",
            Self::MicReopening => "mic_reopening",
            Self::Ready => "ready",
        }
    }

    /// Short caption for the pill. Kept under about 30 characters so it
    /// fits the overlay's caption chip without truncation.
    pub fn caption(self) -> &'static str {
        match self {
            Self::HotkeyBlocked => "hotkey blocked: check permissions",
            Self::MicUnavailable => "microphone unavailable",
            Self::LoadingAsr => "loading speech model...",
            Self::WarmingPolish => "warming polish...",
            Self::MicStarting => "waiting for microphone access",
            Self::MicReopening => "microphone starting...",
            Self::Ready => "",
        }
    }

    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }
}

impl fmt::Display for DaemonHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthInputs {
    pub asr_ready: bool,
    pub polish_warmed: bool,
    pub hotkey_blocked: bool,
    pub mic: MicState,
    /// The daemon itself asked for the mic to reopen after an idle release.
    pub mic_reopening: bool,
}

impl HealthInputs {
    /// Priority order: what the user must fix first comes first. A blocked
    /// hotkey needs a settings change; everything else resolves on its own.
    /// A microphone that never starts is reported last, after the model has
    /// loaded, because on a cold start it is usually just slower than us.
    pub fn health(self) -> DaemonHealth {
        if self.hotkey_blocked {
            DaemonHealth::HotkeyBlocked
        } else if self.mic == MicState::Unavailable {
            DaemonHealth::MicUnavailable
        } else if !self.asr_ready {
            DaemonHealth::LoadingAsr
        } else if !self.polish_warmed {
            DaemonHealth::WarmingPolish
        } else if self.mic == MicState::Starting && self.mic_reopening {
            DaemonHealth::MicReopening
        } else if self.mic == MicState::Starting {
            DaemonHealth::MicStarting
        } else {
            DaemonHealth::Ready
        }
    }
}

/// Tracks the last published health so callers can act on transitions only.
#[derive(Debug, Default)]
pub struct HealthMonitor {
    current: Option<DaemonHealth>,
}

impl HealthMonitor {
    /// Recompute from `inputs`. Returns the new health when it differs from
    /// the last published one (including the very first evaluation).
    pub fn refresh(&mut self, inputs: HealthInputs) -> Option<DaemonHealth> {
        let next = inputs.health();
        if self.current == Some(next) {
            return None;
        }
        self.current = Some(next);
        Some(next)
    }

    pub fn current(&self) -> DaemonHealth {
        self.current.unwrap_or(DaemonHealth::LoadingAsr)
    }
}

pub(crate) fn health_inputs(
    asr_ready: bool,
    polish_warmed: bool,
    blocked_modes: &[SessionMode],
    mic: MicState,
    mic_reopening: bool,
) -> HealthInputs {
    HealthInputs {
        asr_ready,
        polish_warmed,
        hotkey_blocked: !blocked_modes.is_empty(),
        mic,
        mic_reopening,
    }
}

/// Log a health transition and push it to the overlay. This is the only
/// place that says "ready", so the log line means a key press will record.
pub(crate) fn publish_health(health: DaemonHealth, ui: &mut UiFront, settings: &Settings) {
    match health {
        DaemonHealth::Ready => {
            if settings.system_mode_enabled {
                logging::info(&format!(
                    "Sunoto ready. Hold {} to dictate or {} for System mode.",
                    settings.shortcut, settings.system_shortcut
                ));
            } else {
                logging::info(&format!(
                    "Sunoto ready for dictation. Hold {} to dictate.",
                    settings.shortcut
                ));
            }
        }
        DaemonHealth::HotkeyBlocked => {
            logging::error(&format!(
                "daemon state: {health}: {}",
                hotkey_block_reason()
            ));
        }
        other => {
            logging::info(&format!("daemon state: {other}: {}", other.caption()));
        }
    }
    ui.health(health);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_good() -> HealthInputs {
        HealthInputs {
            asr_ready: true,
            polish_warmed: true,
            hotkey_blocked: false,
            mic: MicState::Capturing,
            mic_reopening: false,
        }
    }

    #[test]
    fn blocked_hotkey_outranks_everything_else() {
        let inputs = HealthInputs {
            asr_ready: false,
            polish_warmed: false,
            hotkey_blocked: true,
            mic: MicState::Unavailable,
            mic_reopening: false,
        };
        assert_eq!(inputs.health(), DaemonHealth::HotkeyBlocked);
    }

    #[test]
    fn startup_order_is_asr_then_polish_then_mic_then_ready() {
        let mut inputs = HealthInputs {
            asr_ready: false,
            polish_warmed: false,
            hotkey_blocked: false,
            mic: MicState::Starting,
            mic_reopening: false,
        };
        assert_eq!(inputs.health(), DaemonHealth::LoadingAsr);
        inputs.asr_ready = true;
        assert_eq!(inputs.health(), DaemonHealth::WarmingPolish);
        inputs.polish_warmed = true;
        assert_eq!(inputs.health(), DaemonHealth::MicStarting);
        inputs.mic = MicState::Capturing;
        assert_eq!(inputs.health(), DaemonHealth::Ready);
        // Reopening after an idle release is not a permission problem.
        inputs.mic = MicState::Starting;
        inputs.mic_reopening = true;
        assert_eq!(inputs.health(), DaemonHealth::MicReopening);
        inputs.mic_reopening = false;
        // An idle release is not a problem.
        inputs.mic = MicState::Idle;
        assert_eq!(inputs.health(), DaemonHealth::Ready);
        inputs.mic = MicState::Unavailable;
        assert_eq!(inputs.health(), DaemonHealth::MicUnavailable);
    }

    #[test]
    fn monitor_reports_transitions_only() {
        let mut monitor = HealthMonitor::default();
        let mut inputs = all_good();
        inputs.asr_ready = false;
        assert_eq!(monitor.refresh(inputs), Some(DaemonHealth::LoadingAsr));
        assert_eq!(monitor.refresh(inputs), None);
        inputs.asr_ready = true;
        assert_eq!(monitor.refresh(inputs), Some(DaemonHealth::Ready));
        assert_eq!(monitor.refresh(inputs), None);
        assert!(monitor.current().is_ready());
        // Losing the sidecar later flips back, and is announced once.
        inputs.asr_ready = false;
        assert_eq!(monitor.refresh(inputs), Some(DaemonHealth::LoadingAsr));
        assert_eq!(monitor.refresh(inputs), None);
    }

    #[test]
    fn captions_fit_the_overlay_chip() {
        for health in [
            DaemonHealth::HotkeyBlocked,
            DaemonHealth::MicUnavailable,
            DaemonHealth::LoadingAsr,
            DaemonHealth::WarmingPolish,
            DaemonHealth::MicStarting,
            DaemonHealth::MicReopening,
        ] {
            assert!(!health.caption().is_empty());
            assert!(health.caption().len() <= 36, "{health}");
        }
        assert!(DaemonHealth::Ready.caption().is_empty());
    }
}

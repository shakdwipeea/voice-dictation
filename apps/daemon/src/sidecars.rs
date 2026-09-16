//! Child processes and bridge threads the loop depends on: the ASR sidecar
//! (with restart backoff), the global hotkey listeners, and microphone
//! capture. Each thread turns its source into `DaemonEvent`s and nothing
//! else; policy stays in the loop.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sunoto_audio::{CaptureConfig, start_capture};
use sunoto_core::{SessionAction, SessionMachine, SessionMode};
use sunoto_desktop::{HotkeyListener, Shortcut};
use sunoto_ipc::{SidecarClient, SidecarRequest};

use crate::events::{DaemonEvent, ModeHotkeyEvent};
use crate::logging;
use crate::overlay::{UiFront, show_error};
use crate::session::{SessionTiming, mode_label};
use crate::settings::Settings;

pub(crate) const SIDECAR_BACKOFF_START: Duration = Duration::from_millis(500);

pub(crate) const SIDECAR_BACKOFF_CAP: Duration = Duration::from_secs(5);

pub(crate) fn spawn_sidecar(
    settings: &Settings,
    events: Sender<DaemonEvent>,
) -> Result<SidecarClient, String> {
    let (python, args) = settings.sidecar_command()?;
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut client = SidecarClient::spawn(&python, &arg_refs, move |message| {
        events.send(DaemonEvent::Sidecar(message)).is_ok()
    })
    .map_err(|error| format!("cannot start ASR sidecar: {error}"))?;
    client
        .send(&SidecarRequest::Health)
        .map_err(|error| format!("cannot health-check ASR sidecar: {error}"))?;
    Ok(client)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_sidecar_loss(
    reason: &str,
    machine: &mut SessionMachine,
    sidecar: &mut Option<SidecarClient>,
    respawn_at: &mut Option<Instant>,
    respawn_backoff: &mut Duration,
    ui: &UiFront,
    bubble_hide_at: &mut Option<Instant>,
    timing: &mut Option<SessionTiming>,
    transcribe_deadline: &mut Option<Instant>,
    sidecar_ready: &mut bool,
    llm_post_asr_warmed: &mut bool,
    llm_polish_active: bool,
) {
    if sidecar.is_none() {
        return;
    }
    logging::error(reason);
    *sidecar = None;
    *sidecar_ready = false;
    *llm_post_asr_warmed = !llm_polish_active;
    if let SessionAction::Failed { .. } = machine.fail(reason) {
        show_error(ui, "ASR backend lost; restarting", bubble_hide_at);
    }
    *timing = None;
    *transcribe_deadline = None;
    *respawn_at = Some(Instant::now() + *respawn_backoff);
    *respawn_backoff = (*respawn_backoff * 2).min(SIDECAR_BACKOFF_CAP);
}

pub(crate) fn spawn_hotkey_thread(
    mode: SessionMode,
    shortcut: Shortcut,
    events: Sender<DaemonEvent>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = match HotkeyListener::open(&shortcut) {
            Ok(listener) => listener,
            Err(error) => {
                // A missing macOS TCC grant must not take down the daemon:
                // compositor/control-socket triggers remain useful for
                // recovery and mock System-mode verification. The native
                // hotkey stays unavailable until the user grants permission.
                logging::warn(&format!(
                    "global {} shortcut unavailable: {error}; physical hotkey disabled while control triggers remain available",
                    mode_label(mode)
                ));
                return;
            }
        };
        while !stop.load(Ordering::SeqCst) {
            if let Some(event) = listener.wait(Duration::from_millis(250))
                && events
                    .send(DaemonEvent::Hotkey(ModeHotkeyEvent { mode, edge: event }))
                    .is_err()
            {
                return;
            }
        }
    })
}

pub(crate) fn spawn_capture_thread(
    settings: &Settings,
    events: Sender<DaemonEvent>,
    stop: Arc<AtomicBool>,
    wanted: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, Box<dyn Error>> {
    let device = settings.microphone.clone();
    Ok(std::thread::spawn(move || {
        // Retry capture startup with a backoff. On macOS the first attempt
        // may fail until Microphone TCC permission is granted (the prompt
        // appears on the first access); dying here would make the daemon
        // unstartable before the user can grant it. Mirror the Linux
        // capture-restart behavior: log, back off, and try again.
        let backoffs = [250, 500, 1000, 2000, 4000];
        let mut attempt = 0usize;
        let mut capture: Option<sunoto_audio::CaptureHandle> = None;
        while !stop.load(Ordering::SeqCst) {
            if !wanted.load(Ordering::SeqCst) {
                // Idle: release the device so the mic indicator goes away
                // and no audio is captured. Reopened as soon as `wanted`
                // flips back.
                if let Some(h) = capture.take() {
                    h.stop();
                    attempt = 0;
                }
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            if capture.is_none() {
                match start_capture(CaptureConfig {
                    device: device.clone(),
                    ..CaptureConfig::default()
                }) {
                    Ok(handle) => capture = Some(handle),
                    Err(error) => {
                        logging::warn(&format!(
                            "microphone capture unavailable: {error}; retrying"
                        ));
                        let delay = backoffs[attempt.min(backoffs.len() - 1)];
                        attempt += 1;
                        // Sleep in slices so stop() interrupts promptly.
                        let mut remaining = Duration::from_millis(delay);
                        while !remaining.is_zero() && !stop.load(Ordering::SeqCst) {
                            let step = remaining.min(Duration::from_millis(50));
                            std::thread::sleep(step);
                            remaining = remaining.saturating_sub(step);
                        }
                        continue;
                    }
                }
            }
            let Some(handle) = capture.as_ref() else {
                continue;
            };
            match handle.events().recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    if events.send(DaemonEvent::Audio(event)).is_err() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // The capture stream ended (mic unplugged, permission
                    // revoked, ...). Drop and retry.
                    if let Some(h) = capture.take() {
                        h.stop();
                    }
                    attempt = 0;
                }
            }
        }
        if let Some(h) = capture.take() {
            h.stop();
        }
    }))
}

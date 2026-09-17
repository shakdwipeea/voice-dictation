use std::error::Error;
use std::io::Write;
use std::os::raw::c_int;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use sunoto_audio::AudioEvent;
use sunoto_core::{AudioPreRoll, SessionAction, SessionMachine, SessionMode, SessionState};
use sunoto_desktop::{
    BubbleKind, HotkeyEvent, InsertionOutcome, Shortcut, hotkey_block_reason,
    keep_process_responsive, permission_preflights,
};
use sunoto_ipc::{OverlayRequest, OverlaySuggestion, SidecarEvent, SidecarMessage, SidecarRequest};
use sunoto_polish::{polish, resolve_style};
use sunoto_system::{
    CapabilityInput, NativeCapabilityCall, PendingSuggestionSet, RouteOutcome, SystemIntent,
    TargetHint, ValidatedHttpUrl, route_deterministically,
};

use crate::control::{control_polish_response, spawn_control_thread};
use crate::events::{DaemonEvent, ModeHotkeyEvent};
use crate::health::{DaemonHealth, HealthMonitor, MicState, health_inputs, publish_health};
use crate::insertion::{DesktopBackend, UiCommand, UiOptions, desktop_backend, ui_thread};
use crate::llm_polish;
use crate::llm_report::{format_llm_diagnostics, format_llm_warmup_summary};
use crate::logging;
use crate::overlay::{ERROR_BUBBLE_VISIBLE, UiFront, show_error, spawn_overlay};
use crate::session::{SAMPLES_PER_MS, SessionAudioStats, SessionTiming, frame_levels, mode_label};
use crate::settings::{Settings, sanitize_for_insertion, transcript_for_log};
use crate::sidecars::{
    SIDECAR_BACKOFF_CAP, SIDECAR_BACKOFF_START, handle_sidecar_loss, spawn_capture_thread,
    spawn_hotkey_thread, spawn_sidecar,
};
use crate::system_dispatch::{
    PendingSystemSelection, SelectedSystemAction, present_navigation_confirmation,
    suggestion_action_label,
};
use crate::system_worker::{SystemJob, SystemWorker, SystemWorkerEvent};

const TICK: Duration = Duration::from_millis(50);

/// How long after a session ends the LLM keepalive keeps pinging, so a quick
/// follow-up dictation still finds a warm GPU.
const KEEPALIVE_GRACE: Duration = Duration::from_secs(20);

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
}

unsafe extern "C" fn on_termination_signal(_signum: c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        signal(SIGINT, on_termination_signal as *const () as usize);
        signal(SIGTERM, on_termination_signal as *const () as usize);
    }
}

pub fn run(settings: Settings) -> Result<(), Box<dyn Error>> {
    install_signal_handlers();
    if !keep_process_responsive("Sunoto listens for the push-to-talk shortcut") {
        logging::warn("could not opt out of App Nap; the hotkey may be throttled while idle");
    }
    let backend = desktop_backend(&settings);
    // Wayland has no global-grab primitive; it relies on compositor bindings
    // driving `sunoto-daemon trigger press|release` over the control socket.
    // X11 (XGrabKey) and macOS (CGEventTap) both install a real global hotkey.
    let shortcuts = if backend == DesktopBackend::Wayland {
        Vec::new()
    } else {
        let mut shortcuts = vec![(SessionMode::Dictation, Shortcut::parse(&settings.shortcut)?)];
        if settings.system_mode_enabled {
            shortcuts.push((
                SessionMode::System,
                Shortcut::parse(&settings.system_shortcut)?,
            ));
        }
        shortcuts
    };
    let (events_tx, events) = mpsc::channel::<DaemonEvent>();
    let system_worker = SystemWorker::spawn(events_tx.clone(), settings.system_search_root_paths());

    // UI thread (insertion/clipboard/bubble on its own backend connection).
    let (ui_tx, ui_rx) = mpsc::channel::<UiCommand>();
    let ui_events = events_tx.clone();
    let ui_options = UiOptions {
        clipboard_restore: settings.clipboard_restore,
    };
    let ui_handle = std::thread::spawn(move || ui_thread(ui_rx, ui_events, backend, ui_options));

    // Control socket for compositor/user-triggered push-to-talk edges.
    let control_stop = Arc::new(AtomicBool::new(false));
    let control_handle = spawn_control_thread(events_tx.clone(), Arc::clone(&control_stop))?;

    // Hotkey thread (second X11 connection, blocking with poll timeouts).
    let hotkey_stop = Arc::new(AtomicBool::new(false));
    let hotkey_handles = shortcuts
        .into_iter()
        .map(|(mode, shortcut)| {
            spawn_hotkey_thread(mode, shortcut, events_tx.clone(), Arc::clone(&hotkey_stop))
        })
        .collect::<Vec<_>>();
    if backend == DesktopBackend::Wayland {
        logging::info(
            "Wayland session detected; use compositor bindings to call `sunoto-daemon trigger [dictation|system] press|release`",
        );
    }

    // Persistent microphone capture bridged into the event channel.
    let capture_stop = Arc::new(AtomicBool::new(false));
    let capture_wanted = Arc::new(AtomicBool::new(true));
    let capture_handle = spawn_capture_thread(
        &settings,
        events_tx.clone(),
        Arc::clone(&capture_stop),
        Arc::clone(&capture_wanted),
    )?;

    // Status UI: GTK overlay sidecar when enabled and startable, X11 bubble
    // otherwise. The overlay is cosmetic — any failure degrades, never aborts.
    let mut ui = UiFront {
        bubble: ui_tx.clone(),
        overlay: None,
        overlay_ready: false,
        attention: None,
    };
    let mut overlay_ever_ready = false;
    let mut overlay_respawn_at: Option<Instant> = None;
    let mut overlay_backoff = SIDECAR_BACKOFF_START;
    if settings.overlay_enabled {
        match spawn_overlay(&settings, events_tx.clone()) {
            Ok(handle) => ui.overlay = Some(handle),
            Err(error) => logging::warn(&format!("{error}; using the native X11 bubble")),
        }
    }

    let mut sidecar = Some(spawn_sidecar(&settings, events_tx.clone())?);
    let llm_model_present = settings
        .llm_polish_model_file()
        .is_some_and(|path| path.is_file());
    let mut llm_polish = if settings.llm_polish_enabled && !llm_model_present {
        logging::warn(&format!(
            "LLM polish disabled: model file not found ({}); run `sunoto-daemon setup --with-llm` to download it, or set llm_polish_enabled to false",
            settings
                .llm_polish_model_file()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no path resolved".to_string())
        ));
        None
    } else if settings.llm_polish_enabled {
        match llm_polish::LlmPolishClient::spawn(&settings) {
            Ok(client) => Some(client),
            Err(error) => {
                logging::warn(&format!("LLM polish disabled for this daemon run: {error}"));
                None
            }
        }
    } else {
        None
    };
    let system_shortcut = if settings.system_mode_enabled {
        format!(", system_shortcut={}", settings.system_shortcut)
    } else {
        String::new()
    };
    logging::info(&format!(
        "Sunoto daemon starting: backend={}, desktop={backend:?}, profile={}ms, shortcut={}{}. Wait for the ASR sidecar ready message; Ctrl+C exits.",
        settings.backend, settings.profile_ms, settings.shortcut, system_shortcut
    ));

    let mut machine = SessionMachine::default();
    let mut preroll = AudioPreRoll::new(settings.preroll_ms as usize * SAMPLES_PER_MS);
    let mut focused_class: Option<(String, String)> = None;
    let mut timing: Option<SessionTiming> = None;
    let mut last_pressed_at: Option<Instant> = None;
    let mut audio_stats: Option<SessionAudioStats> = None;
    let mut sidecar_ready = false;
    let mut transcribe_deadline: Option<Instant> = None;
    let mut bubble_hide_at: Option<Instant> = None;
    let mut respawn_at: Option<Instant> = None;
    let mut respawn_backoff = SIDECAR_BACKOFF_START;
    let mut llm_post_asr_warmed = llm_polish.is_none();
    let started_at = Instant::now();
    let mut health = HealthMonitor::default();
    let mut blocked_modes: Vec<SessionMode> = Vec::new();
    let mut hotkey_verified = false;
    let mut mic = MicState::Starting;
    // Quiet-idle bookkeeping: the mic is released and the LLM keepalive
    // window closed after a stretch with no session.
    let mut last_activity = Instant::now();
    // While the hotkey is blocked, watch the permission preflights once a
    // second. A grant given in System Settings only applies to a fresh
    // process, so when both report granted and delivery is still blocked,
    // the app relaunches itself instead of waiting for the installer.
    let mut last_preflight_check = Instant::now();
    let mut grants_seen_at: Option<Instant> = None;
    let mut permission_db_stamp = crate::setup::permission_db_stamp();
    let mut relaunch_due: Option<Instant> = None;
    let mut warmup_pending = false;
    let mut capture_idle = false;
    let mut capture_requested_at: Option<Instant> = None;
    let mut keepalive_active = false;
    let mut active_system_resolution: Option<u64> = None;
    let mut pending_system: Option<PendingSystemSelection> = None;
    let mut exit_error: Option<String> = None;

    while !STOP_REQUESTED.load(Ordering::SeqCst) {
        match events.recv_timeout(TICK) {
            Ok(DaemonEvent::Hotkey(ModeHotkeyEvent {
                mode,
                edge: HotkeyEvent::Blocked,
            })) => {
                if !matches!(machine.state(), SessionState::Idle) {
                    // The shortcut just started or ended this session, so
                    // events are plainly arriving; a probe lost under load
                    // is not a permission problem.
                    logging::warn("ignored a blocked-hotkey verdict during a live session");
                    continue;
                }
                if !blocked_modes.contains(&mode) {
                    blocked_modes.push(mode);
                }
                logging::error(&format!(
                    "{} shortcut is not receiving events: {}",
                    mode_label(mode),
                    hotkey_block_reason()
                ));
            }
            Ok(DaemonEvent::Hotkey(ModeHotkeyEvent {
                mode,
                edge: HotkeyEvent::Available,
            })) => {
                let was_blocked = blocked_modes.contains(&mode);
                blocked_modes.retain(|blocked| *blocked != mode);
                hotkey_verified = true;
                if was_blocked {
                    logging::info(&format!("{} shortcut delivery restored", mode_label(mode)));
                } else {
                    logging::info(&format!(
                        "{} shortcut verified: events reach the daemon",
                        mode_label(mode)
                    ));
                }
                if warmup_pending && blocked_modes.is_empty() && sidecar_ready {
                    warmup_pending = false;
                    if let Some(next) = health.refresh(health_inputs(
                        sidecar_ready,
                        llm_post_asr_warmed,
                        &blocked_modes,
                        mic,
                        capture_requested_at.is_some(),
                    )) {
                        publish_health(next, &mut ui, &settings);
                    }
                    run_llm_warmup(
                        &mut llm_polish,
                        &settings,
                        &mut llm_post_asr_warmed,
                        &ui,
                        &mut bubble_hide_at,
                    );
                }
            }
            Ok(DaemonEvent::Hotkey(ModeHotkeyEvent {
                mode,
                edge: HotkeyEvent::Pressed,
            })) => {
                if mode == SessionMode::System && !settings.system_mode_enabled {
                    logging::warn("System shortcut ignored because System mode is disabled");
                    continue;
                }
                // A new capture explicitly replaces any old palette. This
                // also invalidates a discovery result still in flight.
                active_system_resolution = None;
                system_worker.invalidate_sessions();
                if let Some(pending) = pending_system.take() {
                    let session_id = pending.session_id();
                    ui.dismiss_system_palette(session_id);
                }
                if !health.current().is_ready() {
                    // Show the real blocker rather than dropping the press
                    // silently. System mode never polishes, so a warming
                    // LLM does not hold it back.
                    let blocker = health.current();
                    if blocker != DaemonHealth::WarmingPolish || mode == SessionMode::Dictation {
                        logging::warn(&format!("push-to-talk ignored: {blocker}"));
                        show_error(&ui, blocker.caption(), &mut bubble_hide_at);
                        continue;
                    }
                }
                if let SessionAction::Started { session_id, .. } = machine.press_mode(mode) {
                    if mode == SessionMode::System {
                        system_worker.activate_session(session_id);
                    }
                    last_pressed_at = Some(Instant::now());
                    last_activity = Instant::now();
                    if capture_idle {
                        // The mic was released while idle; reopen it now.
                        // This first session gets no pre-roll; every later
                        // one does until the next idle stretch.
                        capture_idle = false;
                        mic = MicState::Starting;
                        capture_wanted.store(true, Ordering::SeqCst);
                        capture_requested_at = Some(Instant::now());
                        logging::info("microphone starting after idle; keep holding the key");
                    }
                    if mode == SessionMode::Dictation
                        && let Some(client) = llm_polish.as_mut()
                    {
                        if let Err(error) = client.keepalive_start() {
                            logging::warn(&format!("LLM keepalive start failed: {error}"));
                        } else {
                            keepalive_active = true;
                        }
                    }
                    logging::info(&format!(
                        "session {session_id}: recording ({})",
                        mode_label(mode)
                    ));
                    let recording_label = match mode {
                        SessionMode::Dictation => "recording...",
                        SessionMode::System => "System — listening...",
                    };
                    ui.show(BubbleKind::Recording, recording_label);
                    bubble_hide_at = None;
                    let preroll_samples = preroll.snapshot();
                    preroll.clear();
                    let mut stats = SessionAudioStats::default();
                    stats.observe(&preroll_samples);
                    audio_stats = Some(stats);
                    if let Some(client) = sidecar.as_mut() {
                        let mut send_result = client.send(&SidecarRequest::StartSession {
                            session_id,
                            profile_ms: settings.profile_ms,
                        });
                        if send_result.is_ok() && !preroll_samples.is_empty() {
                            send_result = client.send(&SidecarRequest::AudioChunk {
                                session_id,
                                samples: preroll_samples,
                            });
                        }
                        if let Err(error) = send_result {
                            handle_sidecar_loss(
                                &format!("sidecar write failed: {error}"),
                                &mut machine,
                                &mut sidecar,
                                &mut respawn_at,
                                &mut respawn_backoff,
                                &ui,
                                &mut bubble_hide_at,
                                &mut timing,
                                &mut transcribe_deadline,
                                &mut sidecar_ready,
                                &mut llm_post_asr_warmed,
                                llm_polish.is_some(),
                            );
                        }
                    } else {
                        machine.fail("ASR sidecar is not running");
                        show_error(&ui, "ASR backend restarting...", &mut bubble_hide_at);
                    }
                }
            }
            Ok(DaemonEvent::Hotkey(ModeHotkeyEvent {
                mode,
                edge: HotkeyEvent::Released,
            })) => {
                last_activity = Instant::now();
                if let SessionAction::FinishRequested {
                    session_id, mode, ..
                } = machine.release_mode(mode)
                {
                    // The class arrives with the fresh focus capture below; a
                    // stale one from an earlier session must not style this one.
                    focused_class = None;
                    let released_at = Instant::now();
                    let recording_ms = last_pressed_at
                        .map(|pressed| released_at.duration_since(pressed).as_millis());
                    timing = Some(SessionTiming {
                        session_id,
                        pressed_at: last_pressed_at.unwrap_or(released_at),
                        released_at,
                        finish_sent_at: None,
                        final_at: None,
                        polish_done_at: None,
                        insert_dispatched_at: None,
                    });
                    // Adaptive transcribe deadline: the offline backend's
                    // latency grows ~linearly with utterance length, so give
                    // long utterances proportionally more headroom than a flat
                    // timeout allows. recorded_ms * rtf + final_timeout_ms.
                    let recorded_ms = last_pressed_at
                        .map(|p| released_at.duration_since(p).as_millis() as u64)
                        .unwrap_or(0);
                    let deadline_ms = settings.final_timeout_ms
                        + (recorded_ms as f64 * settings.final_timeout_rtf) as u64;
                    transcribe_deadline = Some(released_at + Duration::from_millis(deadline_ms));
                    if let Some(stats) = audio_stats.as_ref() {
                        logging::info(&format!(
                            "session {session_id}: sent to ASR: {}{}, timeout {deadline_ms}ms",
                            stats.summary(),
                            recording_ms
                                .map(|ms| format!(", recorded {ms}ms"))
                                .unwrap_or_default(),
                        ));
                    }
                    if mode == SessionMode::Dictation {
                        // Focus is captured only for dictation. System mode
                        // never inserts its transcript into the active app.
                        let _ = ui_tx.send(UiCommand::CaptureFocus);
                        ui.show(BubbleKind::Transcribing, "transcribing...");
                    } else {
                        ui.show(BubbleKind::Transcribing, "System — matching...");
                    }
                    let request = SidecarRequest::FinishSession { session_id };
                    let send_error = sidecar
                        .as_mut()
                        .and_then(|client| client.send(&request).err());
                    if send_error.is_none()
                        && let Some(timing) = timing.as_mut()
                    {
                        timing.finish_sent_at = Some(Instant::now());
                    }
                    if let Some(error) = send_error {
                        handle_sidecar_loss(
                            &format!("sidecar write failed: {error}"),
                            &mut machine,
                            &mut sidecar,
                            &mut respawn_at,
                            &mut respawn_backoff,
                            &ui,
                            &mut bubble_hide_at,
                            &mut timing,
                            &mut transcribe_deadline,
                            &mut sidecar_ready,
                            &mut llm_post_asr_warmed,
                            llm_polish.is_some(),
                        );
                    }
                }
            }
            Ok(DaemonEvent::Audio(AudioEvent::Frame(samples))) => match machine.state() {
                SessionState::Recording { session_id, .. } => {
                    let session_id = *session_id;
                    if let Some(stats) = audio_stats.as_mut() {
                        stats.observe(&samples);
                    }
                    if ui.overlay_active() {
                        let (peak, rms) = frame_levels(&samples);
                        let elapsed_s = audio_stats
                            .as_ref()
                            .map(|stats| stats.samples as f64 / (SAMPLES_PER_MS as f64 * 1000.0))
                            .unwrap_or(0.0);
                        ui.level(elapsed_s, peak, rms);
                    }
                    let request = SidecarRequest::AudioChunk {
                        session_id,
                        samples,
                    };
                    let send_error = sidecar
                        .as_mut()
                        .and_then(|client| client.send(&request).err());
                    if let Some(error) = send_error {
                        handle_sidecar_loss(
                            &format!("sidecar write failed: {error}"),
                            &mut machine,
                            &mut sidecar,
                            &mut respawn_at,
                            &mut respawn_backoff,
                            &ui,
                            &mut bubble_hide_at,
                            &mut timing,
                            &mut transcribe_deadline,
                            &mut sidecar_ready,
                            &mut llm_post_asr_warmed,
                            llm_polish.is_some(),
                        );
                    }
                }
                _ => preroll.push(&samples),
            },
            Ok(DaemonEvent::Audio(AudioEvent::Started {
                device,
                description,
            })) => {
                let description = description.unwrap_or_else(|| "<unknown>".to_string());
                let after_request = capture_requested_at
                    .take()
                    .map(|at| format!(" ({}ms after the press)", at.elapsed().as_millis()))
                    .unwrap_or_default();
                logging::info(&format!(
                    "microphone capture started: {description} (source: {device}){after_request}"
                ));
                mic = MicState::Capturing;
            }
            Ok(DaemonEvent::Audio(AudioEvent::Stopped { reason })) if capture_idle => {
                // Expected: we released the mic ourselves.
                logging::info(&format!("microphone released while idle: {reason}"));
            }
            Ok(DaemonEvent::Audio(AudioEvent::Stopped { reason })) => {
                logging::warn(&format!("microphone capture stopped: {reason}"));
                mic = MicState::Unavailable;
                if matches!(machine.state(), SessionState::Recording { .. }) {
                    logging::warn("microphone lost mid-dictation; the session keeps running");
                }
            }
            Ok(DaemonEvent::Sidecar(SidecarMessage::Event(event))) => match event {
                SidecarEvent::Ready { backend } => {
                    sidecar_ready = true;
                    respawn_backoff = SIDECAR_BACKOFF_START;
                    logging::info(&format!("ASR sidecar ready: {backend}"));
                    llm_post_asr_warmed = llm_polish.is_none();
                    if llm_polish.is_some() && !blocked_modes.is_empty() {
                        // The warm-up blocks the loop for several seconds
                        // and is thrown away by the relaunch that follows a
                        // permission grant. Do it once the hotkey works.
                        warmup_pending = true;
                        logging::info("LLM polish warm-up deferred until the hotkey is verified");
                    } else if llm_polish.is_some() {
                        // Publish "warming" before the blocking warm-up, or
                        // the overlay would jump straight from loading to
                        // ready without explaining the pause.
                        if let Some(next) = health.refresh(health_inputs(
                            sidecar_ready,
                            llm_post_asr_warmed,
                            &blocked_modes,
                            mic,
                            capture_requested_at.is_some(),
                        )) {
                            publish_health(next, &mut ui, &settings);
                        }
                        run_llm_warmup(
                            &mut llm_polish,
                            &settings,
                            &mut llm_post_asr_warmed,
                            &ui,
                            &mut bubble_hide_at,
                        );
                    }
                    // "Sunoto ready" is logged by publish_health on the
                    // transition to Ready, never here: ASR alone is not
                    // readiness.
                }
                SidecarEvent::SessionStarted { session_id } => {
                    logging::info(&format!("session {session_id}: sidecar accepted"));
                }
                SidecarEvent::Partial { session_id, text } => {
                    if let SessionAction::PartialUpdated { mode, text, .. } =
                        machine.partial(session_id, text)
                    {
                        let kind = match machine.state() {
                            SessionState::Transcribing { .. } => BubbleKind::Transcribing,
                            _ => BubbleKind::Recording,
                        };
                        let tail: String = text
                            .chars()
                            .rev()
                            .take(40)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        let visible = match mode {
                            SessionMode::Dictation => tail,
                            SessionMode::System => format!("System: {tail}"),
                        };
                        ui.show(kind, &visible);
                    }
                }
                SidecarEvent::Final { session_id, text } => {
                    if let SessionAction::Finalized {
                        session_id,
                        mode,
                        text,
                    } = machine.finalize(session_id, text)
                    {
                        transcribe_deadline = None;
                        if text.is_empty() {
                            let summary = audio_stats
                                .as_ref()
                                .map(SessionAudioStats::summary)
                                .unwrap_or_else(|| "audio statistics unavailable".to_string());
                            logging::warn(&format!(
                                "session {session_id}: ASR backend returned an empty transcript ({summary})"
                            ));
                        } else if mode == SessionMode::System {
                            logging::info(&format!(
                                "session {session_id}: System transcript captured ({} characters)",
                                text.chars().count()
                            ));
                        } else {
                            logging::info(&format!(
                                "session {session_id}: final transcript: {}",
                                transcript_for_log(&text, settings.log_transcripts)
                            ));
                        }
                        last_activity = Instant::now();
                        audio_stats = None;
                        if let Some(timing) = timing.as_mut() {
                            timing.final_at = Some(Instant::now());
                            let release_to_final = timing.released_at.elapsed().as_millis();
                            let asr_turnaround = timing.finish_sent_at.map(|sent| {
                                timing.final_at.unwrap().duration_since(sent).as_millis()
                            });
                            logging::info(&format!(
                                "session {session_id}: release-to-final {release_to_final}ms{}",
                                asr_turnaround
                                    .map(|ms| format!(", ASR turnaround {ms}ms"))
                                    .unwrap_or_default(),
                            ));
                        }
                        if mode == SessionMode::System {
                            let outcome = crate::system_mode::present_live_plan(
                                &text,
                                settings.system_llm_fallback_enabled,
                            );
                            logging::info(&format!(
                                "session {session_id}: System route completed (matched={})",
                                outcome.intent.is_some()
                            ));
                            match outcome.intent {
                                Some(
                                    intent @ (SystemIntent::OpenTarget {
                                        hint: TargetHint::Application | TargetHint::Auto,
                                        ..
                                    }
                                    | SystemIntent::OpenTargetWithApplication { .. }
                                    | SystemIntent::FindFile { .. }
                                    | SystemIntent::OpenFileByQuery { .. }
                                    | SystemIntent::RevealFileByQuery { .. }
                                    | SystemIntent::OpenFolder { .. }),
                                ) => {
                                    ui.show(
                                        BubbleKind::Transcribing,
                                        "System — searching targets...",
                                    );
                                    active_system_resolution = Some(session_id);
                                    if let Err(error) =
                                        system_worker.send(SystemJob::DispatchFindTargets {
                                            session_id,
                                            transcript: text.clone(),
                                            intent,
                                        })
                                    {
                                        active_system_resolution = None;
                                        show_error(&ui, &error, &mut bubble_hide_at);
                                    }
                                }
                                Some(SystemIntent::OpenBrowserAndUrl { browser_query, .. }) => {
                                    ui.show(
                                        BubbleKind::Transcribing,
                                        "System — finding selected browser...",
                                    );
                                    active_system_resolution = Some(session_id);
                                    let intent = SystemIntent::OpenTarget {
                                        query: browser_query,
                                        hint: TargetHint::Application,
                                    };
                                    if let Err(error) =
                                        system_worker.send(SystemJob::DispatchFindTargets {
                                            session_id,
                                            transcript: text.clone(),
                                            intent,
                                        })
                                    {
                                        active_system_resolution = None;
                                        show_error(&ui, &error, &mut bubble_hide_at);
                                    }
                                }
                                Some(SystemIntent::OpenUrl { spoken_url })
                                | Some(SystemIntent::OpenTarget {
                                    query: spoken_url,
                                    hint: TargetHint::Url,
                                }) => {
                                    let navigation = ValidatedHttpUrl::parse_spoken(&spoken_url)
                                        .map(|url| {
                                            (
                                                CapabilityInput::Native(
                                                    NativeCapabilityCall::OpenUrl {
                                                        url,
                                                        browser: None,
                                                    },
                                                ),
                                                format!("open {spoken_url}"),
                                            )
                                        });
                                    present_navigation_confirmation(
                                        navigation,
                                        session_id,
                                        &text,
                                        &ui,
                                        &mut pending_system,
                                        &mut bubble_hide_at,
                                    );
                                }
                                Some(SystemIntent::WebSearch { query }) => {
                                    let navigation = Ok((
                                        CapabilityInput::Native(NativeCapabilityCall::WebSearch {
                                            query: query.clone(),
                                            browser: None,
                                        }),
                                        format!("search the web for {query}"),
                                    ));
                                    present_navigation_confirmation(
                                        navigation,
                                        session_id,
                                        &text,
                                        &ui,
                                        &mut pending_system,
                                        &mut bubble_hide_at,
                                    );
                                }
                                Some(_) => show_error(
                                    &ui,
                                    "That System action is not available yet",
                                    &mut bubble_hide_at,
                                ),
                                None => show_error(&ui, &outcome.message, &mut bubble_hide_at),
                            }
                            // Raw ASR text is never polished, pasted, or sent
                            // to a command interpreter. Only a resolved typed
                            // action can proceed, after explicit UI selection.
                            timing = None;
                            focused_class = None;
                            continue;
                        }
                        let raw_text = text.clone();
                        let mut output = if settings.polish_enabled {
                            let style = resolve_style(
                                focused_class
                                    .as_ref()
                                    .map(|(instance, class)| (instance.as_str(), class.as_str())),
                                &settings.polish.app_styles,
                                settings.polish.style,
                            );
                            let mut config = std::borrow::Cow::Borrowed(&settings.polish);
                            if style != config.style {
                                logging::info(&format!(
                                    "session {session_id}: style {style:?} for focused window class {:?}",
                                    focused_class
                                        .as_ref()
                                        .map(|(_, class)| class.as_str())
                                        .unwrap_or("<unknown>")
                                ));
                                config.to_mut().style = style;
                            }
                            let outcome = polish(&text, &config);
                            for stage in &outcome.trace {
                                logging::info(&format!(
                                    "session {session_id}: polish {}: {} -> {}",
                                    stage.stage,
                                    transcript_for_log(&stage.before, settings.log_transcripts),
                                    transcript_for_log(&stage.after, settings.log_transcripts)
                                ));
                            }
                            outcome.text
                        } else {
                            text
                        };
                        let mut streaming_inserted = false;
                        if settings.llm_polish_enabled
                            && !raw_text.trim().is_empty()
                            && let Some(client) = llm_polish.as_mut()
                        {
                            ui.show(BubbleKind::Transcribing, "polishing...");
                            let llm_input = output.clone();
                            let stream_insert = settings.llm_polish_stream_insert;
                            let first = std::cell::Cell::new(true);
                            let dispatched = std::cell::Cell::new(false);
                            let ui_tx_ref = &ui_tx;
                            let mut on_chunk = |delta: &str| {
                                if !stream_insert {
                                    return;
                                }
                                let is_first = first.get();
                                first.set(false);
                                dispatched.set(true);
                                let _ = ui_tx_ref.send(UiCommand::InsertStreamChunk {
                                    session_id,
                                    first: is_first,
                                    delta: delta.to_string(),
                                });
                            };
                            match client.polish_stream(
                                session_id,
                                &llm_input,
                                settings.llm_polish_timeout_ms,
                                &mut on_chunk,
                            ) {
                                Ok(outcome) => {
                                    let ttft = outcome.diagnostics.ttft_ms;
                                    let streamed = outcome.diagnostics.streamed == Some(true);
                                    let chunks = outcome.diagnostics.stream_chunks.unwrap_or(0);
                                    logging::info(&format!(
                                        "session {session_id}: llm polish accepted in {}ms (ttft {}ms, streamed={} {}chunks){}: {} -> {}",
                                        outcome.latency_ms,
                                        ttft.map(|ms| ms.to_string()).unwrap_or_else(|| "?".into()),
                                        streamed,
                                        chunks,
                                        format_llm_diagnostics(&outcome.diagnostics),
                                        transcript_for_log(&llm_input, settings.log_transcripts),
                                        transcript_for_log(&outcome.text, settings.log_transcripts)
                                    ));
                                    if stream_insert && dispatched.get() {
                                        // Progressive insertion already typed
                                        // the text; finalize the streaming
                                        // session without a second atomic
                                        // paste. The typed text is kept as
                                        // authoritative (a content-loss
                                        // guard revert after streaming is an
                                        // accepted edge; see
                                        // docs/llm-polish-streaming-plan.md).
                                        let sanitized_outcome = sanitize_for_insertion(
                                            &outcome.text,
                                            settings.allow_enter_and_tab,
                                        );
                                        if let Some(timing) = timing.as_mut() {
                                            timing.polish_done_at = Some(Instant::now());
                                            timing.insert_dispatched_at = Some(Instant::now());
                                        }
                                        if sanitized_outcome.is_empty() {
                                            logging::info(&format!(
                                                "session {session_id}: empty result"
                                            ));
                                            ui.hide();
                                        } else {
                                            let _ = ui_tx.send(UiCommand::InsertStreamEnd {
                                                session_id,
                                                final_text: sanitized_outcome,
                                                streamed_ok: true,
                                            });
                                        }
                                        streaming_inserted = true;
                                    } else {
                                        output = outcome.text;
                                    }
                                }
                                Err(error) => {
                                    logging::warn(&format!(
                                        "session {session_id}: llm polish skipped: {error}"
                                    ));
                                    if stream_insert && dispatched.get() {
                                        // Chunks already typed; finalize what
                                        // we have even though the call erred
                                        // (e.g. timeout past the last token).
                                        // The partial typed text is kept as-is:
                                        // pasting `output` again would
                                        // DUPLICATE what was already typed,
                                        // so we keep the typed (possibly
                                        // truncated) text and log it.
                                        logging::warn(&format!(
                                            "session {session_id}: streaming insert kept partial text after llm error; result may be truncated"
                                        ));
                                        if let Some(timing) = timing.as_mut() {
                                            timing.polish_done_at = Some(Instant::now());
                                            timing.insert_dispatched_at = Some(Instant::now());
                                        }
                                        let _ = ui_tx.send(UiCommand::InsertStreamEnd {
                                            session_id,
                                            final_text: output.clone(),
                                            streamed_ok: true,
                                        });
                                        streaming_inserted = true;
                                    }
                                }
                            }
                        }
                        if streaming_inserted {
                            // Streaming insertion already dispatched the text;
                            // skip the standard atomic paste path below.
                        } else {
                            let sanitized =
                                sanitize_for_insertion(&output, settings.allow_enter_and_tab);
                            if let Some(timing) = timing.as_mut() {
                                timing.polish_done_at = Some(Instant::now());
                            }
                            if sanitized.is_empty() {
                                logging::info(&format!("session {session_id}: empty result"));
                                ui.hide();
                            } else {
                                if let Some(timing) = timing.as_mut() {
                                    timing.insert_dispatched_at = Some(Instant::now());
                                }
                                let _ = ui_tx.send(UiCommand::Insert {
                                    session_id,
                                    text: sanitized,
                                });
                            }
                        }
                    }
                }
                SidecarEvent::Error {
                    session_id,
                    message,
                } => {
                    if message == "superseded" {
                        logging::warn(&format!("sidecar superseded session {session_id:?}"));
                    } else if session_id.is_none() || session_id == machine.current_session() {
                        if let SessionAction::Failed { message } = machine.fail(message) {
                            logging::error(&format!("sidecar error: {message}"));
                            show_error(&ui, &message, &mut bubble_hide_at);
                        }
                        timing = None;
                        transcribe_deadline = None;
                        audio_stats = None;
                    } else {
                        logging::warn(&format!(
                            "sidecar error for stale session {session_id:?}: {message}"
                        ));
                    }
                }
                SidecarEvent::SystemSelection { .. } | SidecarEvent::SystemCancelled { .. } => {
                    logging::warn("unexpected System UI event from ASR sidecar");
                }
            },
            Ok(DaemonEvent::Sidecar(SidecarMessage::Garbage { line })) => {
                logging::warn(&format!("ignored non-protocol sidecar output: {line}"));
            }
            Ok(DaemonEvent::Sidecar(SidecarMessage::Closed)) => {
                handle_sidecar_loss(
                    "ASR sidecar exited",
                    &mut machine,
                    &mut sidecar,
                    &mut respawn_at,
                    &mut respawn_backoff,
                    &ui,
                    &mut bubble_hide_at,
                    &mut timing,
                    &mut transcribe_deadline,
                    &mut sidecar_ready,
                    &mut llm_post_asr_warmed,
                    llm_polish.is_some(),
                );
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(SidecarEvent::Ready { backend }))) => {
                ui.overlay_ready = true;
                overlay_ever_ready = true;
                overlay_backoff = SIDECAR_BACKOFF_START;
                logging::info(&format!("overlay UI ready ({backend})"));
                // The overlay usually comes up before ASR does; show the
                // current state right away instead of a blank screen.
                let current = health.current();
                ui.health(current);
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(SidecarEvent::SystemSelection {
                session_id,
                suggestion_id,
            }))) => {
                let selection = pending_system
                    .as_mut()
                    .ok_or_else(|| "there is no active System suggestion set".to_string())
                    .and_then(|pending| pending.select(session_id, &suggestion_id));
                match selection {
                    Ok(SelectedSystemAction::Target(action)) => {
                        let display_name = action.display_name().to_string();
                        pending_system = None;
                        ui.dismiss_system_palette(session_id);
                        ui.show(
                            BubbleKind::Transcribing,
                            &format!("System — opening {display_name}..."),
                        );
                        if let Err(error) =
                            system_worker.send(SystemJob::DispatchOpenTarget { session_id, action })
                        {
                            show_error(&ui, &error, &mut bubble_hide_at);
                        }
                    }
                    Ok(SelectedSystemAction::Navigation { input, title }) => {
                        pending_system = None;
                        ui.dismiss_system_palette(session_id);
                        ui.show(BubbleKind::Transcribing, &format!("System — {title}..."));
                        if let Err(error) =
                            system_worker.send(SystemJob::DispatchNavigation { session_id, input })
                        {
                            show_error(&ui, &error, &mut bubble_hide_at);
                        }
                    }
                    Ok(SelectedSystemAction::BrowserNavigation { action, url }) => {
                        pending_system = None;
                        ui.dismiss_system_palette(session_id);
                        ui.show(
                            BubbleKind::Transcribing,
                            "System — opening selected browser...",
                        );
                        if let Err(error) =
                            system_worker.send(SystemJob::DispatchSelectedBrowserNavigation {
                                session_id,
                                action,
                                url,
                            })
                        {
                            show_error(&ui, &error, &mut bubble_hide_at);
                        }
                    }
                    Err(error) => {
                        logging::warn(&format!(
                            "ignored invalid System selection for session {session_id}: {error}"
                        ));
                    }
                }
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(SidecarEvent::SystemCancelled {
                session_id,
            }))) => {
                let is_current = pending_system
                    .as_ref()
                    .is_some_and(|pending| pending.session_id() == session_id);
                if is_current {
                    pending_system = None;
                    ui.dismiss_system_palette(session_id);
                    ui.hide();
                    system_worker.cancel_session(session_id);
                    logging::info(&format!(
                        "session {session_id}: System suggestions dismissed"
                    ));
                } else {
                    logging::warn(&format!(
                        "ignored cancellation for stale System session {session_id}"
                    ));
                }
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Event(event))) => {
                logging::warn(&format!("unexpected overlay event: {event:?}"));
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Garbage { line })) => {
                logging::warn(&format!("ignored non-protocol overlay output: {line}"));
            }
            Ok(DaemonEvent::Overlay(SidecarMessage::Closed)) => {
                pending_system = None;
                system_worker.invalidate_sessions();
                if ui.overlay.is_none() {
                    // Already torn down (shutdown path); nothing to do.
                } else if overlay_ever_ready {
                    logging::warn("overlay UI exited; respawning");
                    ui.overlay = None;
                    ui.overlay_ready = false;
                    overlay_respawn_at = Some(Instant::now() + overlay_backoff);
                    overlay_backoff = (overlay_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                } else {
                    // Never came up — most likely GTK4 is not installed.
                    // Permanent fallback beats a respawn loop of noise.
                    logging::warn(
                        "overlay UI exited before becoming ready (GTK4 missing?); using the native X11 bubble",
                    );
                    ui.overlay = None;
                    ui.overlay_ready = false;
                }
            }
            Ok(DaemonEvent::System(SystemWorkerEvent::TargetFindDispatched {
                session_id,
                transcript,
                intent,
                observation,
                candidates,
            })) => {
                if active_system_resolution != Some(session_id) {
                    logging::warn(&format!(
                        "ignored target results for stale System session {session_id}"
                    ));
                    continue;
                }
                active_system_resolution = None;
                match observation {
                    observation if !observation.is_success() => {
                        logging::error(&format!(
                            "session {session_id}: target discovery failed ({})",
                            "native_dispatch_failed"
                        ));
                        show_error(
                            &ui,
                            "Could not search Voice Spotlight targets",
                            &mut bubble_hide_at,
                        );
                    }
                    _ => {
                        let browser_navigation = match route_deterministically(&transcript) {
                            RouteOutcome::Matched {
                                intent: SystemIntent::OpenBrowserAndUrl { spoken_url, .. },
                                ..
                            } => ValidatedHttpUrl::parse_spoken(&spoken_url).ok(),
                            _ => None,
                        };
                        let pending =
                            PendingSuggestionSet::build(session_id, &intent, candidates, 5);
                        if pending.suggestions().is_empty() {
                            logging::info(&format!(
                                "session {session_id}: target resolution returned no matches"
                            ));
                            show_error(&ui, "No matching target found", &mut bubble_hide_at);
                            continue;
                        }
                        let suggestions = pending
                            .suggestions()
                            .iter()
                            .map(|suggestion| OverlaySuggestion {
                                suggestion_id: suggestion.suggestion_id.clone(),
                                title: suggestion.title.clone(),
                                subtitle: suggestion.subtitle.clone(),
                                action_label: suggestion_action_label(suggestion.action).into(),
                            })
                            .collect::<Vec<_>>();
                        let suggestion_count = suggestions.len();
                        ui.hide();
                        if ui.show_system_palette(session_id, transcript, suggestions) {
                            pending_system = Some(match browser_navigation {
                                Some(url) => {
                                    PendingSystemSelection::BrowserNavigation { pending, url }
                                }
                                None => PendingSystemSelection::Targets(pending),
                            });
                            logging::info(&format!(
                                "session {session_id}: presented {suggestion_count} System suggestion(s); awaiting explicit selection"
                            ));
                        } else {
                            show_error(
                                &ui,
                                "System suggestions UI is unavailable",
                                &mut bubble_hide_at,
                            );
                        }
                    }
                }
            }
            Ok(DaemonEvent::System(SystemWorkerEvent::TargetOpenDispatched {
                session_id,
                elapsed_ms,
                terminal,
                observation,
            })) => {
                if !system_worker.is_active_session(session_id) {
                    logging::warn(&format!(
                        "ignored target-open result for stale System session {session_id}"
                    ));
                    continue;
                }
                if !matches!(terminal, sunoto_system::PlanTerminalState::Completed) {
                    let terminal_kind = match &terminal {
                        sunoto_system::PlanTerminalState::Cancelled => "cancelled",
                        sunoto_system::PlanTerminalState::TimedOut { .. } => "timed_out",
                        sunoto_system::PlanTerminalState::Rejected { .. } => "rejected",
                        sunoto_system::PlanTerminalState::Failed { .. } => "failed",
                        sunoto_system::PlanTerminalState::Completed => "completed",
                    };
                    logging::warn(&format!(
                        "session {session_id}: System plan stopped before completion ({terminal_kind})"
                    ));
                    if !matches!(terminal, sunoto_system::PlanTerminalState::Cancelled) {
                        show_error(&ui, "Could not open target", &mut bubble_hide_at);
                    }
                    system_worker.cancel_session(session_id);
                    continue;
                }
                match observation {
                    observation if observation.is_success() => {
                        let result = match observation.evidence {
                            sunoto_system::ObservationEvidence::ApplicationLaunched {
                                display_name,
                                native_result,
                            } => (display_name, native_result, "launch_application"),
                            sunoto_system::ObservationEvidence::LocalTargetOpened {
                                display_name,
                                native_result,
                                ..
                            } => (display_name, native_result, "open_local_target"),
                            sunoto_system::ObservationEvidence::LocalTargetRevealed {
                                display_name,
                                native_result,
                            } => (display_name, native_result, "reveal_local_target"),
                            _ => {
                                logging::error(&format!(
                                    "session {session_id}: native dispatcher returned no target success evidence"
                                ));
                                show_error(&ui, "Could not open target", &mut bubble_hide_at);
                                system_worker.cancel_session(session_id);
                                continue;
                            }
                        };
                        logging::info(&format!(
                            "session {session_id}: System audit action={} policy=selected outcome=success latency_ms={elapsed_ms} backend={}",
                            result.2, result.1,
                        ));
                        ui.show(BubbleKind::Transcribing, &format!("Opened {}", result.0));
                        bubble_hide_at = Some(Instant::now() + ERROR_BUBBLE_VISIBLE);
                    }
                    _ => {
                        logging::error(&format!(
                            "session {session_id}: System audit action=open_target policy=selected outcome=failure latency_ms={elapsed_ms} error_class={}",
                            "native_dispatch_failed",
                        ));
                        show_error(&ui, "Could not open target", &mut bubble_hide_at);
                    }
                }
                system_worker.cancel_session(session_id);
            }
            Ok(DaemonEvent::System(SystemWorkerEvent::NavigationComplete {
                session_id,
                elapsed_ms,
                terminal,
                observation,
            })) => {
                if !system_worker.is_active_session(session_id) {
                    logging::warn(&format!(
                        "ignored navigation result for stale System session {session_id}"
                    ));
                    continue;
                }
                match (&terminal, observation) {
                    (sunoto_system::PlanTerminalState::Completed, observation)
                        if observation.is_success() =>
                    {
                        if let sunoto_system::ObservationEvidence::UrlOpened {
                            url,
                            native_result,
                        } = observation.evidence
                        {
                            logging::info(&format!(
                                "session {session_id}: System audit action=url_open policy=selected outcome=success latency_ms={elapsed_ms} backend={native_result}"
                            ));
                            ui.show(BubbleKind::Transcribing, &format!("Opened {url}"));
                            bubble_hide_at = Some(Instant::now() + ERROR_BUBBLE_VISIBLE);
                        } else {
                            show_error(&ui, "Could not open web target", &mut bubble_hide_at);
                        }
                    }
                    (sunoto_system::PlanTerminalState::Cancelled, _) => {}
                    _ => {
                        logging::warn(&format!(
                            "session {session_id}: System navigation failed before verification"
                        ));
                        show_error(&ui, "Could not open web target", &mut bubble_hide_at);
                    }
                }
                system_worker.cancel_session(session_id);
            }
            Ok(DaemonEvent::FocusClass(class)) => {
                // Logged per session: when text "disappears", the first
                // question is always which window actually received it.
                match class.as_ref() {
                    Some((instance, class_name)) => logging::info(&format!(
                        "insertion target at release: {instance:?} / {class_name:?}"
                    )),
                    None => logging::warn(
                        "insertion target at release has no WM_CLASS (text may land in a non-text window)",
                    ),
                }
                focused_class = class;
            }
            Ok(DaemonEvent::Ui(report)) => {
                match report.result {
                    Ok(outcome) => {
                        let insert_ms = report.insert_duration.as_millis();
                        if let Some(timing) = timing
                            .as_ref()
                            .filter(|timing| timing.session_id == report.session_id)
                        {
                            let now = Instant::now();
                            let recording = timing
                                .released_at
                                .duration_since(timing.pressed_at)
                                .as_millis();
                            let release_to_finish_sent = timing
                                .finish_sent_at
                                .map(|t| t.duration_since(timing.released_at).as_millis());
                            let asr_turnaround = match (timing.finish_sent_at, timing.final_at) {
                                (Some(sent), Some(final_)) => {
                                    Some(final_.duration_since(sent).as_millis())
                                }
                                _ => None,
                            };
                            let release_to_final = timing
                                .final_at
                                .map(|f| f.duration_since(timing.released_at).as_millis());
                            let polish_ms = match (timing.final_at, timing.polish_done_at) {
                                (Some(final_), Some(done)) => {
                                    Some(done.duration_since(final_).as_millis())
                                }
                                _ => None,
                            };
                            let dispatch_ms =
                                match (timing.polish_done_at, timing.insert_dispatched_at) {
                                    (Some(done), Some(dispatched)) => {
                                        Some(dispatched.duration_since(done).as_millis())
                                    }
                                    _ => None,
                                };
                            let insert_wait_ms = match (timing.insert_dispatched_at, now) {
                                (Some(dispatched), _) => {
                                    Some(now.duration_since(dispatched).as_millis())
                                }
                                _ => None,
                            };
                            let release_to_insertion =
                                now.duration_since(timing.released_at).as_millis();
                            logging::info(&format!(
                                "session {}: inserted via {:?}; timing breakdown: recorded {recording}ms, release->finish_sent {}ms, ASR turnaround {}ms, release->final {}ms, polish {}ms, dispatch {}ms, insert {insert_ms}ms (waited {}ms for UI thread), release->insertion {release_to_insertion}ms",
                                report.session_id,
                                outcome,
                                release_to_finish_sent.unwrap_or(0),
                                asr_turnaround
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                release_to_final
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                polish_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                dispatch_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                                insert_wait_ms
                                    .map(|ms| ms.to_string())
                                    .unwrap_or_else(|| "?".into()),
                            ));
                        } else {
                            logging::info(&format!(
                                "session {}: inserted via {:?} in {insert_ms}ms",
                                report.session_id, outcome
                            ));
                        }
                        match outcome {
                            InsertionOutcome::ClipboardOnly => show_error(
                                &ui,
                                "focus changed; result is in the clipboard",
                                &mut bubble_hide_at,
                            ),
                            InsertionOutcome::SecureField => {
                                logging::warn(&format!(
                                    "session {}: focus is a password field; nothing inserted or copied",
                                    report.session_id
                                ));
                                show_error(
                                    &ui,
                                    "password field: nothing inserted",
                                    &mut bubble_hide_at,
                                );
                            }
                            InsertionOutcome::Typed | InsertionOutcome::Pasted => ui.hide(),
                        }
                    }
                    Err(message) => {
                        logging::error(&format!(
                            "session {}: insertion failed: {message}",
                            report.session_id
                        ));
                        show_error(&ui, "insertion failed", &mut bubble_hide_at);
                    }
                }
                timing = None;
            }
            Ok(DaemonEvent::ControlPolish { text, mut response }) => {
                let result = if matches!(machine.state(), SessionState::Idle) {
                    control_polish_response(&settings, &mut llm_polish, llm_post_asr_warmed, &text)
                } else {
                    serde_json::json!({
                        "type": "polish_result",
                        "ok": false,
                        "error": "daemon is busy with a dictation session",
                    })
                };
                if serde_json::to_writer(&mut response, &result).is_ok() {
                    let _ = response.write_all(b"\n");
                }
            }
            Ok(DaemonEvent::ControlSystemPlan { text, mut response }) => {
                match crate::system_mode::plan_json_with_llm_fallback(
                    &text,
                    settings.system_llm_fallback_enabled,
                ) {
                    Ok(payload) => {
                        let _ = response.write_all(payload.as_bytes());
                        let _ = response.write_all(b"\n");
                    }
                    Err(error) => {
                        let _ = serde_json::to_writer(
                            &mut response,
                            &serde_json::json!({
                                "type": "error",
                                "ok": false,
                                "error": format!("cannot serialize System plan: {error}"),
                            }),
                        );
                        let _ = response.write_all(b"\n");
                    }
                }
            }
            Ok(DaemonEvent::ControlStatus { mut response }) => {
                let current = health.current();
                let hotkey = if !blocked_modes.is_empty() {
                    "blocked"
                } else if hotkey_verified {
                    "verified"
                } else {
                    "unknown"
                };
                let polish = if llm_polish.is_none() {
                    if settings.llm_polish_enabled {
                        "disabled_after_failure"
                    } else {
                        "disabled"
                    }
                } else if llm_post_asr_warmed {
                    "ready"
                } else {
                    "warming"
                };
                let payload = serde_json::json!({
                    "type": "status",
                    "ok": true,
                    "health": current.name(),
                    "detail": current.caption(),
                    "ready": current.is_ready(),
                    "hotkey": hotkey,
                    "hotkey_reason": if hotkey == "blocked" { hotkey_block_reason() } else { String::new() },
                    "microphone": mic.name(),
                    "asr": if sidecar_ready { "ready" } else { "loading" },
                    "asr_backend": settings.backend,
                    "polish": polish,
                    "overlay": if ui.overlay_active() { "ready" } else if ui.overlay.is_some() { "starting" } else { "native" },
                    "session": match machine.state() {
                        SessionState::Idle => "idle",
                        SessionState::Recording { .. } => "recording",
                        SessionState::Transcribing { .. } => "transcribing",
                    },
                    "shortcut": settings.shortcut,
                    "pid": std::process::id(),
                    "uptime_s": started_at.elapsed().as_secs(),
                });
                let _ = serde_json::to_writer(&mut response, &payload);
                let _ = response.write_all(b"\n");
            }
            Ok(DaemonEvent::Fatal(message)) => {
                exit_error = Some(message);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                exit_error = Some("event channel disconnected".into());
                break;
            }
        }

        // Watchdogs and deferred work, evaluated on every loop pass.
        if !blocked_modes.is_empty() && last_preflight_check.elapsed() >= Duration::from_secs(1) {
            last_preflight_check = Instant::now();
            // Signal 1: the permission database changed, so the user just
            // flipped a switch. Wait a moment for a second flip, then
            // relaunch; a stale process cannot use the grant otherwise.
            let stamp = crate::setup::permission_db_stamp();
            if stamp.is_some() && stamp != permission_db_stamp {
                permission_db_stamp = stamp;
                logging::info("privacy settings changed; relaunching shortly to apply them");
                relaunch_due = Some(Instant::now() + Duration::from_millis(1500));
            }
            // Signal 2: the preflights themselves report granted while
            // delivery is still blocked (seen when the grant existed before
            // this process started).
            let (listen, accessibility) = permission_preflights();
            if listen && accessibility {
                let seen = *grants_seen_at.get_or_insert_with(Instant::now);
                if seen.elapsed() >= Duration::from_secs(2) && relaunch_due.is_none() {
                    relaunch_due = Some(Instant::now());
                }
            } else {
                grants_seen_at = None;
            }
        } else if blocked_modes.is_empty() {
            grants_seen_at = None;
            relaunch_due = None;
        }
        if let Some(due) = relaunch_due
            && Instant::now() >= due
        {
            relaunch_due = None;
            if crate::setup::relaunch_self_if_bundled() {
                logging::info("relaunching so the hotkey can use the new permissions");
                STOP_REQUESTED.store(true, Ordering::SeqCst);
            }
        }
        if matches!(machine.state(), SessionState::Idle) {
            let idle_for = last_activity.elapsed();
            if keepalive_active
                && idle_for >= KEEPALIVE_GRACE
                && let Some(client) = llm_polish.as_mut()
            {
                keepalive_active = false;
                if let Err(error) = client.keepalive_stop() {
                    logging::warn(&format!("LLM keepalive stop failed: {error}"));
                }
            }
            if settings.capture_idle_stop_secs > 0
                && !capture_idle
                && idle_for >= Duration::from_secs(settings.capture_idle_stop_secs)
            {
                capture_idle = true;
                mic = MicState::Idle;
                capture_wanted.store(false, Ordering::SeqCst);
                preroll.clear();
                logging::info(&format!(
                    "microphone released after {}s idle; it reopens on the next press",
                    settings.capture_idle_stop_secs
                ));
            }
        }
        if let Some(next) = health.refresh(health_inputs(
            sidecar_ready,
            llm_post_asr_warmed,
            &blocked_modes,
            mic,
            capture_requested_at.is_some(),
        )) {
            publish_health(next, &mut ui, &settings);
        }
        if let Some(deadline) = transcribe_deadline
            && Instant::now() >= deadline
        {
            transcribe_deadline = None;
            if let SessionState::Transcribing { session_id, .. } = *machine.state() {
                if let Some(client) = sidecar.as_mut() {
                    let _ = client.send(&SidecarRequest::CancelSession { session_id });
                }
                machine.fail("ASR timed out");
                logging::error(&format!("session {session_id}: ASR timed out"));
                show_error(&ui, "ASR timed out", &mut bubble_hide_at);
                timing = None;
                audio_stats = None;
            }
        }
        if let Some(hide_at) = bubble_hide_at
            && Instant::now() >= hide_at
        {
            bubble_hide_at = None;
            ui.hide();
        }
        if ui.overlay.is_none()
            && let Some(at) = overlay_respawn_at
            && Instant::now() >= at
        {
            overlay_respawn_at = None;
            match spawn_overlay(&settings, events_tx.clone()) {
                Ok(handle) => {
                    logging::info("overlay UI respawned");
                    ui.overlay = Some(handle);
                }
                Err(error) => {
                    logging::warn(&format!("overlay respawn failed: {error}"));
                    overlay_backoff = (overlay_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                    overlay_respawn_at = Some(Instant::now() + overlay_backoff);
                }
            }
        }
        if sidecar.is_none()
            && let Some(at) = respawn_at
            && Instant::now() >= at
        {
            respawn_at = None;
            match spawn_sidecar(&settings, events_tx.clone()) {
                Ok(client) => {
                    logging::info("ASR sidecar respawned");
                    sidecar = Some(client);
                }
                Err(error) => {
                    logging::error(&format!("sidecar respawn failed: {error}"));
                    respawn_backoff = (respawn_backoff * 2).min(SIDECAR_BACKOFF_CAP);
                    respawn_at = Some(Instant::now() + respawn_backoff);
                }
            }
        }
    }

    logging::info("shutting down");
    if let SessionState::Recording { session_id, .. }
    | SessionState::Transcribing { session_id, .. } = *machine.state()
        && let Some(client) = sidecar.as_mut()
    {
        let _ = client.send(&SidecarRequest::CancelSession { session_id });
    }
    ui.overlay_send(OverlayRequest::Shutdown);
    ui.overlay = None;
    system_worker.shutdown();
    let _ = ui_tx.send(UiCommand::Shutdown);
    control_stop.store(true, Ordering::SeqCst);
    hotkey_stop.store(true, Ordering::SeqCst);
    capture_stop.store(true, Ordering::SeqCst);
    drop(sidecar);
    let _ = ui_handle.join();
    let _ = control_handle.join();
    for hotkey_handle in hotkey_handles {
        let _ = hotkey_handle.join();
    }
    let _ = capture_handle.join();
    match exit_error {
        Some(message) => Err(message.into()),
        None => Ok(()),
    }
}

/// The post-ASR LLM warm-up. Blocks the loop for several seconds, so the
/// caller publishes the "warming" state first.
fn run_llm_warmup(
    llm_polish: &mut Option<llm_polish::LlmPolishClient>,
    settings: &Settings,
    llm_post_asr_warmed: &mut bool,
    ui: &UiFront,
    bubble_hide_at: &mut Option<Instant>,
) {
    let Some(client) = llm_polish.as_mut() else {
        *llm_post_asr_warmed = true;
        return;
    };
    match client.warmup(&llm_polish::WARMUP_TEXTS, settings.llm_polish_timeout_ms) {
        Ok(outcome) => {
            *llm_post_asr_warmed = true;
            logging::info(&format!(
                "LLM polish post-ASR warmup complete: {}",
                format_llm_warmup_summary(&outcome)
            ));
        }
        Err(error) => {
            logging::warn(&format!(
                "LLM polish disabled for this daemon run after post-ASR warmup failure: {error}"
            ));
            *llm_polish = None;
            *llm_post_asr_warmed = true;
            show_error(ui, "LLM polish unavailable", bubble_hide_at);
        }
    }
}

#[cfg(test)]
mod system_mode_integration_tests {
    use super::*;
    use sunoto_system::{
        ActionExecutor, ActionResult, ActionSuccessEvidence, ApplicationResolver,
        CapabilityDispatcher, FakePlanner, FixtureApplicationResolver, NativeCapabilityDispatcher,
        ObservationEvidence, PlanLimits, PlanTerminalState, PlannedCapabilityCall,
        PlannedCapabilityInput, ResolvedSystemAction, SystemOperationError, SystemPlan,
        SystemPlanRunner,
    };

    #[derive(Default)]
    struct BrowserFixture {
        resolver: FixtureApplicationResolver,
        url_calls: Vec<(String, String)>,
    }

    impl ApplicationResolver for BrowserFixture {
        fn application_candidates(
            &mut self,
        ) -> Result<Vec<sunoto_system::ActionCandidate>, SystemOperationError> {
            self.resolver.application_candidates()
        }

        fn target_candidates(
            &mut self,
            intent: &SystemIntent,
        ) -> Result<Vec<sunoto_system::ActionCandidate>, SystemOperationError> {
            self.resolver.target_candidates(intent)
        }
    }

    impl ActionExecutor for BrowserFixture {
        fn execute(
            &mut self,
            action: &ResolvedSystemAction,
        ) -> Result<ActionResult, SystemOperationError> {
            let evidence = match action {
                ResolvedSystemAction::LaunchApplication { .. } => {
                    ActionSuccessEvidence::ApplicationRunning
                }
                ResolvedSystemAction::OpenLocalTarget { reveal: true, .. } => {
                    ActionSuccessEvidence::LocalTargetRevealed
                }
                ResolvedSystemAction::OpenLocalTarget { .. } => {
                    ActionSuccessEvidence::LocalTargetOpened
                }
            };
            Ok(ActionResult {
                display_name: action.display_name().into(),
                detail: "unused application fixture action".into(),
                evidence,
            })
        }

        fn can_open_http_urls(
            &mut self,
            action: &ResolvedSystemAction,
        ) -> Result<bool, SystemOperationError> {
            Ok(action.display_name() == "Google Chrome")
        }

        fn open_url(
            &mut self,
            url: &ValidatedHttpUrl,
            browser: Option<&ResolvedSystemAction>,
        ) -> Result<ActionResult, SystemOperationError> {
            let browser = browser.ok_or_else(|| SystemOperationError::new("missing browser"))?;
            self.url_calls
                .push((url.as_str().into(), browser.display_name().into()));
            Ok(ActionResult {
                display_name: browser.display_name().into(),
                detail: "fixture native URL observation".into(),
                evidence: ActionSuccessEvidence::ApplicationRunning,
            })
        }
    }

    #[test]
    fn compound_system_route_requires_palette_selection_then_observes_selected_browser_url() {
        let presentation =
            crate::system_mode::present_live_plan("open Chrome and go to google dot com", false);
        let Some(SystemIntent::OpenBrowserAndUrl {
            browser_query,
            spoken_url,
        }) = presentation.intent
        else {
            panic!("compound route was not deterministic");
        };
        let url = ValidatedHttpUrl::parse_spoken(&spoken_url).unwrap();
        let find = CapabilityInput::Native(NativeCapabilityCall::FindApplication {
            query: browser_query,
        });
        let mut dispatcher = NativeCapabilityDispatcher::new(BrowserFixture::default());
        let found = dispatcher.dispatch(&find);
        assert!(found.is_success());
        let candidates = dispatcher.take_last_candidates();
        let intent = SystemIntent::OpenTarget {
            query: "Chrome".into(),
            hint: TargetHint::Application,
        };
        let mut palette = PendingSystemSelection::BrowserNavigation {
            pending: PendingSuggestionSet::build(77, &intent, candidates, 5),
            url: url.clone(),
        };
        let suggestion = match &palette {
            PendingSystemSelection::BrowserNavigation { pending, .. } => {
                pending.suggestions()[0].suggestion_id.clone()
            }
            _ => unreachable!(),
        };
        let SelectedSystemAction::BrowserNavigation { action, url } =
            palette.select(77, &suggestion).unwrap()
        else {
            panic!("palette did not produce browser navigation");
        };
        assert!(
            palette.select(77, &suggestion).is_err(),
            "selection must be one-time"
        );
        let CapabilityInput::Native(NativeCapabilityCall::OpenApplication { target }) =
            dispatcher.authorize_selected_action(&action).unwrap()
        else {
            panic!("selected application did not produce an opaque target");
        };
        let input = CapabilityInput::Native(NativeCapabilityCall::OpenUrl {
            url: url.clone(),
            browser: Some(target),
        });
        let plan = SystemPlan {
            goal: "Open Chrome and go to google.com".into(),
            steps: vec![PlannedCapabilityCall {
                id: "open-url".into(),
                input: PlannedCapabilityInput::Literal(input),
                depends_on: vec![],
            }],
            limits: PlanLimits::default(),
        };
        let mut planner = FakePlanner::returning(plan);
        let result = SystemPlanRunner::run(
            "Open Chrome and go to google.com",
            &mut planner,
            &mut dispatcher,
            &(),
        );
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert!(matches!(
            &result.observations[0].1.evidence,
            ObservationEvidence::UrlOpened { url: observed, .. } if observed == url.as_str()
        ));
        assert_eq!(
            dispatcher.into_inner().url_calls,
            vec![(url.as_str().into(), "Google Chrome".into())]
        );
    }

    #[test]
    fn voice_spotlight_project_and_editor_route_through_one_time_palette_and_runner() {
        let presentation =
            crate::system_mode::present_live_plan("open who-else-is-free in VS Code", false);
        let Some(intent @ SystemIntent::OpenTargetWithApplication { .. }) = presentation.intent
        else {
            panic!("project/editor route was not deterministic");
        };
        let mut dispatcher = NativeCapabilityDispatcher::new(BrowserFixture::default());
        let found =
            dispatcher.dispatch(&CapabilityInput::Native(NativeCapabilityCall::FindTarget {
                intent: intent.clone(),
            }));
        assert!(found.is_success(), "{found:?}");
        let candidates = dispatcher.take_last_candidates();
        let mut palette = PendingSystemSelection::Targets(PendingSuggestionSet::build(
            88, &intent, candidates, 10,
        ));
        let suggestion_id = match &palette {
            PendingSystemSelection::Targets(pending) => pending
                .suggestions()
                .iter()
                .find(|suggestion| suggestion.title == "Open who-else-is-free")
                .unwrap()
                .suggestion_id
                .clone(),
            _ => unreachable!(),
        };
        let SelectedSystemAction::Target(action) = palette.select(88, &suggestion_id).unwrap()
        else {
            panic!("palette did not produce a typed local-target action");
        };
        assert!(palette.select(88, &suggestion_id).is_err());
        assert!(matches!(
            &action,
            ResolvedSystemAction::OpenLocalTarget {
                application_display_name: Some(name),
                ..
            } if name == "Visual Studio Code"
        ));
        let input = dispatcher.authorize_selected_action(&action).unwrap();
        let plan = SystemPlan {
            goal: "Open project in selected editor".into(),
            steps: vec![PlannedCapabilityCall {
                id: "open-project".into(),
                input: PlannedCapabilityInput::Literal(input.clone()),
                depends_on: vec![],
            }],
            limits: PlanLimits::default(),
        };
        let mut planner = FakePlanner::returning(plan);
        let result = SystemPlanRunner::run(
            "Open who-else-is-free in VS Code",
            &mut planner,
            &mut dispatcher,
            &(),
        );
        assert_eq!(result.terminal, PlanTerminalState::Completed);
        assert!(matches!(
            &result.observations[0].1.evidence,
            ObservationEvidence::LocalTargetOpened {
                kind: sunoto_system::CandidateKind::Project,
                ..
            }
        ));
        assert!(!dispatcher.dispatch(&input).is_success());
    }
}

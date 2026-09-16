//! Blocking native System operations isolated from the daemon event loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sunoto_desktop::SystemPlatform;
use sunoto_system::{
    ActionCandidate, ActionExecutor, ApplicationResolver, CapabilityDispatcher, CapabilityInput,
    DispatchGuard, FakePlanner, NativeCapabilityDispatcher, PlanLimits, PlanTerminalState,
    PlannedCapabilityCall, PlannedCapabilityInput, ResolvedSystemAction, RunnerCancellation,
    SystemIntent, SystemPlan, SystemPlanRunner, TypedObservation,
};

use crate::daemon::DaemonEvent;

pub enum SystemJob {
    DispatchFindTargets {
        session_id: u64,
        transcript: String,
        intent: SystemIntent,
    },
    DispatchOpenTarget {
        session_id: u64,
        action: ResolvedSystemAction,
    },
    DispatchNavigation {
        session_id: u64,
        input: sunoto_system::CapabilityInput,
    },
    DispatchSelectedBrowserNavigation {
        session_id: u64,
        action: ResolvedSystemAction,
        url: sunoto_system::ValidatedHttpUrl,
    },
    Shutdown,
}

pub enum SystemWorkerEvent {
    TargetFindDispatched {
        session_id: u64,
        transcript: String,
        intent: SystemIntent,
        observation: TypedObservation,
        candidates: Vec<ActionCandidate>,
    },
    TargetOpenDispatched {
        session_id: u64,
        elapsed_ms: u128,
        terminal: PlanTerminalState,
        observation: TypedObservation,
    },
    NavigationComplete {
        session_id: u64,
        elapsed_ms: u128,
        terminal: PlanTerminalState,
        observation: TypedObservation,
    },
}

pub struct SystemWorker {
    jobs: Sender<SystemJob>,
    active_session: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

struct SessionDispatchGuard {
    active_session: Arc<AtomicU64>,
    session_id: u64,
    deadline: Instant,
}

impl SessionDispatchGuard {
    fn new(active_session: Arc<AtomicU64>, session_id: u64, timeout: Duration) -> Self {
        Self {
            active_session,
            session_id,
            deadline: Instant::now() + timeout,
        }
    }
}

impl DispatchGuard for SessionDispatchGuard {
    fn is_cancelled(&self) -> bool {
        self.active_session.load(Ordering::SeqCst) != self.session_id
    }

    fn deadline_exceeded(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

struct SessionCancellation {
    active_session: Arc<AtomicU64>,
    session_id: u64,
}

impl RunnerCancellation for SessionCancellation {
    fn is_cancelled(&self) -> bool {
        self.active_session.load(Ordering::SeqCst) != self.session_id
    }
}

fn run_navigation_plan<P: ApplicationResolver + ActionExecutor>(
    dispatcher: &mut NativeCapabilityDispatcher<P>,
    active_session: &Arc<AtomicU64>,
    session_id: u64,
    input: CapabilityInput,
) -> (PlanTerminalState, TypedObservation) {
    let capability = input.capability_id();
    if active_session.load(Ordering::SeqCst) != session_id {
        return (
            PlanTerminalState::Cancelled,
            TypedObservation::cancelled(capability),
        );
    }
    let plan = SystemPlan {
        goal: "Open validated web target".into(),
        steps: vec![PlannedCapabilityCall {
            id: "navigate".into(),
            input: PlannedCapabilityInput::Literal(input),
            depends_on: Vec::new(),
        }],
        limits: PlanLimits {
            max_steps: 1,
            whole_plan_timeout: Duration::from_secs(10),
            per_step_timeout: Duration::from_secs(10),
        },
    };
    let mut planner = FakePlanner::returning(plan);
    let cancellation = SessionCancellation {
        active_session: Arc::clone(active_session),
        session_id,
    };
    let run = SystemPlanRunner::run(
        "Open validated web target",
        &mut planner,
        dispatcher,
        &cancellation,
    );
    let terminal = run.terminal.clone();
    let observation = run
        .observations
        .into_iter()
        .last()
        .map(|(_, observation)| observation)
        .unwrap_or_else(|| match &terminal {
            PlanTerminalState::Cancelled => TypedObservation::cancelled(capability),
            PlanTerminalState::TimedOut { .. } => {
                TypedObservation::failed(capability, "capability deadline exceeded")
            }
            PlanTerminalState::Rejected { reason } | PlanTerminalState::Failed { reason, .. } => {
                TypedObservation::failed(capability, reason.clone())
            }
            PlanTerminalState::Completed => {
                TypedObservation::failed(capability, "completed plan returned no observation")
            }
        });
    (terminal, observation)
}

impl SystemWorker {
    pub fn spawn(events: Sender<DaemonEvent>, search_roots: Vec<std::path::PathBuf>) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let active_session = Arc::new(AtomicU64::new(0));
        let worker_session = Arc::clone(&active_session);
        let thread = std::thread::spawn(move || {
            let mut dispatcher =
                NativeCapabilityDispatcher::new(SystemPlatform::with_search_roots(search_roots));
            while let Ok(job) = receiver.recv() {
                let event = match job {
                    SystemJob::DispatchFindTargets {
                        session_id,
                        transcript,
                        intent,
                    } => {
                        let guard = SessionDispatchGuard::new(
                            Arc::clone(&worker_session),
                            session_id,
                            Duration::from_secs(5),
                        );
                        let input = if matches!(
                            intent,
                            SystemIntent::OpenTarget {
                                hint: sunoto_system::TargetHint::Application,
                                ..
                            }
                        ) {
                            sunoto_system::CapabilityInput::Native(
                                sunoto_system::NativeCapabilityCall::FindApplication {
                                    query: intent.query().into(),
                                },
                            )
                        } else {
                            sunoto_system::CapabilityInput::Native(
                                sunoto_system::NativeCapabilityCall::FindTarget {
                                    intent: intent.clone(),
                                },
                            )
                        };
                        let observation = dispatcher.dispatch_guarded(&input, &guard);
                        SystemWorkerEvent::TargetFindDispatched {
                            session_id,
                            transcript,
                            intent,
                            observation,
                            candidates: dispatcher.take_last_candidates(),
                        }
                    }
                    SystemJob::DispatchOpenTarget { session_id, action } => {
                        let started = Instant::now();
                        let capability = match action {
                            ResolvedSystemAction::LaunchApplication { .. } => {
                                sunoto_system::CapabilityId::ApplicationOpen
                            }
                            ResolvedSystemAction::OpenLocalTarget { .. } => {
                                sunoto_system::CapabilityId::TargetOpen
                            }
                        };
                        let (terminal, observation) = if worker_session.load(Ordering::SeqCst)
                            != session_id
                        {
                            (
                                PlanTerminalState::Cancelled,
                                TypedObservation::cancelled(capability),
                            )
                        } else {
                            match dispatcher.authorize_selected_action(&action) {
                                Ok(input) => {
                                    let plan = SystemPlan {
                                        goal: format!("Open {}", action.display_name()),
                                        steps: vec![PlannedCapabilityCall {
                                            id: "open-application".into(),
                                            input: PlannedCapabilityInput::Literal(input),
                                            depends_on: Vec::new(),
                                        }],
                                        limits: PlanLimits {
                                            max_steps: 1,
                                            whole_plan_timeout: Duration::from_secs(10),
                                            per_step_timeout: Duration::from_secs(10),
                                        },
                                    };
                                    let mut planner = FakePlanner::returning(plan);
                                    let cancellation = SessionCancellation {
                                        active_session: Arc::clone(&worker_session),
                                        session_id,
                                    };
                                    let run = SystemPlanRunner::run(
                                        action.display_name(),
                                        &mut planner,
                                        &mut dispatcher,
                                        &cancellation,
                                    );
                                    let terminal = run.terminal.clone();
                                    let observation = run
                                        .observations
                                        .into_iter()
                                        .last()
                                        .map(|(_, observation)| observation)
                                        .unwrap_or_else(|| match &terminal {
                                            PlanTerminalState::Cancelled => {
                                                TypedObservation::cancelled(capability)
                                            }
                                            PlanTerminalState::TimedOut { .. } => {
                                                TypedObservation::failed(
                                                    capability,
                                                    "capability deadline exceeded",
                                                )
                                            }
                                            PlanTerminalState::Rejected { reason }
                                            | PlanTerminalState::Failed { reason, .. } => {
                                                TypedObservation::failed(capability, reason.clone())
                                            }
                                            PlanTerminalState::Completed => {
                                                TypedObservation::failed(
                                                    capability,
                                                    "completed plan returned no observation",
                                                )
                                            }
                                        });
                                    (terminal, observation)
                                }
                                Err(error) => (
                                    PlanTerminalState::Rejected {
                                        reason: error.clone(),
                                    },
                                    TypedObservation::failed(capability, error),
                                ),
                            }
                        };
                        SystemWorkerEvent::TargetOpenDispatched {
                            session_id,
                            elapsed_ms: started.elapsed().as_millis(),
                            terminal,
                            observation,
                        }
                    }
                    SystemJob::DispatchNavigation { session_id, input } => {
                        let started = Instant::now();
                        let capability = input.capability_id();
                        let (terminal, observation) = if worker_session.load(Ordering::SeqCst)
                            != session_id
                        {
                            (
                                PlanTerminalState::Cancelled,
                                TypedObservation::cancelled(capability),
                            )
                        } else {
                            let plan = SystemPlan {
                                goal: "Open validated web target".into(),
                                steps: vec![PlannedCapabilityCall {
                                    id: "navigate".into(),
                                    input: PlannedCapabilityInput::Literal(input),
                                    depends_on: Vec::new(),
                                }],
                                limits: PlanLimits {
                                    max_steps: 1,
                                    whole_plan_timeout: Duration::from_secs(10),
                                    per_step_timeout: Duration::from_secs(10),
                                },
                            };
                            let mut planner = FakePlanner::returning(plan);
                            let cancellation = SessionCancellation {
                                active_session: Arc::clone(&worker_session),
                                session_id,
                            };
                            let run = SystemPlanRunner::run(
                                "Open validated web target",
                                &mut planner,
                                &mut dispatcher,
                                &cancellation,
                            );
                            let terminal = run.terminal.clone();
                            let observation = run
                                .observations
                                .into_iter()
                                .last()
                                .map(|(_, observation)| observation)
                                .unwrap_or_else(|| match &terminal {
                                    PlanTerminalState::Cancelled => {
                                        TypedObservation::cancelled(capability)
                                    }
                                    PlanTerminalState::TimedOut { .. } => TypedObservation::failed(
                                        capability,
                                        "capability deadline exceeded",
                                    ),
                                    PlanTerminalState::Rejected { reason }
                                    | PlanTerminalState::Failed { reason, .. } => {
                                        TypedObservation::failed(capability, reason.clone())
                                    }
                                    PlanTerminalState::Completed => TypedObservation::failed(
                                        capability,
                                        "completed plan returned no observation",
                                    ),
                                });
                            (terminal, observation)
                        };
                        SystemWorkerEvent::NavigationComplete {
                            session_id,
                            elapsed_ms: started.elapsed().as_millis(),
                            terminal,
                            observation,
                        }
                    }
                    SystemJob::DispatchSelectedBrowserNavigation {
                        session_id,
                        action,
                        url,
                    } => {
                        let started = Instant::now();
                        let capability = sunoto_system::CapabilityId::UrlOpen;
                        let input =
                            dispatcher
                                .authorize_selected_action(&action)
                                .and_then(|selected| {
                                    match selected {
                                sunoto_system::CapabilityInput::Native(
                                    sunoto_system::NativeCapabilityCall::OpenApplication { target },
                                ) => Ok(sunoto_system::CapabilityInput::Native(
                                    sunoto_system::NativeCapabilityCall::OpenUrl {
                                        url,
                                        browser: Some(target),
                                    },
                                )),
                                _ => Err(
                                    "selected browser did not produce an opaque application target"
                                        .into(),
                                ),
                            }
                                });
                        let (terminal, observation) = match input {
                            Ok(input) => run_navigation_plan(
                                &mut dispatcher,
                                &worker_session,
                                session_id,
                                input,
                            ),
                            Err(error) => (
                                PlanTerminalState::Rejected {
                                    reason: error.clone(),
                                },
                                TypedObservation::failed(capability, error),
                            ),
                        };
                        SystemWorkerEvent::NavigationComplete {
                            session_id,
                            elapsed_ms: started.elapsed().as_millis(),
                            terminal,
                            observation,
                        }
                    }
                    SystemJob::Shutdown => return,
                };
                if events.send(DaemonEvent::System(event)).is_err() {
                    return;
                }
            }
        });
        Self {
            jobs,
            active_session,
            thread: Some(thread),
        }
    }

    /// Invalidates queued or in-flight System work before starting another
    /// capture session. The worker checks this directly rather than waiting
    /// for a cancellation message behind the work it must cancel.
    pub fn invalidate_sessions(&self) {
        self.active_session.store(0, Ordering::SeqCst);
    }

    pub fn activate_session(&self, session_id: u64) {
        self.active_session.store(session_id, Ordering::SeqCst);
    }

    pub fn cancel_session(&self, session_id: u64) {
        let _ =
            self.active_session
                .compare_exchange(session_id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn is_active_session(&self, session_id: u64) -> bool {
        self.active_session.load(Ordering::SeqCst) == session_id
    }

    pub fn send(&self, job: SystemJob) -> Result<(), String> {
        self.jobs
            .send(job)
            .map_err(|_| "System worker is unavailable".into())
    }

    pub fn shutdown(mut self) {
        let _ = self.jobs.send(SystemJob::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

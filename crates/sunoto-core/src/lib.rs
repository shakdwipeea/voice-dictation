use std::collections::VecDeque;

/// Product mode attached to one capture/transcription session.
///
/// Dictation and System mode share the audio/ASR lifecycle, but their final
/// text has a different destination. Keeping the mode in the state machine
/// prevents a late ASR result from accidentally crossing that boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionMode {
    #[default]
    Dictation,
    System,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SessionState {
    #[default]
    Idle,
    Recording {
        session_id: u64,
        mode: SessionMode,
    },
    Transcribing {
        session_id: u64,
        mode: SessionMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    None,
    Started {
        session_id: u64,
        mode: SessionMode,
    },
    FinishRequested {
        session_id: u64,
        mode: SessionMode,
    },
    PartialUpdated {
        session_id: u64,
        mode: SessionMode,
        text: String,
    },
    Finalized {
        session_id: u64,
        mode: SessionMode,
        text: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Default)]
pub struct SessionMachine {
    state: SessionState,
    next_session_id: u64,
}

impl SessionMachine {
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn current_session(&self) -> Option<u64> {
        match self.state {
            SessionState::Idle => None,
            SessionState::Recording { session_id, .. }
            | SessionState::Transcribing { session_id, .. } => Some(session_id),
        }
    }

    pub fn current_mode(&self) -> Option<SessionMode> {
        match self.state {
            SessionState::Idle => None,
            SessionState::Recording { mode, .. } | SessionState::Transcribing { mode, .. } => {
                Some(mode)
            }
        }
    }

    /// Start a normal dictation session.
    ///
    /// Existing callers retain their original behavior; a dedicated System
    /// hotkey can opt into `press_mode(SessionMode::System)` explicitly.
    pub fn press(&mut self) -> SessionAction {
        self.press_mode(SessionMode::Dictation)
    }

    pub fn press_mode(&mut self, mode: SessionMode) -> SessionAction {
        if self.state != SessionState::Idle {
            return SessionAction::None;
        }
        self.next_session_id += 1;
        let session_id = self.next_session_id;
        self.state = SessionState::Recording { session_id, mode };
        SessionAction::Started { session_id, mode }
    }

    pub fn release(&mut self) -> SessionAction {
        self.release_mode(SessionMode::Dictation)
    }

    /// Finish only the recording started by the same product mode.
    ///
    /// With two global shortcuts, a release from the inactive shortcut must
    /// not terminate the other mode's recording.
    pub fn release_mode(&mut self, released_mode: SessionMode) -> SessionAction {
        let SessionState::Recording { session_id, mode } = self.state else {
            return SessionAction::None;
        };
        if mode != released_mode {
            return SessionAction::None;
        }
        self.state = SessionState::Transcribing { session_id, mode };
        SessionAction::FinishRequested { session_id, mode }
    }

    pub fn partial(&mut self, session_id: u64, text: String) -> SessionAction {
        if self.current_session() != Some(session_id) {
            return SessionAction::None;
        }
        SessionAction::PartialUpdated {
            session_id,
            mode: self.current_mode().expect("active session has a mode"),
            text,
        }
    }

    pub fn finalize(&mut self, session_id: u64, text: String) -> SessionAction {
        let SessionState::Transcribing {
            session_id: active_session,
            mode,
        } = self.state
        else {
            return SessionAction::None;
        };
        if active_session != session_id {
            return SessionAction::None;
        }
        self.state = SessionState::Idle;
        SessionAction::Finalized {
            session_id,
            mode,
            text,
        }
    }

    // Failure always returns the machine to Idle: a stuck Error state would
    // make every later hotkey press a silent no-op until restart.
    pub fn fail(&mut self, message: impl Into<String>) -> SessionAction {
        self.state = SessionState::Idle;
        SessionAction::Failed {
            message: message.into(),
        }
    }

    pub fn reset(&mut self) {
        self.state = SessionState::Idle;
    }
}

#[derive(Debug)]
pub struct AudioPreRoll {
    samples: VecDeque<i16>,
    capacity: usize,
}

impl AudioPreRoll {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, samples: &[i16]) {
        if self.capacity == 0 {
            return;
        }
        for sample in samples {
            if self.samples.len() == self.capacity {
                self.samples.pop_front();
            }
            self.samples.push_back(*sample);
        }
    }

    pub fn snapshot(&self) -> Vec<i16> {
        self.samples.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_transitions_are_idempotent() {
        let mut machine = SessionMachine::default();
        assert_eq!(
            machine.press(),
            SessionAction::Started {
                session_id: 1,
                mode: SessionMode::Dictation,
            }
        );
        assert_eq!(machine.press(), SessionAction::None);
        assert_eq!(
            machine.release(),
            SessionAction::FinishRequested {
                session_id: 1,
                mode: SessionMode::Dictation,
            }
        );
        assert_eq!(machine.release(), SessionAction::None);
        assert_eq!(
            machine.finalize(1, "hello".into()),
            SessionAction::Finalized {
                session_id: 1,
                mode: SessionMode::Dictation,
                text: "hello".into()
            }
        );
        assert_eq!(machine.state(), &SessionState::Idle);
    }

    #[test]
    fn stale_final_result_is_ignored() {
        let mut machine = SessionMachine::default();
        machine.press();
        machine.release();
        assert_eq!(machine.finalize(99, "stale".into()), SessionAction::None);
        assert_eq!(
            machine.state(),
            &SessionState::Transcribing {
                session_id: 1,
                mode: SessionMode::Dictation,
            }
        );
    }

    #[test]
    fn failure_returns_to_idle_so_the_next_press_works() {
        let mut machine = SessionMachine::default();
        machine.press();
        machine.release();
        assert_eq!(
            machine.fail("sidecar exploded"),
            SessionAction::Failed {
                message: "sidecar exploded".into()
            }
        );
        assert_eq!(machine.state(), &SessionState::Idle);
        assert!(matches!(
            machine.press(),
            SessionAction::Started {
                session_id: 2,
                mode: SessionMode::Dictation,
            }
        ));
    }

    #[test]
    fn partials_are_accepted_only_for_the_current_session() {
        let mut machine = SessionMachine::default();
        assert_eq!(machine.partial(1, "x".into()), SessionAction::None);
        machine.press();
        assert_eq!(
            machine.partial(1, "hel".into()),
            SessionAction::PartialUpdated {
                session_id: 1,
                mode: SessionMode::Dictation,
                text: "hel".into()
            }
        );
        machine.release();
        assert_eq!(
            machine.partial(1, "hello".into()),
            SessionAction::PartialUpdated {
                session_id: 1,
                mode: SessionMode::Dictation,
                text: "hello".into()
            }
        );
        assert_eq!(machine.partial(7, "other".into()), SessionAction::None);
    }

    #[test]
    fn session_ids_keep_increasing_after_failures() {
        let mut machine = SessionMachine::default();
        machine.press();
        machine.fail("boom");
        assert!(matches!(
            machine.press(),
            SessionAction::Started {
                session_id: 2,
                mode: SessionMode::Dictation,
            }
        ));
        machine.release();
        machine.finalize(2, "ok".into());
        assert!(matches!(
            machine.press(),
            SessionAction::Started {
                session_id: 3,
                mode: SessionMode::Dictation,
            }
        ));
    }

    #[test]
    fn session_mode_is_stable_until_the_session_ends() {
        let mut machine = SessionMachine::default();
        assert_eq!(machine.current_mode(), None);
        assert_eq!(
            machine.press_mode(SessionMode::System),
            SessionAction::Started {
                session_id: 1,
                mode: SessionMode::System,
            }
        );
        assert_eq!(machine.current_mode(), Some(SessionMode::System));

        // A second hotkey cannot change an in-flight session's destination.
        assert_eq!(
            machine.press_mode(SessionMode::Dictation),
            SessionAction::None
        );
        assert_eq!(machine.current_mode(), Some(SessionMode::System));

        // Releasing the other shortcut cannot end this System recording.
        assert_eq!(
            machine.release_mode(SessionMode::Dictation),
            SessionAction::None
        );
        assert!(matches!(machine.state(), SessionState::Recording { .. }));
        machine.release_mode(SessionMode::System);
        assert_eq!(machine.current_mode(), Some(SessionMode::System));
        machine.finalize(1, "open Chrome".into());
        assert_eq!(machine.current_mode(), None);
    }

    #[test]
    fn pre_roll_keeps_only_latest_samples() {
        let mut buffer = AudioPreRoll::new(4);
        buffer.push(&[1, 2, 3]);
        buffer.push(&[4, 5]);
        assert_eq!(buffer.snapshot(), vec![2, 3, 4, 5]);
        buffer.clear();
        assert!(buffer.snapshot().is_empty());
    }
}

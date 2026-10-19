//! The macOS menu bar item, daemon side. The overlay draws the icon and
//! builds the menu from the latest `menu` message, which this module sends
//! whenever something on it changes. Picks come back as overlay events
//! (`check_updates`, `install_update`, `set_auto_update`, `clear_history`)
//! that the loop handles.

use sunoto_ipc::OverlayRequest;

use crate::health::DaemonHealth;
use crate::history::HistoryRecorder;
use crate::overlay::UiFront;
use crate::settings::{self, Settings};
use crate::update::{self, Mode, Staged};

pub(crate) struct MenuBar {
    /// Only the macOS overlay has a menu bar item.
    enabled: bool,
    dirty: bool,
    shown_health: Option<DaemonHealth>,
    /// Check for Updates is running.
    pub(crate) checking: bool,
}

/// What the menu shows besides history, as the loop holds it.
pub(crate) struct MenuInputs<'a> {
    pub(crate) health: DaemonHealth,
    pub(crate) settings: &'a Settings,
    pub(crate) auto_update: Mode,
    /// Why this copy cannot update itself, whatever the setting.
    pub(crate) update_blocked: Option<&'a str>,
    pub(crate) pending_update: Option<&'a Staged>,
}

impl MenuBar {
    pub(crate) fn new(settings: &Settings) -> Self {
        Self {
            enabled: cfg!(target_os = "macos") && settings.overlay_backend == "macos",
            dirty: true,
            shown_health: None,
            checking: false,
        }
    }

    /// Something on the menu changed; it is sent on the next `refresh`.
    pub(crate) fn changed(&mut self) {
        self.dirty = true;
    }

    /// Send the menu if anything on it changed since it was last sent.
    /// Called once per loop turn.
    pub(crate) fn refresh(
        &mut self,
        ui: &UiFront,
        history: &mut HistoryRecorder,
        inputs: MenuInputs,
    ) {
        if history.take_recent_changed() || self.shown_health != Some(inputs.health) {
            self.dirty = true;
        }
        if !self.enabled || !self.dirty || !ui.overlay_active() {
            return;
        }
        self.dirty = false;
        self.shown_health = Some(inputs.health);
        ui.overlay_send(menu_request(&inputs, self.checking, history));
    }

    /// A message box from the menu bar item.
    pub(crate) fn alert(&self, ui: &UiFront, title: String, text: String, offer_install: bool) {
        if self.enabled {
            ui.overlay_send(OverlayRequest::Alert {
                title,
                text,
                offer_install,
            });
        }
    }
}

fn menu_request(inputs: &MenuInputs, checking: bool, history: &HistoryRecorder) -> OverlayRequest {
    OverlayRequest::Menu {
        status: inputs.health.menu_status().to_string(),
        ready: inputs.health.is_ready(),
        hint: format!(
            "Tap {} to dictate",
            settings::shortcut_label(&inputs.settings.shortcut)
        ),
        version: env!("CARGO_PKG_VERSION").to_string(),
        auto_update: inputs.auto_update == Mode::Install,
        update_blocked: inputs.update_blocked.map(sentence),
        update_note: inputs.pending_update.map(|staged| {
            if inputs.auto_update == Mode::Install {
                format!(
                    "Sunoto {} installs after {} without dictation",
                    staged.version,
                    update::idle_wait_label()
                )
            } else {
                format!("Sunoto {} is downloaded", staged.version)
            }
        }),
        checking,
        history_enabled: history.is_enabled(),
        recent: history.recent(),
    }
}

/// "this copy was built from source" -> "This copy was built from source."
pub(crate) fn sentence(text: &str) -> String {
    let text = text.trim().trim_end_matches('.');
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(
            sentence("this copy of Sunoto was built from source; update it with git pull"),
            "This copy of Sunoto was built from source; update it with git pull."
        );
        assert_eq!(sentence("Already one."), "Already one.");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn the_menu_carries_status_hint_switch_and_history() {
        let settings = Settings::default();
        let history = HistoryRecorder::start(&Settings {
            history_enabled: false,
            ..Settings::default()
        });
        let inputs = MenuInputs {
            health: DaemonHealth::Ready,
            settings: &settings,
            auto_update: Mode::Off,
            update_blocked: Some("this copy of Sunoto was built from source"),
            pending_update: None,
        };
        let OverlayRequest::Menu {
            status,
            ready,
            hint,
            auto_update,
            update_blocked,
            update_note,
            checking,
            history_enabled,
            recent,
            ..
        } = menu_request(&inputs, true, &history)
        else {
            panic!("not a menu");
        };
        assert_eq!(status, "Sunoto is ready");
        assert!(ready);
        assert_eq!(
            hint,
            format!(
                "Tap {} to dictate",
                settings::shortcut_label(&settings.shortcut)
            )
        );
        assert!(!auto_update && checking && !history_enabled && recent.is_empty());
        assert_eq!(
            update_blocked.as_deref(),
            Some("This copy of Sunoto was built from source.")
        );
        assert_eq!(update_note, None);
    }
}

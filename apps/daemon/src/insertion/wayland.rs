//! Wayland (Hyprland) insertion through external tools: `hyprctl` for the
//! focus token, `wl-copy` for the clipboard, `wtype` for paste and typing.
//! Wayland has no global-grab primitive, so the shortcut itself comes from
//! compositor bindings over the control socket, not from here.

use std::io::Write;
use std::process::{Command, Stdio};

use sunoto_desktop::InsertionOutcome;

use crate::logging;

use super::FocusSnapshot;

pub(crate) struct WaylandUiAdapter;

impl WaylandUiAdapter {
    pub(crate) fn open() -> Result<Self, String> {
        require_program("hyprctl")?;
        require_program("wtype")?;
        require_program("wl-copy")?;
        Ok(Self)
    }

    pub(crate) fn capture_focus(&self) -> FocusSnapshot {
        match active_hyprland_window() {
            Some(window) => FocusSnapshot {
                token: Some(window.address),
                class: window.class.map(|class| (class.clone(), class)),
            },
            None => FocusSnapshot {
                token: None,
                class: None,
            },
        }
    }

    pub(crate) fn insert(
        &self,
        focus_at_release: Option<String>,
        text: &str,
    ) -> Result<InsertionOutcome, String> {
        let focus_now = active_hyprland_window();
        if let Some(expected) = focus_at_release
            && focus_now.as_ref().map(|window| window.address.as_str()) != Some(expected.as_str())
        {
            return self
                .set_clipboard(text)
                .map(|_| InsertionOutcome::ClipboardOnly);
        }

        self.set_clipboard(text)?;
        let terminal = focus_now
            .as_ref()
            .and_then(|window| window.class.as_deref())
            .is_some_and(is_terminal_class);
        match self.paste_clipboard(terminal) {
            Ok(()) => Ok(InsertionOutcome::Pasted),
            Err(paste_error) => match self.type_direct(text) {
                Ok(()) => Ok(InsertionOutcome::Typed),
                Err(type_error) => {
                    logging::warn(&format!(
                        "Wayland paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                    ));
                    Ok(InsertionOutcome::ClipboardOnly)
                }
            },
        }
    }

    pub(crate) fn paste_clipboard(&self, terminal: bool) -> Result<(), String> {
        let mut command = Command::new("wtype");
        if terminal {
            command.args([
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
            ]);
        } else {
            command.args(["-M", "ctrl", "-k", "v", "-m", "ctrl"]);
        }
        run_command(command, "wtype paste")
    }

    pub(crate) fn type_direct(&self, text: &str) -> Result<(), String> {
        let mut command = Command::new("wtype");
        command.arg("--").arg(text);
        run_command(command, "wtype direct")
    }

    pub(crate) fn focus_matches(&self, expected: &str) -> bool {
        active_hyprland_window()
            .as_ref()
            .map(|window| window.address.as_str())
            == Some(expected)
    }

    pub(crate) fn set_clipboard(&self, text: &str) -> Result<(), String> {
        let mut child = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run wl-copy: {error}"))?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err("wl-copy stdin unavailable".to_string());
        };
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("cannot write wl-copy input: {error}"))?;
        drop(stdin);
        let status = child
            .wait()
            .map_err(|error| format!("cannot wait for wl-copy: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("wl-copy exited with {status}"))
    }
}

pub(crate) fn is_terminal_class(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    [
        "terminal",
        "ghostty",
        "kitty",
        "alacritty",
        "foot",
        "wezterm",
        "xterm",
        "urxvt",
        "konsole",
        "tilix",
        "terminator",
    ]
    .iter()
    .any(|needle| class.contains(needle))
}

pub(crate) fn run_command(mut command: Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot run {label}: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{label} exited with {status}"))
}

pub(crate) struct HyprlandWindow {
    pub(crate) address: String,
    pub(crate) class: Option<String>,
}

pub(crate) fn active_hyprland_window() -> Option<HyprlandWindow> {
    let output = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let address = payload.get("address")?.as_str()?.to_string();
    if address.is_empty() || address == "0x0" {
        return None;
    }
    let class = payload
        .get("class")
        .and_then(serde_json::Value::as_str)
        .filter(|class| !class.is_empty())
        .map(str::to_string);
    Some(HyprlandWindow { address, class })
}

pub(crate) fn require_program(program: &str) -> Result<(), String> {
    match Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{program} unavailable: {error}")),
    }
}

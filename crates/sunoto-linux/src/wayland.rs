//! Wayland (Hyprland) insertion through external tools: `hyprctl` for the
//! focus token, `wl-copy` for the clipboard, `wtype` for paste and typing.
//! Wayland has no global-grab primitive, so the shortcut itself comes from
//! compositor bindings over the daemon's control socket, not from here.
//!
//! This module is plain subprocess glue, so it compiles on every platform
//! and the daemon can name the type unconditionally. It has its own small
//! outcome enum rather than reusing the X11 or macOS one so that no cross
//! platform type identity is needed.

use std::io::Write;
use std::process::{Command, Stdio};

/// What a Wayland insertion did. The daemon maps this onto its platform
/// outcome type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaylandOutcome {
    Typed,
    Pasted,
    ClipboardOnly,
}

/// The focused Hyprland window at capture time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WaylandFocus {
    /// Hyprland window address; `None` when no window is focused.
    pub token: Option<String>,
    /// Window class, when Hyprland reports one.
    pub class: Option<String>,
}

#[derive(Default)]
pub struct WaylandUiAdapter {
    warning: Option<String>,
}

impl WaylandUiAdapter {
    pub fn open() -> Result<Self, String> {
        require_program("hyprctl")?;
        require_program("wtype")?;
        require_program("wl-copy")?;
        Ok(Self::default())
    }

    pub fn capture_focus(&self) -> WaylandFocus {
        match active_hyprland_window() {
            Some(window) => WaylandFocus {
                token: Some(window.address),
                class: window.class,
            },
            None => WaylandFocus::default(),
        }
    }

    /// A diagnostic from the last `insert` worth logging (for example both
    /// paste and typing failed and the text was left on the clipboard).
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }

    pub fn insert(
        &mut self,
        focus_at_release: Option<String>,
        text: &str,
    ) -> Result<WaylandOutcome, String> {
        let focus_now = active_hyprland_window();
        if let Some(expected) = focus_at_release
            && focus_now.as_ref().map(|window| window.address.as_str()) != Some(expected.as_str())
        {
            return self
                .set_clipboard(text)
                .map(|_| WaylandOutcome::ClipboardOnly);
        }

        self.set_clipboard(text)?;
        let terminal = focus_now
            .as_ref()
            .and_then(|window| window.class.as_deref())
            .is_some_and(is_terminal_class);
        match self.paste_clipboard(terminal) {
            Ok(()) => Ok(WaylandOutcome::Pasted),
            Err(paste_error) => match self.type_direct(text) {
                Ok(()) => Ok(WaylandOutcome::Typed),
                Err(type_error) => {
                    self.warning = Some(format!(
                        "Wayland paste failed ({paste_error}); direct typing failed ({type_error}); result left on clipboard"
                    ));
                    Ok(WaylandOutcome::ClipboardOnly)
                }
            },
        }
    }

    fn paste_clipboard(&self, terminal: bool) -> Result<(), String> {
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

    pub fn type_direct(&self, text: &str) -> Result<(), String> {
        let mut command = Command::new("wtype");
        command.arg("--").arg(text);
        run_command(command, "wtype direct")
    }

    pub fn focus_matches(&self, expected: &str) -> bool {
        active_hyprland_window()
            .as_ref()
            .map(|window| window.address.as_str())
            == Some(expected)
    }

    fn set_clipboard(&self, text: &str) -> Result<(), String> {
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

/// Terminals paste with Ctrl+Shift+V; everything else with Ctrl+V.
pub fn is_terminal_class(class: &str) -> bool {
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

fn run_command(mut command: Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot run {label}: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{label} exited with {status}"))
}

struct HyprlandWindow {
    address: String,
    class: Option<String>,
}

fn active_hyprland_window() -> Option<HyprlandWindow> {
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

fn require_program(program: &str) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_classes_are_matched_case_insensitively() {
        assert!(is_terminal_class("Alacritty"));
        assert!(is_terminal_class("org.wezfurlong.wezterm"));
        assert!(is_terminal_class("kitty"));
        assert!(!is_terminal_class("firefox"));
        assert!(!is_terminal_class("code"));
    }

    #[test]
    fn warning_is_handed_over_once() {
        let mut adapter = WaylandUiAdapter {
            warning: Some("left on clipboard".to_string()),
        };
        assert_eq!(adapter.take_warning().as_deref(), Some("left on clipboard"));
        assert_eq!(adapter.take_warning(), None);
    }
}

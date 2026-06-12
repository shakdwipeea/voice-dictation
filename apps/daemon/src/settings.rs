use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use sunoto_polish::PolishConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Push-to-talk shortcut, e.g. "Ctrl+F1".
    pub shortcut: String,
    /// PulseAudio source name, or "auto" for the first physical microphone.
    pub microphone: String,
    /// "mock" or "nemotron".
    pub backend: String,
    /// Streaming profile: 80, 160, 560, or 1120 ms.
    pub profile_ms: u16,
    /// Audio retained before the shortcut press, so the first word survives.
    pub preroll_ms: u32,
    /// Watchdog: how long Transcribing may wait for the final result.
    pub final_timeout_ms: u64,
    /// Override the sidecar interpreter (defaults depend on the backend).
    pub sidecar_python: Option<String>,
    /// Override the sidecar script path.
    pub sidecar_script: Option<String>,
    /// Injecting Enter/Tab into a focused terminal can execute commands, so
    /// both are replaced with spaces unless explicitly allowed.
    pub allow_enter_and_tab: bool,
    /// GTK4 pill overlay (UI sidecar). When it cannot start (e.g. GTK4 is
    /// not installed) the daemon falls back to the native X11 bubble.
    pub overlay_enabled: bool,
    /// "auto", "x11", or "wayland" for the GTK overlay sidecar.
    pub overlay_backend: String,
    /// false = raw transcription, true = deterministic cleanup pipeline.
    pub polish_enabled: bool,
    pub polish: PolishConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: "Ctrl+F1".to_string(),
            microphone: "auto".to_string(),
            backend: "mock".to_string(),
            profile_ms: 160,
            preroll_ms: 300,
            final_timeout_ms: 5000,
            sidecar_python: None,
            sidecar_script: None,
            allow_enter_and_tab: false,
            overlay_enabled: true,
            overlay_backend: "auto".to_string(),
            polish_enabled: true,
            polish: PolishConfig::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|error| format!("invalid settings file {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(self).expect("settings serialize");
        std::fs::write(path, payload + "\n")
            .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }

    pub fn sidecar_command(&self) -> Result<(String, Vec<String>), String> {
        let root = repo_root();
        let (default_python, default_script, extra_args): (&str, PathBuf, Vec<String>) =
            match self.backend.as_str() {
                "mock" => ("python3", root.join("services/asr/mock_sidecar.py"), vec![]),
                "nemotron" => (
                    "python3",
                    root.join("services/asr/nemotron_sidecar.py"),
                    vec!["--profile-ms".to_string(), self.profile_ms.to_string()],
                ),
                other => return Err(format!("unknown ASR backend: {other}")),
            };
        let python = self.sidecar_python.clone().unwrap_or_else(|| {
            if self.backend == "nemotron" {
                let venv = root.join(".venv-nemotron/bin/python");
                if venv.is_file() {
                    return venv.to_string_lossy().into_owned();
                }
            }
            default_python.to_string()
        });
        let script = self
            .sidecar_script
            .clone()
            .unwrap_or_else(|| default_script.to_string_lossy().into_owned());
        let mut args = vec![script];
        args.extend(extra_args);
        Ok((python, args))
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.backend.as_str() {
            "mock" | "nemotron" => {}
            other => return Err(format!("unknown ASR backend: {other}")),
        }
        match self.profile_ms {
            80 | 160 | 560 | 1120 => {}
            other => {
                return Err(format!(
                    "unsupported profile_ms {other}; use 80, 160, 560, or 1120"
                ));
            }
        }
        match self.overlay_backend.as_str() {
            "auto" | "x11" | "wayland" => Ok(()),
            other => Err(format!(
                "unsupported overlay_backend {other:?}; use auto, x11, or wayland"
            )),
        }
    }

    /// Command and environment for the GTK overlay UI sidecar, which runs as
    /// a Python module so its package-relative imports resolve.
    pub fn overlay_command(&self) -> (String, Vec<String>, Vec<(String, String)>) {
        let src = repo_root().join("src");
        let mut envs = vec![
            ("PYTHONPATH".to_string(), src.to_string_lossy().into_owned()),
            (
                "SUNOTO_OVERLAY_BACKEND".to_string(),
                self.overlay_backend.clone(),
            ),
        ];
        match self.overlay_backend.as_str() {
            "x11" => envs.push(("GDK_BACKEND".to_string(), "x11".to_string())),
            "wayland" => {
                envs.push(("GDK_BACKEND".to_string(), "wayland".to_string()));
                if let Some(preload) = gtk4_layer_shell_preload() {
                    envs.push(("LD_PRELOAD".to_string(), preload));
                }
            }
            _ => {}
        }
        (
            "python3".to_string(),
            vec!["-m".to_string(), "voice_dictation.ui_sidecar".to_string()],
            envs,
        )
    }
}

fn gtk4_layer_shell_preload() -> Option<String> {
    if let Ok(path) = std::env::var("SUNOTO_GTK4_LAYER_SHELL_PRELOAD")
        && !path.is_empty()
    {
        return Some(merge_ld_preload(&path));
    }
    let candidates = [
        "/usr/lib/libgtk4-layer-shell.so",
        "/usr/lib/libgtk4-layer-shell.so.0",
        "/usr/lib/x86_64-linux-gnu/libgtk4-layer-shell.so",
        "/usr/lib/x86_64-linux-gnu/libgtk4-layer-shell.so.0",
        "/usr/local/lib/libgtk4-layer-shell.so",
    ];
    candidates
        .iter()
        .find(|path| Path::new(path).is_file())
        .map(|path| merge_ld_preload(path))
}

fn merge_ld_preload(path: &str) -> String {
    match std::env::var("LD_PRELOAD") {
        Ok(existing) if !existing.is_empty() && !existing.split(':').any(|item| item == path) => {
            format!("{path}:{existing}")
        }
        Ok(existing) if !existing.is_empty() => existing,
        _ => path.to_string(),
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("SUNOTO_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });
    base.join("sunoto/config.json")
}

pub fn control_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("SUNOTO_CONTROL_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("sunoto/daemon.sock");
    }
    std::env::temp_dir().join(format!(
        "sunoto-{}-daemon.sock",
        std::env::var("USER").unwrap_or_else(|_| "user".to_string())
    ))
}

/// Repository root for development runs; packaged builds set SUNOTO_ROOT or
/// configure explicit sidecar paths in the settings file.
pub fn repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("SUNOTO_ROOT") {
        return PathBuf::from(root);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Remove control characters that could trigger actions in the focused
/// application (Enter submitting a form or running a shell line).
pub fn sanitize_for_insertion(text: &str, allow_enter_and_tab: bool) -> String {
    text.trim()
        .chars()
        .filter_map(|character| {
            if character == '\n' || character == '\t' {
                if allow_enter_and_tab {
                    Some(character)
                } else {
                    Some(' ')
                }
            } else if character.is_control() {
                None
            } else {
                Some(character)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_file_yields_defaults() {
        let path = std::env::temp_dir().join(format!(
            "sunoto-settings-missing-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
    }

    #[test]
    fn settings_round_trip_and_partial_files_work() {
        let path = std::env::temp_dir().join(format!(
            "sunoto-settings-roundtrip-{}.json",
            std::process::id()
        ));
        let settings = Settings {
            backend: "nemotron".to_string(),
            profile_ms: 80,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        std::fs::write(&path, "{\"profile_ms\": 560}").unwrap();
        let partial = Settings::load(&path).unwrap();
        assert_eq!(partial.profile_ms, 560);
        assert_eq!(partial.backend, "mock");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_settings_files_are_rejected() {
        let path = std::env::temp_dir().join(format!(
            "sunoto-settings-invalid-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "not json").unwrap();
        assert!(Settings::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sidecar_command_selects_backend_defaults() {
        let mut settings = Settings::default();
        let (python, args) = settings.sidecar_command().unwrap();
        assert_eq!(python, "python3");
        assert!(args[0].ends_with("services/asr/mock_sidecar.py"));
        settings.backend = "nemotron".to_string();
        settings.profile_ms = 80;
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/nemotron_sidecar.py"));
        assert_eq!(args[1..], ["--profile-ms".to_string(), "80".to_string()]);
        settings.backend = "imaginary".to_string();
        assert!(settings.sidecar_command().is_err());
    }

    #[test]
    fn overlay_command_runs_the_ui_module_with_pythonpath() {
        let settings = Settings::default();
        assert!(settings.overlay_enabled);
        let (python, args, envs) = settings.overlay_command();
        assert_eq!(python, "python3");
        assert_eq!(args, ["-m", "voice_dictation.ui_sidecar"]);
        assert!(
            envs.iter()
                .any(|(key, value)| key == "PYTHONPATH" && value.ends_with("/src"))
        );
        assert!(
            envs.iter()
                .any(|(key, value)| key == "SUNOTO_OVERLAY_BACKEND" && value == "auto")
        );
    }

    #[test]
    fn overlay_backend_can_force_gdk_backend() {
        let mut settings = Settings {
            overlay_backend: "wayland".to_string(),
            ..Settings::default()
        };
        let (_python, _args, envs) = settings.overlay_command();
        assert!(
            envs.iter()
                .any(|(key, value)| key == "GDK_BACKEND" && value == "wayland")
        );

        settings.overlay_backend = "x11".to_string();
        let (_python, _args, envs) = settings.overlay_command();
        assert!(
            envs.iter()
                .any(|(key, value)| key == "GDK_BACKEND" && value == "x11")
        );
    }

    #[test]
    fn validation_rejects_unknown_overlay_backend() {
        let settings = Settings {
            overlay_backend: "mir".to_string(),
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn control_socket_path_prefers_explicit_env() {
        // SAFETY: this test mutates process environment and does not rely on
        // concurrent environment reads.
        unsafe {
            std::env::set_var("SUNOTO_CONTROL_SOCKET", "/tmp/sunoto-test.sock");
        }
        assert_eq!(
            control_socket_path(),
            PathBuf::from("/tmp/sunoto-test.sock")
        );
        // SAFETY: restores the process environment after the assertion.
        unsafe {
            std::env::remove_var("SUNOTO_CONTROL_SOCKET");
        }
    }

    #[test]
    fn insertion_sanitizer_neutralizes_control_characters() {
        assert_eq!(
            sanitize_for_insertion("hello\nworld\u{7}!\r", false),
            "hello world!"
        );
        assert_eq!(
            sanitize_for_insertion("line one\nline two", true),
            "line one\nline two"
        );
        assert_eq!(sanitize_for_insertion("  padded  ", false), "padded");
    }
}

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use sunoto_polish::PolishConfig;

const DEFAULT_PARAKEET_MLX_MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v3";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Push-to-talk shortcut, e.g. "Ctrl+F1".
    pub shortcut: String,
    /// PulseAudio source name, or "auto" for the first physical microphone.
    pub microphone: String,
    /// "mock", "nemotron" (cache-aware RNNT streaming), or
    /// "nemotron_offline" (whole-utterance).
    pub backend: String,
    /// Streaming profile: 80, 160, 560, or 1120 ms.
    pub profile_ms: u16,
    /// Audio retained before the shortcut press, so the first word survives.
    pub preroll_ms: u32,
    /// Watchdog: how long Transcribing may wait for the final result.
    /// This is the fixed base; the effective deadline also scales with the
    /// recorded audio length via `final_timeout_rtf` (so a long utterance
    /// gets proportionally more time on offline backends whose latency
    /// grows with audio duration).
    pub final_timeout_ms: u64,
    /// Watchdog scale factor on the recorded audio duration. The effective
    /// transcribe deadline is `final_timeout_ms + recorded_ms * final_timeout_rtf`,
    /// covering offline backends (e.g. Nemotron offline on macOS) whose
    /// latency is roughly proportional to utterance length.
    pub final_timeout_rtf: f64,
    /// Override the sidecar interpreter (defaults depend on the backend).
    pub sidecar_python: Option<String>,
    /// Override the sidecar script path.
    pub sidecar_script: Option<String>,
    /// ASR device override for Nemotron backends. Streaming "nemotron"
    /// defaults to CUDA when unset; macOS should set "cpu" for streaming RNNT.
    /// Offline "nemotron_offline" accepts "mps" or "cpu". CoreML and
    /// Parakeet-MLX backends ignore this — their runtimes select compute units.
    pub asr_device: Option<String>,
    /// Optional ASR model override for backends that expose a --model flag.
    /// Currently used by Parakeet-MLX backends; unset means the benchmark-
    /// selected v3 MLX checkpoint.
    pub asr_model: Option<String>,
    /// Directory holding encoder.mlpackage, decoder.mlpackage, and
    /// metadata.json for the "nemotron_coreml" backend. Populated by
    /// services/asr/setup_coreml_runtime.sh from the pre-built FP16 models
    /// on Hugging Face (danielbodart/nemotron-speech-600m-coreml).
    pub coreml_model_dir: String,
    /// Injecting Enter/Tab into a focused terminal can execute commands, so
    /// both are replaced with spaces unless explicitly allowed.
    pub allow_enter_and_tab: bool,
    /// GTK4 pill overlay (UI sidecar). When it cannot start (e.g. GTK4 is
    /// not installed) the daemon falls back to the native X11 bubble.
    pub overlay_enabled: bool,
    /// "auto", "x11", "wayland", or "macos" for the overlay sidecar backend.
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
            final_timeout_ms: 8000,
            final_timeout_rtf: 3.0,
            sidecar_python: None,
            sidecar_script: None,
            asr_device: None,
            asr_model: None,
            coreml_model_dir: "build/phase0/nemotron-coreml/fp16".to_string(),
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
                "nemotron" => ("python3", root.join("services/asr/nemotron_sidecar.py"), {
                    let mut v = vec!["--profile-ms".to_string(), self.profile_ms.to_string()];
                    push_asr_device_arg(&mut v, &self.asr_device);
                    v
                }),
                "nemotron_offline" => (
                    "python3",
                    root.join("services/asr/nemotron_offline_sidecar.py"),
                    {
                        let mut v = vec!["--profile-ms".to_string(), self.profile_ms.to_string()];
                        push_asr_device_arg(&mut v, &self.asr_device);
                        v
                    },
                ),
                "nemotron_coreml" => (
                    "python3",
                    root.join("services/asr/nemotron_coreml_sidecar.py"),
                    {
                        let mut v = vec!["--profile-ms".to_string(), self.profile_ms.to_string()];
                        v.push("--model-dir".to_string());
                        v.push(resolve_path(&root, &self.coreml_model_dir));
                        v
                    },
                ),
                "parakeet_mlx_offline" => (
                    "python3",
                    root.join("services/asr/parakeet_mlx_offline_sidecar.py"),
                    self.parakeet_mlx_args(),
                ),
                "parakeet_mlx_streaming" => (
                    "python3",
                    root.join("services/asr/parakeet_mlx_streaming_sidecar.py"),
                    self.parakeet_mlx_args(),
                ),
                other => return Err(format!("unknown ASR backend: {other}")),
            };
        let python = self.sidecar_python.clone().unwrap_or_else(|| {
            if self.backend == "nemotron" {
                if cfg!(target_os = "macos") {
                    let venv = root.join(".venv-nemotron-mac/bin/python");
                    if venv.is_file() {
                        return venv.to_string_lossy().into_owned();
                    }
                }
                let venv = root.join(".venv-nemotron/bin/python");
                if venv.is_file() {
                    return venv.to_string_lossy().into_owned();
                }
            }
            if self.backend == "nemotron_offline"
                || self.backend == "nemotron_coreml"
                || self.backend == "parakeet_mlx_offline"
                || self.backend == "parakeet_mlx_streaming"
            {
                let venv = root.join(".venv-nemotron-mac/bin/python");
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

    fn parakeet_mlx_args(&self) -> Vec<String> {
        vec![
            "--profile-ms".to_string(),
            self.profile_ms.to_string(),
            "--model".to_string(),
            self.asr_model
                .clone()
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| DEFAULT_PARAKEET_MLX_MODEL.to_string()),
        ]
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.backend.as_str() {
            "mock"
            | "nemotron"
            | "nemotron_offline"
            | "nemotron_coreml"
            | "parakeet_mlx_offline"
            | "parakeet_mlx_streaming" => {}
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
        if let Some(device) = self
            .asr_device
            .as_deref()
            .filter(|device| !device.is_empty())
        {
            match self.backend.as_str() {
                "nemotron_offline" if !matches!(device, "mps" | "cpu") => {
                    return Err(format!(
                        "unsupported asr_device {device:?} for nemotron_offline; use mps or cpu"
                    ));
                }
                // CoreML picks ANE+CPU itself; Parakeet-MLX uses MLX's default
                // Apple GPU/CPU selection. Neither has a torch device concept.
                "nemotron_coreml" | "parakeet_mlx_offline" | "parakeet_mlx_streaming" => {
                    return Err(format!(
                        "asr_device {device:?} is not supported for {}; unset asr_device",
                        self.backend
                    ));
                }
                _ if !matches!(device, "cuda" | "mps" | "cpu") => {
                    return Err(format!(
                        "unsupported asr_device {device:?}; use cuda, mps, or cpu"
                    ));
                }
                _ => {}
            }
        }
        match self.overlay_backend.as_str() {
            "auto" | "x11" | "wayland" | "macos" => Ok(()),
            other => Err(format!(
                "unsupported overlay_backend {other:?}; use auto, x11, wayland, or macos"
            )),
        }
    }

    /// Command and environment for the overlay UI sidecar. Linux uses the
    /// GTK Python module; macOS uses the native Swift NSPanel helper.
    pub fn overlay_command(&self) -> (String, Vec<String>, Vec<(String, String)>) {
        let src = repo_root().join("src");
        if cfg!(target_os = "macos") && self.overlay_backend == "macos" {
            let root = repo_root();
            let binary = root.join("target/release/sunoto-overlay");
            if binary.is_file() {
                return (
                    binary.to_string_lossy().into_owned(),
                    vec![],
                    vec![("SUNOTO_OVERLAY_BACKEND".to_string(), "macos".to_string())],
                );
            }
            return (
                "swift".to_string(),
                vec![
                    root.join("services/macos/sunoto-overlay.swift")
                        .to_string_lossy()
                        .into_owned(),
                ],
                vec![("SUNOTO_OVERLAY_BACKEND".to_string(), "macos".to_string())],
            );
        }
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

fn push_asr_device_arg(args: &mut Vec<String>, device: &Option<String>) {
    if let Some(device) = device.as_ref()
        && !device.is_empty()
    {
        args.push("--device".to_string());
        args.push(device.clone());
    }
}

/// Resolve a config path. Absolute or user-prefixed paths are returned as-is
/// (the leading `~` is expanded). Other paths are joined under `repo_root()`
/// so settings can stay relative in config files (e.g.
/// `build/phase0/nemotron-coreml/fp16`) and still work when the daemon is
/// launched from a different cwd.
fn resolve_path(repo_root: &Path, value: &str) -> String {
    if Path::new(value).is_absolute() {
        return value.to_string();
    }
    if let Some(stripped) = value.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home)
            .join(stripped.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned();
    }
    repo_root.join(value).to_string_lossy().into_owned()
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
    if cfg!(target_os = "macos") {
        // macOS convention: ~/Library/Application Support/sunoto/config.json.
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        return PathBuf::from(home).join("Library/Application Support/sunoto/config.json");
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
    fn sidecar_command_supports_nemotron_offline() {
        let settings = Settings {
            backend: "nemotron_offline".to_string(),
            profile_ms: 560,
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/nemotron_offline_sidecar.py"));
        assert_eq!(args[1..], ["--profile-ms".to_string(), "560".to_string()]);
    }

    #[test]
    fn sidecar_command_supports_nemotron_coreml() {
        let settings = Settings {
            backend: "nemotron_coreml".to_string(),
            profile_ms: 160,
            coreml_model_dir: "build/phase0/nemotron-coreml/fp16".to_string(),
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/nemotron_coreml_sidecar.py"));
        assert_eq!(
            &args[1..3],
            &["--profile-ms".to_string(), "160".to_string()]
        );
        assert_eq!(args[3], "--model-dir");
        // resolve_path joins a relative path under the repo root, so the
        // trailing segments must match the configured dir.
        assert!(args[4].ends_with("nemotron-coreml/fp16"));
    }

    #[test]
    fn sidecar_command_supports_parakeet_mlx_offline_default_model() {
        let settings = Settings {
            backend: "parakeet_mlx_offline".to_string(),
            profile_ms: 560,
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/parakeet_mlx_offline_sidecar.py"));
        assert_eq!(
            args[1..],
            [
                "--profile-ms".to_string(),
                "560".to_string(),
                "--model".to_string(),
                DEFAULT_PARAKEET_MLX_MODEL.to_string(),
            ]
        );
    }

    #[test]
    fn sidecar_command_supports_parakeet_mlx_streaming_default_model() {
        let settings = Settings {
            backend: "parakeet_mlx_streaming".to_string(),
            profile_ms: 80,
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/parakeet_mlx_streaming_sidecar.py"));
        assert_eq!(
            args[1..],
            [
                "--profile-ms".to_string(),
                "80".to_string(),
                "--model".to_string(),
                DEFAULT_PARAKEET_MLX_MODEL.to_string(),
            ]
        );
    }

    #[test]
    fn sidecar_command_supports_parakeet_mlx_model_override() {
        for backend in ["parakeet_mlx_offline", "parakeet_mlx_streaming"] {
            let settings = Settings {
                backend: backend.to_string(),
                asr_model: Some("mlx-community/parakeet-tdt-0.6b-v2".to_string()),
                ..Settings::default()
            };
            let (_python, args) = settings.sidecar_command().unwrap();
            assert_eq!(args.last().unwrap(), "mlx-community/parakeet-tdt-0.6b-v2");
        }
    }

    #[test]
    fn sidecar_command_resolves_absolute_coreml_model_dir() {
        let settings = Settings {
            backend: "nemotron_coreml".to_string(),
            coreml_model_dir: "/opt/sunoto/coreml/fp16".to_string(),
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert_eq!(args.last().unwrap(), "/opt/sunoto/coreml/fp16");
    }

    #[test]
    fn validation_rejects_asr_device_for_coreml_backend() {
        let with_device = Settings {
            backend: "nemotron_coreml".to_string(),
            asr_device: Some("cpu".to_string()),
            ..Settings::default()
        };
        assert!(with_device.validate().is_err());
        let without = Settings {
            backend: "nemotron_coreml".to_string(),
            ..Settings::default()
        };
        assert!(without.validate().is_ok());
    }

    #[test]
    fn validation_rejects_asr_device_for_parakeet_mlx_backend() {
        for backend in ["parakeet_mlx_offline", "parakeet_mlx_streaming"] {
            let with_device = Settings {
                backend: backend.to_string(),
                asr_device: Some("mps".to_string()),
                ..Settings::default()
            };
            assert!(with_device.validate().is_err());
            let without = Settings {
                backend: backend.to_string(),
                ..Settings::default()
            };
            assert!(without.validate().is_ok());
        }
    }

    #[test]
    fn sidecar_command_passes_asr_device_to_offline_backend() {
        let settings = Settings {
            backend: "nemotron_offline".to_string(),
            profile_ms: 160,
            asr_device: Some("cpu".to_string()),
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert_eq!(
            args[1..],
            [
                "--profile-ms".to_string(),
                "160".to_string(),
                "--device".to_string(),
                "cpu".to_string(),
            ]
        );
    }

    #[test]
    fn sidecar_command_passes_asr_device_to_streaming_backend() {
        let settings = Settings {
            backend: "nemotron".to_string(),
            profile_ms: 80,
            asr_device: Some("cpu".to_string()),
            ..Settings::default()
        };
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/nemotron_sidecar.py"));
        assert_eq!(
            args[1..],
            [
                "--profile-ms".to_string(),
                "80".to_string(),
                "--device".to_string(),
                "cpu".to_string(),
            ]
        );
    }

    #[test]
    fn validation_accepts_backend_specific_asr_devices() {
        let cuda = Settings {
            asr_device: Some("cuda".to_string()),
            ..Settings::default()
        };
        assert!(cuda.validate().is_ok());
        let cpu = Settings {
            asr_device: Some("cpu".to_string()),
            ..Settings::default()
        };
        assert!(cpu.validate().is_ok());
        let mps = Settings {
            asr_device: Some("mps".to_string()),
            ..Settings::default()
        };
        assert!(mps.validate().is_ok());
        let unknown = Settings {
            asr_device: Some("ane".to_string()),
            ..Settings::default()
        };
        assert!(unknown.validate().is_err());
        let offline_cuda = Settings {
            backend: "nemotron_offline".to_string(),
            asr_device: Some("cuda".to_string()),
            ..Settings::default()
        };
        assert!(offline_cuda.validate().is_err());
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
    fn validation_accepts_macos_overlay_backend() {
        let settings = Settings {
            overlay_backend: "macos".to_string(),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_overlay_backend_uses_native_sidecar() {
        let settings = Settings {
            overlay_backend: "macos".to_string(),
            ..Settings::default()
        };
        let (command, args, envs) = settings.overlay_command();
        assert!(command.ends_with("sunoto-overlay") || command == "swift");
        if command == "swift" {
            assert_eq!(args.len(), 1);
            assert!(args[0].ends_with("services/macos/sunoto-overlay.swift"));
        } else {
            assert!(args.is_empty());
        }
        assert!(
            envs.iter()
                .any(|(key, value)| key == "SUNOTO_OVERLAY_BACKEND" && value == "macos")
        );
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

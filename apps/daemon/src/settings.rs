use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use sunoto_polish::PolishConfig;

const DEFAULT_PARAKEET_MLX_MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v3";

/// Default named LLM polish model profile. Resolved (see
/// `llm_polish_model_relative`) under `models/llm-polish-hf/` relative to
/// the repo root. Phi-4-mini Q5 is the post-ASR harness winner: ~433ms edit
/// p50 / ~236ms clean p50, vs ~799ms edit p50 for the previous Gemma 2B Q4.
const DEFAULT_LLM_POLISH_MODEL: &str = "phi4_mini";

/// Default and allowed values for the LLM polish sidecar dispatch mode. The
/// sidecar selects its completion path by reading `SUNOTO_LLM_POLISH_MODE`.
/// `constrained_one_call` uses the grammar-constrained single LLM call that
/// the post-ASR harness validated (Phi-4-mini edit p50 ~433ms); it is the
/// default so the live daemon runs the same path we benchmarked. The legacy
/// `one_pass_minimal` path is kept for fallback but is not the default.
const DEFAULT_LLM_POLISH_MODE: &str = "constrained_one_call";
const ALLOWED_LLM_POLISH_MODES: &[&str] =
    &["constrained_one_call", "one_pass_minimal", "two_step"];

/// Resolve a named LLM polish profile to its repo-relative GGUF path. Used
/// both to build the sidecar env and to validate config. Returns None for an
/// unknown name so callers can surface a config error instead of silently
/// falling back to the Python sidecar's bundled default.
fn llm_polish_model_relative(name: &str) -> Option<&'static str> {
    match name {
        "phi4_mini" => Some(
            "models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf",
        ),
        "gemma4_e2b" => Some(
            "models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf",
        ),
        _ => None,
    }
}

fn default_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "parakeet_mlx_streaming"
    } else {
        "mock"
    }
}

fn default_profile_ms() -> u16 {
    if cfg!(target_os = "macos") { 560 } else { 160 }
}

fn default_overlay_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "auto"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Push-to-talk shortcut, e.g. "Ctrl+F1".
    pub shortcut: String,
    /// PulseAudio source name, or "auto" for the first physical microphone.
    pub microphone: String,
    /// ASR backend. Linux config init defaults to "mock"; macOS config init
    /// defaults to "parakeet_mlx_streaming".
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
    /// Offline "nemotron_offline" accepts "mps" or "cpu". Parakeet-MLX
    /// backends ignore this — MLX selects compute units.
    pub asr_device: Option<String>,
    /// Optional ASR model override for backends that expose a --model flag.
    /// Currently used by Parakeet-MLX backends; unset means the benchmark-
    /// selected v3 MLX checkpoint.
    pub asr_model: Option<String>,
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
    /// Experimental local LLM cleanup after deterministic polish. Disabled by
    /// default; when enabled, a warm sidecar loads the model once at startup.
    pub llm_polish_enabled: bool,
    /// Override the Python interpreter used by the LLM polish sidecar.
    pub llm_polish_python: Option<String>,
    /// Override the LLM polish sidecar path.
    pub llm_polish_script: Option<String>,
    /// Override the local GGUF model path used by the helper. When set this
    /// wins over `llm_polish_model`. Unset means resolve `llm_polish_model`.
    pub llm_polish_model_path: Option<String>,
    /// Named LLM polish model profile resolved under `models/llm-polish-hf/`.
    /// "phi4_mini" (default) selects the Phi-4-mini Q5 checkpoint that
    /// benchmarked best on the post-ASR harness; "gemma4_e2b" keeps the
    /// original Gemma 2B Q4 model. Ignored when `llm_polish_model_path` is
    /// set. Overridable at runtime with `SUNOTO_LLM_POLISH_MODEL`.
    pub llm_polish_model: String,
    /// LLM polish sidecar dispatch mode. The post-ASR harness and e2e gate
    /// validate `constrained_one_call` (grammar-constrained single call); it
    /// is the default so the live path matches the benchmarked path. Other
    /// values: `one_pass_minimal` (legacy minimal prompt), `two_step`. Override
    /// at runtime with `SUNOTO_LLM_POLISH_MODE`.
    pub llm_polish_mode: String,
    /// Maximum wall-clock time for LLM polish startup and each request before
    /// falling back.
    pub llm_polish_timeout_ms: u64,
    /// LLM keepalive heartbeat interval, seconds. A ping fires when the sidecar
    /// stdin is idle for this long, keeping Metal prefill kernels warm so the
    /// first polish after an ASR recording is fast (ASR and polish share one
    /// GPU; without a mid-recording ping, ASR evicts the LLM's kernel working
    /// set and the post-ASR polish pays a ~3s cold ramp). ~1s is required to
    /// reliably fire during a short recording's final-generate window; 0
    /// disables. Override at runtime with `SUNOTO_LLM_POLISH_KEEPALIVE_S`.
    pub llm_polish_keepalive_secs: f64,
    pub polish: PolishConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: "Ctrl+F1".to_string(),
            microphone: "auto".to_string(),
            backend: default_backend().to_string(),
            profile_ms: default_profile_ms(),
            preroll_ms: 300,
            final_timeout_ms: 8000,
            final_timeout_rtf: 3.0,
            sidecar_python: None,
            sidecar_script: None,
            asr_device: None,
            asr_model: None,
            allow_enter_and_tab: false,
            overlay_enabled: true,
            overlay_backend: default_overlay_backend().to_string(),
            polish_enabled: true,
            llm_polish_enabled: true,
            llm_polish_python: None,
            llm_polish_script: None,
            llm_polish_model_path: None,
            llm_polish_model: DEFAULT_LLM_POLISH_MODEL.to_string(),
            llm_polish_mode: DEFAULT_LLM_POLISH_MODE.to_string(),
            llm_polish_timeout_ms: 10_000,
            llm_polish_keepalive_secs: 1.0,
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

    pub fn llm_polish_command(&self) -> (String, Vec<String>, Vec<(String, String)>) {
        let root = repo_root();
        let python = self.llm_polish_python.clone().unwrap_or_else(|| {
            let venv = root.join(".venv-llm-polish-mac/bin/python");
            if venv.is_file() {
                return venv.to_string_lossy().into_owned();
            }
            "python3".to_string()
        });
        let script = self.llm_polish_script.clone().unwrap_or_else(|| {
            root.join("services/polish/llm_polish_sidecar.py")
                .to_string_lossy()
                .into_owned()
        });
        let mut envs = vec![(
            "SUNOTO_ROOT".to_string(),
            root.to_string_lossy().into_owned(),
        )];
        // An explicit path always wins; otherwise resolve the named profile to
        // a repo-relative GGUF so the sidecar never silently falls back to its
        // bundled (Gemma) default when a daemon/bench asks for Phi.
        let resolved_model_path = self
            .llm_polish_model_path
            .as_ref()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .or_else(|| {
                llm_polish_model_relative(&self.llm_polish_model)
                    .map(|relative| root.join(relative).to_string_lossy().into_owned())
            });
        if let Some(model_path) = resolved_model_path {
            envs.push((
                "SUNOTO_LLM_POLISH_MODEL_PATH".to_string(),
                model_path,
            ));
        }
        // Always pass the dispatch mode so the live daemon runs the same
        // completion path we benchmarked (defaults to constrained_one_call);
        // without this the sidecar silently uses its one_pass_minimal default,
        // which is a different (slower, less-validated) code path.
        envs.push((
            "SUNOTO_LLM_POLISH_MODE".to_string(),
            self.llm_polish_mode.clone(),
        ));
        envs.push((
            "SUNOTO_LLM_POLISH_KEEPALIVE_S".to_string(),
            format!("{}", self.llm_polish_keepalive_secs),
        ));
        (python, vec![script], envs)
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.backend.as_str() {
            "mock"
            | "nemotron"
            | "nemotron_offline"
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
                // Parakeet-MLX uses MLX's default Apple GPU/CPU selection.
                // It has no torch device concept.
                "parakeet_mlx_offline" | "parakeet_mlx_streaming" => {
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
        }?;
        if self.llm_polish_enabled && self.llm_polish_timeout_ms == 0 {
            return Err("llm_polish_timeout_ms must be greater than zero".to_string());
        }
        let has_explicit_model_path = self
            .llm_polish_model_path
            .as_ref()
            .map(|path| !path.trim().is_empty())
            .unwrap_or(false);
        if !has_explicit_model_path
            && llm_polish_model_relative(&self.llm_polish_model).is_none()
        {
            return Err(format!(
                "unknown llm_polish_model {:?}; use phi4_mini or gemma4_e2b, or set llm_polish_model_path",
                self.llm_polish_model
            ));
        }
        if self.llm_polish_enabled
            && !ALLOWED_LLM_POLISH_MODES.contains(&self.llm_polish_mode.as_str())
        {
            return Err(format!(
                "unknown llm_polish_mode {:?}; use constrained_one_call, one_pass_minimal, or two_step",
                self.llm_polish_mode
            ));
        }
        Ok(())
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
        assert_eq!(partial.backend, default_backend());
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
        if cfg!(target_os = "macos") {
            assert!(args[0].ends_with("services/asr/parakeet_mlx_streaming_sidecar.py"));
            assert_eq!(
                args[1..],
                [
                    "--profile-ms".to_string(),
                    "560".to_string(),
                    "--model".to_string(),
                    DEFAULT_PARAKEET_MLX_MODEL.to_string(),
                ]
            );
        } else {
            assert_eq!(python, "python3");
            assert!(args[0].ends_with("services/asr/mock_sidecar.py"));
        }
        settings.backend = "nemotron".to_string();
        settings.profile_ms = 80;
        let (_python, args) = settings.sidecar_command().unwrap();
        assert!(args[0].ends_with("services/asr/nemotron_sidecar.py"));
        assert_eq!(args[1..], ["--profile-ms".to_string(), "80".to_string()]);
        settings.backend = "imaginary".to_string();
        assert!(settings.sidecar_command().is_err());
    }

    #[test]
    fn llm_polish_defaults_to_enabled_phi4_mini_sidecar() {
        let settings = Settings::default();
        assert!(settings.llm_polish_enabled);
        assert_eq!(settings.llm_polish_model, "phi4_mini");
        assert_eq!(settings.llm_polish_mode, "constrained_one_call");
        assert_eq!(settings.llm_polish_timeout_ms, 10_000);
        let (python, args, envs) = settings.llm_polish_command();
        if cfg!(target_os = "macos") {
            assert!(python.ends_with(".venv-llm-polish-mac/bin/python") || python == "python3");
        }
        assert_eq!(args.len(), 1);
        assert!(args[0].ends_with("services/polish/llm_polish_sidecar.py"));
        assert!(envs.iter().any(|(key, _)| key == "SUNOTO_ROOT"));
        // The named default profile is resolved to a concrete path so the
        // sidecar never silently falls back to its bundled Gemma default.
        let model_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODEL_PATH")
            .map(|(_, value)| value.as_str());
        assert!(
            model_env.unwrap_or("").ends_with(
                "models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf"
            ),
            "phi model path was {model_env:?}"
        );
        // The dispatch mode is always pushed so the live daemon runs the
        // benchmarked constrained_one_call path instead of the sidecar's
        // one_pass_minimal default.
        let mode_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODE")
            .map(|(_, value)| value.as_str());
        assert_eq!(mode_env, Some("constrained_one_call"));
    }

    #[test]
    fn llm_polish_command_resolves_gemma4_e2b_profile() {
        let settings = Settings {
            llm_polish_model: "gemma4_e2b".to_string(),
            ..Settings::default()
        };
        let (_python, _args, envs) = settings.llm_polish_command();
        let model_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODEL_PATH")
            .map(|(_, value)| value.as_str());
        assert!(
            model_env.unwrap_or("").ends_with(
                "models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf"
            ),
            "gemma model path was {model_env:?}"
        );
    }

    #[test]
    fn llm_polish_command_explicit_path_overrides_profile() {
        let settings = Settings {
            llm_polish_model: "gemma4_e2b".to_string(),
            llm_polish_model_path: Some("/tmp/custom-model.gguf".to_string()),
            ..Settings::default()
        };
        let (_python, _args, envs) = settings.llm_polish_command();
        let model_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODEL_PATH")
            .map(|(_, value)| value.as_str());
        assert_eq!(model_env, Some("/tmp/custom-model.gguf"));
    }

    #[test]
    fn loaded_config_without_model_field_defaults_to_phi4_mini() {
        let path = std::env::temp_dir().join(format!(
            "sunoto-settings-llm-default-{}.json",
            std::process::id()
        ));
        // The existing macOS config predates the model field; loading it must
        // resolve to the Phi default rather than rejecting.
        std::fs::write(
            &path,
            "{\"llm_polish_enabled\": true, \"llm_polish_timeout_ms\": 30000}",
        )
        .unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.llm_polish_model, "phi4_mini");
        assert_eq!(loaded.llm_polish_mode, "constrained_one_call");
        assert!(loaded.validate().is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validation_rejects_unknown_llm_polish_model_name() {
        let settings = Settings {
            llm_polish_model: "does-not-exist".to_string(),
            ..Settings::default()
        };
        let error = settings.validate().unwrap_err();
        assert!(error.contains("unknown llm_polish_model"), "{error}");
        // An explicit path bypasses the named-profile check.
        let with_path = Settings {
            llm_polish_model: "does-not-exist".to_string(),
            llm_polish_model_path: Some("/tmp/anything.gguf".to_string()),
            ..Settings::default()
        };
        assert!(with_path.validate().is_ok());
    }

    #[test]
    fn llm_polish_mode_defaults_to_constrained_one_call_and_validates() {
        let settings = Settings::default();
        assert_eq!(settings.llm_polish_mode, "constrained_one_call");
        // The mode is pushed even when polish is disabled so a future opt-in
        // run never silently hits the sidecar's one_pass_minimal default.
        let (_python, _args, envs) = settings.llm_polish_command();
        let mode_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODE")
            .map(|(_, value)| value.as_str());
        assert_eq!(mode_env, Some("constrained_one_call"));
        // Explicit legacy mode still passes validation when enabled.
        let legacy = Settings {
            llm_polish_enabled: true,
            llm_polish_mode: "one_pass_minimal".to_string(),
            ..Settings::default()
        };
        assert!(legacy.validate().is_ok());
        let (_python, _args, envs) = legacy.llm_polish_command();
        let mode_env = envs
            .iter()
            .find(|(key, _)| key == "SUNOTO_LLM_POLISH_MODE")
            .map(|(_, value)| value.as_str());
        assert_eq!(mode_env, Some("one_pass_minimal"));
    }

    #[test]
    fn validation_rejects_unknown_llm_polish_mode_name() {
        let settings = Settings {
            llm_polish_enabled: true,
            llm_polish_mode: "fancy_mode".to_string(),
            ..Settings::default()
        };
        let error = settings.validate().unwrap_err();
        assert!(error.contains("unknown llm_polish_mode"), "{error}");
        // An unknown mode is allowed when polish is disabled (validated only
        // at opt-in), matching the model-name-check behaviour.
        let disabled = Settings {
            llm_polish_enabled: false,
            llm_polish_mode: "fancy_mode".to_string(),
            ..Settings::default()
        };
        assert!(disabled.validate().is_ok());
    }

    #[test]
    fn validation_rejects_zero_llm_polish_timeout_when_enabled() {
        let settings = Settings {
            llm_polish_enabled: true,
            llm_polish_timeout_ms: 0,
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
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
            backend: "nemotron".to_string(),
            asr_device: Some("cuda".to_string()),
            ..Settings::default()
        };
        assert!(cuda.validate().is_ok());
        let cpu = Settings {
            backend: "nemotron".to_string(),
            asr_device: Some("cpu".to_string()),
            ..Settings::default()
        };
        assert!(cpu.validate().is_ok());
        let mps = Settings {
            backend: "nemotron".to_string(),
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
        let settings = Settings {
            overlay_backend: "auto".to_string(),
            ..Settings::default()
        };
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

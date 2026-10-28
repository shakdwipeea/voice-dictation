use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use sunoto_polish::PolishConfig;

/// The speech model: Parakeet TDT 0.6B v2, English. It beat the
/// multilingual v3 on every English test at the same speed and memory
/// (tools/vocab, 2026-10-18), so it is the only model the app offers.
const DEFAULT_PARAKEET_MLX_MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v2";
/// Sunoto's bf16 copy of the default model, under Application Support.
/// setup downloads it in place of the 2.5 GB float32 original; the sidecar
/// casts the original to bf16 on load anyway, so the weights are identical.
const LOCAL_PARAKEET_MLX_MODEL: &str = "models/parakeet-tdt-0.6b-v2-bf16";
/// The model before 0.1.12: multilingual v3, and Sunoto's bf16 copy of it.
const RETIRED_PARAKEET_MLX_REPO: &str = "mlx-community/parakeet-tdt-0.6b-v3";
const RETIRED_PARAKEET_MLX_COPY: &str = "models/parakeet-tdt-0.6b-v3-bf16";
/// Models Sunoto downloaded for v3, relative to its Application Support
/// folder; they are deleted once v2 is in. Sunoto's bf16 copy (1.25 GB),
/// and the original (2.5 GB) in the Hugging Face cache a release install
/// keeps there (`setup::apply_bundle_environment` points HF_HOME at
/// `sunoto/hf`). The shared ~/.cache/huggingface is never touched: other
/// tools may have downloaded the same model.
const RETIRED_PARAKEET_MLX_MODELS: &[&str] = &[
    RETIRED_PARAKEET_MLX_COPY,
    "hf/hub/models--mlx-community--parakeet-tdt-0.6b-v3",
    "hf/hub/.locks/models--mlx-community--parakeet-tdt-0.6b-v3",
];

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
const ALLOWED_LLM_POLISH_MODES: &[&str] = &["constrained_one_call", "one_pass_minimal", "two_step"];

/// Resolve a named LLM polish profile to its repo-relative GGUF path. Used
/// both to build the sidecar env and to validate config. Returns None for an
/// unknown name so callers can surface a config error instead of silently
/// falling back to the Python sidecar's bundled default.
fn llm_polish_model_relative(name: &str) -> Option<&'static str> {
    match name {
        "phi4_mini" => {
            Some("models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf")
        }
        "gemma4_e2b" => {
            Some("models/llm-polish-hf/gemma-4-e2b-it-q4/google_gemma-4-E2B-it-Q4_K_M.gguf")
        }
        _ => None,
    }
}

fn default_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "parakeet_mlx_offline"
    } else {
        "mock"
    }
}

/// Right Option on a Mac: one key under the right thumb that does nothing on
/// its own. Linux keeps a chord, since X11 grabs need a key plus modifiers.
fn default_shortcut() -> &'static str {
    if cfg!(target_os = "macos") {
        "RightOption"
    } else {
        "Ctrl+F1"
    }
}

/// The shortcut as people read it: "Right Option", "Control-F1".
pub fn shortcut_label(shortcut: &str) -> String {
    let squashed: String = shortcut
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect::<String>()
        .to_ascii_lowercase();
    let lone = match squashed.as_str() {
        "rightoption" | "rightalt" => Some("Right Option"),
        "rightcommand" | "rightcmd" => Some("Right Command"),
        "rightcontrol" | "rightctrl" => Some("Right Control"),
        "rightshift" => Some("Right Shift"),
        _ => None,
    };
    if let Some(lone) = lone {
        return lone.to_string();
    }
    shortcut
        .split('+')
        .map(str::trim)
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Control".to_string(),
            "shift" => "Shift".to_string(),
            "alt" | "option" => "Option".to_string(),
            "cmd" | "command" | "super" => "Command".to_string(),
            "esc" | "escape" => "Esc".to_string(),
            key if key.len() > 1
                && key.starts_with('f')
                && key[1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                key.to_uppercase()
            }
            _ => {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join("-")
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
    /// Dictation shortcut: a chord such as "Ctrl+F1", or on macOS a single
    /// right-hand modifier such as "RightOption". Tap it to start recording
    /// and tap it again to paste; Esc throws the recording away (macOS).
    pub shortcut: String,
    /// Enables the separate voice-command capture path. It remains off until
    /// the suggestion palette is connected; enabling it must never change the
    /// behavior of the normal dictation shortcut.
    pub system_mode_enabled: bool,
    /// Push-to-talk shortcut reserved for System mode, e.g. "Ctrl+F2".
    pub system_shortcut: String,
    /// Send only clean deterministic no-matches to the constrained LLM
    /// intent router. Unsafe inputs are rejected before this fallback.
    pub system_llm_fallback_enabled: bool,
    /// User-approved roots for Voice Spotlight file/folder/project search.
    /// Relative values resolve below the current user's home directory.
    pub system_search_roots: Vec<String>,
    /// PulseAudio source name, or "auto" for the first physical microphone.
    pub microphone: String,
    /// ASR backend. Linux config init defaults to "mock"; macOS config init
    /// defaults to "parakeet_mlx_offline".
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
    /// macOS: snapshot the clipboard before a paste and put it back about
    /// 300 ms later, if nothing else wrote to it in between. Off only for
    /// apps that read the pasteboard late.
    pub clipboard_restore: bool,
    /// GTK4 pill overlay (UI sidecar). When it cannot start (e.g. GTK4 is
    /// not installed) the daemon falls back to the native X11 bubble.
    pub overlay_enabled: bool,
    /// "auto", "x11", "wayland", or "macos" for the overlay sidecar backend.
    pub overlay_backend: String,
    /// macOS: "auto" grows the pill out of the camera notch on a screen that
    /// has one and floats it under the menu bar elsewhere; "pill" always
    /// floats it (for people who run another notch app). Linux ignores it.
    pub overlay_style: String,
    /// false = raw transcription, true = deterministic cleanup pipeline.
    pub polish_enabled: bool,
    /// Optional local LLM cleanup after deterministic polish. Off by default:
    /// it adds roughly a second per dictation, holds a 2.7 GB model in memory,
    /// and dictation waits for its warm-up at startup. `setup --with-llm`
    /// downloads the model and turns it on.
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
    /// LLM keepalive heartbeat interval, seconds. While a dictation session
    /// is in flight (key press until 20 s after the polish result) the
    /// sidecar pings the GPU this often, keeping Metal prefill kernels warm so
    /// the polish right after ASR is fast (ASR and polish share one GPU;
    /// without a mid-recording ping, ASR evicts the LLM's kernel working set
    /// and the post-ASR polish pays a ~3s cold ramp). ~1s is required to
    /// reliably fire during a short recording's final-generate window; 0
    /// disables. Override at runtime with `SUNOTO_LLM_POLISH_KEEPALIVE_S`.
    pub llm_polish_keepalive_secs: f64,
    /// Ping all day instead of only around sessions. Costs battery; only for
    /// profiling the old behaviour.
    pub llm_polish_keepalive_always: bool,
    /// How long the microphone stays on after a dictation ends. 0, the
    /// default, turns it off right away, so it is on only while recording
    /// and a tap turns it back on. A longer value keeps it on for quick
    /// follow-ups, which then get the 300 ms pre-roll, at the cost of the
    /// menu-bar microphone indicator staying on.
    pub capture_idle_stop_secs: u64,
    /// What a release install does when a newer release is published:
    /// "install" (the default) downloads and verifies it, then installs it
    /// after 10 minutes without dictation; "notify" only logs that it is
    /// available; "off" never checks. Builds from a checkout never update.
    pub auto_update: String,
    /// Write transcripts and polish stages to the log. Off replaces the text
    /// with its length and a short hash.
    pub log_transcripts: bool,
    /// Keep a private history of dictations on this machine: one JSON line
    /// per dictation under `history/dictations/` next to the config (macOS)
    /// or in `$XDG_DATA_HOME/sunoto/history` (Linux). On by default (owner
    /// decision, 2026-10-16); setup says so and `false` turns it off. See
    /// docs/local-history-plan-2026-10-16.md.
    pub history_enabled: bool,
    /// Days of history to keep; older day files are deleted. 0 keeps all.
    pub history_retention_days: u32,
    /// Apps whose dictations are recorded without any text (bundle ids or
    /// app names on macOS, WM_CLASS names on X11). Password managers by
    /// default; password fields are always left out.
    pub history_exclude_apps: Vec<String>,
    /// Progressive LLM-polish insertion (default off). When on, the sidecar
    /// streams decoded tokens as `polish_chunk` deltas and the daemon types
    /// them into the focused window as they arrive (CGEvent keypresses, the
    /// existing typing path), overlapping decode with paste so the user sees
    /// the first character at TTFT instead of after the whole completion.
    /// Only the `constrained_one_call` mode streams; OK (clean) transcripts
    /// take the unchanged single-paste fast path. See
    /// docs/llm-polish-streaming-plan.md for the trade-offs (typing may be
    /// dropped by apps that ignore CGEvent keypresses; on such failure the
    /// daemon keeps the typed text and logs a warning).
    pub llm_polish_stream_insert: bool,
    /// Favour unusual words from around you while transcribing: names on the
    /// screen, in the project you are working on (a terminal's folder, an
    /// editor's window title), and among your project folders and apps. They
    /// are read when a dictation starts, used for that dictation only, and
    /// never written anywhere. Needs Accessibility for the screen; only the
    /// parakeet_mlx_offline backend uses the list.
    pub word_list_enabled: bool,
    /// Your own words to favour in every dictation, spelled as you want them
    /// written ("Kubernetes", "Priya").
    pub word_list: Vec<String>,
    /// Also take names from your documents: the subfolders of Documents and
    /// Desktop (clients, cases, courses, books) and documents opened in the
    /// last two weeks. Off by default: macOS asks once before an app may
    /// read Documents and Desktop.
    pub word_list_documents: bool,
    /// Also take people's names from Contacts. Off by default: macOS asks
    /// once, the first time the list is built with this on.
    pub word_list_contacts: bool,
    pub polish: PolishConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shortcut: default_shortcut().to_string(),
            system_mode_enabled: false,
            system_shortcut: "Ctrl+F2".to_string(),
            system_llm_fallback_enabled: true,
            system_search_roots: vec![
                "Desktop".into(),
                "Documents".into(),
                "Downloads".into(),
                "workspace".into(),
            ],
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
            clipboard_restore: true,
            overlay_enabled: true,
            overlay_backend: default_overlay_backend().to_string(),
            overlay_style: "auto".to_string(),
            polish_enabled: true,
            llm_polish_enabled: false,
            llm_polish_python: None,
            llm_polish_script: None,
            llm_polish_model_path: None,
            llm_polish_model: DEFAULT_LLM_POLISH_MODEL.to_string(),
            llm_polish_mode: DEFAULT_LLM_POLISH_MODE.to_string(),
            llm_polish_timeout_ms: 10_000,
            llm_polish_keepalive_secs: 1.0,
            llm_polish_keepalive_always: false,
            capture_idle_stop_secs: 0,
            auto_update: "install".to_string(),
            log_transcripts: true,
            history_enabled: true,
            history_retention_days: 90,
            history_exclude_apps: crate::history::default_exclude_apps(),
            llm_polish_stream_insert: false,
            word_list_enabled: true,
            word_list: Vec::new(),
            word_list_documents: false,
            word_list_contacts: false,
            polish: PolishConfig::default(),
        }
    }
}

impl Settings {
    /// Where the word list looks, from the word_list_* settings.
    pub fn word_sources(&self) -> crate::vocabulary::Sources {
        // Rare English words ("hidradenitis") stay on the lists: they cut
        // LibriSpeech rare-word errors further and no common word changed.
        // The rest of the window is read when the focused box is nearly
        // empty (a chat, a reply), within 150 ms on the word-list thread.
        // Waking Electron apps' accessibility stays off: its cost to those
        // apps while in use was not measured.
        crate::vocabulary::Sources {
            documents: self.word_list_documents,
            recent_files: self.word_list_documents,
            system_contacts: self.word_list_contacts,
            keep_rare: true,
            walk_window: true,
            ..Default::default()
        }
    }

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

    /// Write only what the user chose: keys that differ from the defaults,
    /// plus any key the file already names. Everything else stays out, so it
    /// keeps following the defaults of later versions. `save` writes every
    /// key, which froze the defaults of the version that wrote the file.
    pub fn save_changes(&self, path: &Path) -> Result<(), String> {
        let mut file = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(serde_json::Value::Object(map)) => map,
                _ => return Err(format!("{} is not a JSON object", path.display())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let serde_json::Value::Object(current) =
            serde_json::to_value(self).expect("settings serialize")
        else {
            unreachable!("settings serialize to an object");
        };
        let defaults = serde_json::to_value(Settings::default()).expect("settings serialize");
        for (key, value) in current {
            if file.contains_key(&key) || defaults.get(&key) != Some(&value) {
                file.insert(key, value);
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let payload = serde_json::to_string_pretty(&file).expect("settings serialize");
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
            self.parakeet_model(),
        ]
    }

    /// The model the Parakeet sidecar loads: `asr_model` when set, else the
    /// local bf16 copy once setup has fetched it, else the old model of an
    /// updated install while the app fetches that copy
    /// (`stopgap_parakeet_model`), else the Hugging Face original.
    pub fn parakeet_model(&self) -> String {
        let local = local_parakeet_model_dir().filter(|dir| model_dir_complete(dir));
        let stopgap = local.is_none().then(stopgap_parakeet_model).flatten();
        choose_parakeet_model(
            self.asr_model.as_deref(),
            local.as_deref(),
            stopgap.as_deref(),
        )
    }

    /// The Parakeet sidecar runs the model Sunoto picks, not one set in
    /// `asr_model`.
    pub fn runs_default_parakeet(&self) -> bool {
        self.backend.starts_with("parakeet_mlx")
            && self.asr_model.as_deref().is_none_or(str::is_empty)
    }

    /// The sidecar runs the old model of an updated install, so the app
    /// should fetch the copy of the current one.
    pub fn on_stopgap_model(&self) -> bool {
        self.runs_default_parakeet() && stopgap_parakeet_model().is_some()
    }

    /// No speech model is on disk at all: an app opened from a disk image,
    /// with no installer run before it. The app fetches Sunoto's copy before
    /// it starts the sidecar, which would otherwise download the 2.5 GB
    /// original itself, with nothing to show for it.
    pub fn needs_model_download(&self) -> bool {
        if !self.runs_default_parakeet() {
            return false;
        }
        let (Some(local), Some(home)) = (local_parakeet_model_dir(), std::env::var_os("HOME"))
        else {
            return false;
        };
        let Some(support) = local.parent().and_then(Path::parent) else {
            return false;
        };
        model_missing_in(&local, support, &hub_cache_dir(Path::new(&home)))
    }

    /// The GGUF the polish sidecar will load: the explicit path if set,
    /// otherwise the named profile under the repository root.
    pub fn llm_polish_model_file(&self) -> Option<PathBuf> {
        self.llm_polish_model_path
            .as_ref()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(expand_home(&path)))
            .or_else(|| {
                llm_polish_model_relative(&self.llm_polish_model)
                    .map(|relative| repo_root().join(relative))
            })
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
            .llm_polish_model_file()
            .map(|path| path.to_string_lossy().into_owned());
        if let Some(model_path) = resolved_model_path {
            envs.push(("SUNOTO_LLM_POLISH_MODEL_PATH".to_string(), model_path));
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
        if self.llm_polish_keepalive_always {
            envs.push((
                "SUNOTO_LLM_POLISH_KEEPALIVE_ALWAYS".to_string(),
                "1".to_string(),
            ));
        }
        if self.llm_polish_stream_insert {
            envs.push(("SUNOTO_LLM_POLISH_STREAM".to_string(), "1".to_string()));
        }
        (python, vec![script], envs)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.system_mode_enabled && self.system_shortcut.trim().is_empty() {
            return Err("system_shortcut must not be empty when System mode is enabled".into());
        }
        if self.system_mode_enabled
            && self
                .shortcut
                .trim()
                .eq_ignore_ascii_case(self.system_shortcut.trim())
        {
            return Err("system_shortcut must differ from the dictation shortcut".into());
        }
        if self.system_search_roots.len() > 16
            || self
                .system_search_roots
                .iter()
                .any(|root| root.trim().is_empty() || root.chars().any(char::is_control))
        {
            return Err(
                "system_search_roots must contain at most 16 non-empty single-line paths".into(),
            );
        }
        if self.system_search_roots.iter().any(|root| {
            let path = Path::new(root.trim());
            path == Path::new("/") || path.components().any(|part| part.as_os_str() == "..")
        }) {
            return Err(
                "system_search_roots must not be the filesystem root or contain parent traversal"
                    .into(),
            );
        }
        match self.backend.as_str() {
            "mock"
            | "nemotron"
            | "nemotron_offline"
            | "parakeet_mlx_offline"
            | "parakeet_mlx_streaming" => {}
            other => return Err(format!("unknown ASR backend: {other}")),
        }
        if !matches!(self.auto_update.as_str(), "install" | "notify" | "off") {
            return Err(format!(
                "unsupported auto_update {:?}; use install, notify, or off",
                self.auto_update
            ));
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
            "auto" | "x11" | "wayland" | "macos" | "mock" => Ok(()),
            other => Err(format!(
                "unsupported overlay_backend {other:?}; use auto, x11, wayland, macos, or mock"
            )),
        }?;
        match self.overlay_style.as_str() {
            "auto" | "pill" => Ok(()),
            other => Err(format!(
                "unsupported overlay_style {other:?}; use auto or pill"
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
        if !has_explicit_model_path && llm_polish_model_relative(&self.llm_polish_model).is_none() {
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
        if self.llm_polish_stream_insert && self.llm_polish_mode != "constrained_one_call" {
            return Err(format!(
                "llm_polish_stream_insert is on but llm_polish_mode is {:?}; streaming is only implemented for constrained_one_call",
                self.llm_polish_mode
            ));
        }
        Ok(())
    }

    pub fn system_search_root_paths(&self) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.system_search_roots
            .iter()
            .map(|root| PathBuf::from(root.trim()))
            .filter_map(|root| {
                if root.is_absolute() {
                    Some(root)
                } else {
                    home.as_ref().map(|home| home.join(root))
                }
            })
            .collect()
    }

    /// Command and environment for the overlay UI sidecar. Linux uses the
    /// GTK Python module; macOS uses the native Swift NSPanel helper.
    pub fn overlay_command(&self) -> (String, Vec<String>, Vec<(String, String)>) {
        if self.overlay_backend == "mock" {
            return (
                "python3".to_string(),
                vec![
                    repo_root()
                        .join("services/ui/mock_overlay_sidecar.py")
                        .to_string_lossy()
                        .into_owned(),
                ],
                vec![("SUNOTO_OVERLAY_BACKEND".to_string(), "mock".to_string())],
            );
        }
        let src = repo_root().join("src");
        if cfg!(target_os = "macos") && self.overlay_backend == "macos" {
            let root = repo_root();
            let envs = vec![
                ("SUNOTO_OVERLAY_BACKEND".to_string(), "macos".to_string()),
                (
                    "SUNOTO_OVERLAY_STYLE".to_string(),
                    self.overlay_style.clone(),
                ),
            ];
            // Inside Sunoto.app the overlay sits next to the daemon; a
            // development run finds it in target/release.
            let sibling = std::env::current_exe()
                .ok()
                .map(|exe| exe.with_file_name("sunoto-overlay"))
                .filter(|path| path.is_file());
            let binary = sibling.unwrap_or_else(|| root.join("target/release/sunoto-overlay"));
            if binary.is_file() {
                return (binary.to_string_lossy().into_owned(), vec![], envs);
            }
            return (
                "swift".to_string(),
                vec![
                    root.join("services/macos/sunoto-overlay.swift")
                        .to_string_lossy()
                        .into_owned(),
                ],
                envs,
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

/// Expand a leading `~/` so a config written by `setup` stays readable.
pub fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// Sunoto's folder under Application Support, where its speech models
/// live. macOS only: Linux uses Nemotron.
fn support_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/sunoto"))
}

/// Where setup puts the bf16 speech model.
pub fn local_parakeet_model_dir() -> Option<PathBuf> {
    Some(support_dir()?.join(LOCAL_PARAKEET_MLX_MODEL))
}

/// Both files are in place. Each one is moved there only after its size
/// and SHA-256 are checked, so present means verified.
pub fn model_dir_complete(dir: &Path) -> bool {
    dir.join("config.json").is_file() && dir.join("model.safetensors").is_file()
}

/// The model an install that updated from 0.1.11 or older already has,
/// while Sunoto's copy of the current one is missing and the Hugging Face
/// cache has no original: Sunoto's v3 copy, else the v3 original in the
/// cache. An update has minutes to start the new version (update.rs), too
/// few for 1.24 GB, so the app runs this one while it fetches the copy in
/// the background, then moves to the copy and deletes what Sunoto
/// downloaded for v3.
pub fn stopgap_parakeet_model() -> Option<String> {
    let local = local_parakeet_model_dir()?;
    let support = local.parent()?.parent()?;
    let home = PathBuf::from(std::env::var_os("HOME")?);
    stopgap_in(&local, support, &hub_cache_dir(&home))
}

fn stopgap_in(local: &Path, support: &Path, hub: &Path) -> Option<String> {
    if model_dir_complete(local) || hub_has_model(hub, DEFAULT_PARAKEET_MLX_MODEL) {
        return None;
    }
    let copy = support.join(RETIRED_PARAKEET_MLX_COPY);
    if model_dir_complete(&copy) {
        return Some(copy.to_string_lossy().into_owned());
    }
    hub_has_model(hub, RETIRED_PARAKEET_MLX_REPO).then(|| RETIRED_PARAKEET_MLX_REPO.to_string())
}

fn model_missing_in(local: &Path, support: &Path, hub: &Path) -> bool {
    !model_dir_complete(local)
        && !hub_has_model(hub, DEFAULT_PARAKEET_MLX_MODEL)
        && stopgap_in(local, support, hub).is_none()
}

/// The Hugging Face cache the sidecar reads, found the way huggingface_hub
/// finds it.
fn hub_cache_dir(home: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("HF_HOME") {
        return PathBuf::from(dir).join("hub");
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("huggingface/hub")
}

/// Some revision of `repo` in the cache has both files.
fn hub_has_model(hub: &Path, repo: &str) -> bool {
    let snapshots = hub
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    std::fs::read_dir(snapshots).is_ok_and(|mut revisions| {
        revisions.any(|revision| revision.is_ok_and(|r| model_dir_complete(&r.path())))
    })
}

/// The retired models Sunoto downloaded that are still on disk.
pub fn retired_model_dirs() -> Vec<PathBuf> {
    support_dir()
        .map(|support| retired_model_dirs_in(&support))
        .unwrap_or_default()
}

fn retired_model_dirs_in(support: &Path) -> Vec<PathBuf> {
    RETIRED_PARAKEET_MLX_MODELS
        .iter()
        .map(|retired| support.join(retired))
        .filter(|dir| dir.is_dir())
        .collect()
}

fn choose_parakeet_model(
    configured: Option<&str>,
    local: Option<&Path>,
    stopgap: Option<&str>,
) -> String {
    configured
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| local.map(|dir| dir.to_string_lossy().into_owned()))
        .or_else(|| stopgap.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_PARAKEET_MLX_MODEL.to_string())
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
/// Transcript text as it should appear in the log: verbatim when
/// `log_transcripts` is on, otherwise its length and a short hash so
/// sessions can still be correlated.
pub fn transcript_for_log(text: &str, log_transcripts: bool) -> String {
    if log_transcripts {
        return format!("{text:?}");
    }
    let mut hash: u32 = 2166136261;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16777619);
    }
    format!("<{} chars, {hash:08x}>", text.chars().count())
}

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
    fn transcript_for_log_is_verbatim_or_length_and_hash() {
        assert_eq!(transcript_for_log("hello there", true), "\"hello there\"");
        let redacted = transcript_for_log("hello there", false);
        assert!(redacted.starts_with("<11 chars, "));
        assert!(!redacted.contains("hello"));
        assert_eq!(redacted, transcript_for_log("hello there", false));
        assert_ne!(redacted, transcript_for_log("hello where", false));
    }

    #[test]
    fn shortcut_defaults_to_right_option_on_macos() {
        let settings = Settings::default();
        if cfg!(target_os = "macos") {
            assert_eq!(settings.shortcut, "RightOption");
        } else {
            assert_eq!(settings.shortcut, "Ctrl+F1");
        }
        // A config written while hold-to-talk was optional still loads.
        let loaded: Settings =
            serde_json::from_str(r#"{"shortcut": "Ctrl+F1", "tap_to_lock": true}"#).unwrap();
        assert_eq!(loaded.shortcut, "Ctrl+F1");
    }

    #[test]
    fn shortcut_labels_read_like_macos_menus() {
        assert_eq!(shortcut_label("RightOption"), "Right Option");
        assert_eq!(shortcut_label("right-command"), "Right Command");
        assert_eq!(shortcut_label("Ctrl+F1"), "Control-F1");
        assert_eq!(shortcut_label("cmd+shift+space"), "Command-Shift-Space");
    }

    #[test]
    fn dictation_history_is_on_by_default() {
        let settings = Settings::default();
        assert!(settings.history_enabled);
        assert_eq!(settings.history_retention_days, 90);
    }

    #[test]
    fn quiet_idle_defaults() {
        let settings = Settings::default();
        assert_eq!(settings.capture_idle_stop_secs, 0);
        assert!(!settings.llm_polish_keepalive_always);
        assert!(settings.log_transcripts);
        assert!(settings.clipboard_restore);
        assert!(
            !settings
                .llm_polish_command()
                .2
                .iter()
                .any(|(key, _)| key == "SUNOTO_LLM_POLISH_KEEPALIVE_ALWAYS")
        );
    }

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
    fn save_changes_writes_only_chosen_keys() {
        let path = std::env::temp_dir().join(format!(
            "sunoto-settings-changes-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        // A fresh install chooses nothing, so nothing is frozen.
        Settings::default().save_changes(&path).unwrap();
        let written = |path: &Path| -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        assert_eq!(written(&path), serde_json::json!({}));
        // A changed key is written; a key the file names stays in step,
        // even when it goes back to its default.
        std::fs::write(
            &path,
            r#"{"llm_polish_enabled": true, "microphone": "auto"}"#,
        )
        .unwrap();
        let settings = Settings {
            llm_polish_enabled: false,
            profile_ms: 80,
            ..Settings::default()
        };
        settings.save_changes(&path).unwrap();
        assert_eq!(
            written(&path),
            serde_json::json!({"llm_polish_enabled": false, "microphone": "auto", "profile_ms": 80})
        );
        assert_eq!(Settings::load(&path).unwrap(), settings);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn system_mode_defaults_are_safe_and_do_not_claim_the_hotkey() {
        let settings = Settings::default();
        assert!(!settings.system_mode_enabled);
        assert_eq!(settings.system_shortcut, "Ctrl+F2");
        assert!(settings.system_llm_fallback_enabled);
        assert!(settings.system_search_roots.contains(&"workspace".into()));
    }

    #[test]
    fn system_search_roots_are_bounded_and_resolve_below_home() {
        let traversal = Settings {
            system_search_roots: vec!["Documents/../Library".into()],
            ..Settings::default()
        };
        assert!(traversal.validate().is_err());
        let root = Settings {
            system_search_roots: vec!["/".into()],
            ..Settings::default()
        };
        assert!(root.validate().is_err());
        let settings = Settings::default();
        assert!(
            settings
                .system_search_root_paths()
                .iter()
                .any(|path| path.ends_with("workspace"))
        );
    }

    #[test]
    fn enabled_system_mode_requires_a_distinct_shortcut() {
        let same_shortcut = Settings {
            shortcut: "Ctrl+F1".into(),
            system_mode_enabled: true,
            system_shortcut: "ctrl+f1".into(),
            ..Settings::default()
        };
        assert!(same_shortcut.validate().is_err());

        let empty_shortcut = Settings {
            system_mode_enabled: true,
            system_shortcut: "  ".into(),
            ..Settings::default()
        };
        assert!(empty_shortcut.validate().is_err());

        let valid = Settings {
            system_mode_enabled: true,
            ..Settings::default()
        };
        assert!(valid.validate().is_ok());
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
            assert!(args[0].ends_with("services/asr/parakeet_mlx_offline_sidecar.py"));
            assert_eq!(
                args[1..],
                [
                    "--profile-ms".to_string(),
                    "560".to_string(),
                    "--model".to_string(),
                    settings.parakeet_model(),
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
    fn llm_polish_defaults_to_off_with_the_phi4_mini_sidecar() {
        let settings = Settings::default();
        assert!(!settings.llm_polish_enabled);
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
        // Streaming insert defaults off; the env var is only pushed when on.
        assert!(!settings.llm_polish_stream_insert);
        assert!(
            !envs
                .iter()
                .any(|(key, _)| key == "SUNOTO_LLM_POLISH_STREAM")
        );
    }

    #[test]
    fn llm_polish_stream_insert_pushes_env_when_on() {
        let settings = Settings {
            llm_polish_stream_insert: true,
            ..Settings::default()
        };
        let (_python, _args, envs) = settings.llm_polish_command();
        assert_eq!(
            envs.iter()
                .find(|(key, _)| key == "SUNOTO_LLM_POLISH_STREAM")
                .map(|(_, value)| value.as_str()),
            Some("1")
        );
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn validation_rejects_stream_insert_with_non_constrained_mode() {
        let settings = Settings {
            llm_polish_mode: "one_pass_minimal".to_string(),
            llm_polish_stream_insert: true,
            ..Settings::default()
        };
        let error = settings.validate().unwrap_err();
        assert!(
            error.contains("streaming is only implemented for constrained_one_call"),
            "{error}"
        );
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
        // Streaming default survives a config without the field.
        assert!(!loaded.llm_polish_stream_insert);
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
                settings.parakeet_model(),
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
                settings.parakeet_model(),
            ]
        );
    }

    #[test]
    fn parakeet_model_prefers_config_then_local_copy_then_stopgap_then_hub() {
        let local = Path::new("/models/parakeet-bf16");
        let old = Some("/models/old-bf16");
        assert_eq!(
            choose_parakeet_model(Some("mlx-community/parakeet-tdt-1.1b"), Some(local), old),
            "mlx-community/parakeet-tdt-1.1b"
        );
        assert_eq!(
            choose_parakeet_model(Some(""), Some(local), old),
            "/models/parakeet-bf16"
        );
        assert_eq!(
            choose_parakeet_model(None, Some(local), old),
            "/models/parakeet-bf16"
        );
        assert_eq!(choose_parakeet_model(None, None, old), "/models/old-bf16");
        assert_eq!(
            choose_parakeet_model(None, None, None),
            DEFAULT_PARAKEET_MLX_MODEL
        );
    }

    fn put_model(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.json"), "{}").unwrap();
        std::fs::write(dir.join("model.safetensors"), "x").unwrap();
    }

    #[test]
    fn an_updated_install_runs_its_old_model_until_the_new_copy_is_in() {
        let support = std::env::temp_dir().join(format!("sunoto-stopgap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&support);
        let local = support.join(LOCAL_PARAKEET_MLX_MODEL);
        let hub = support.join("hf/hub");
        let v3_hub = hub.join("models--mlx-community--parakeet-tdt-0.6b-v3/snapshots/abc");
        let v2_hub = hub.join("models--mlx-community--parakeet-tdt-0.6b-v2/snapshots/def");
        let v3_copy = support.join(RETIRED_PARAKEET_MLX_COPY);

        assert_eq!(
            stopgap_in(&local, &support, &hub),
            None,
            "nothing to fall back on"
        );
        put_model(&v3_hub);
        assert_eq!(
            stopgap_in(&local, &support, &hub).as_deref(),
            Some(RETIRED_PARAKEET_MLX_REPO)
        );
        put_model(&v3_copy);
        assert_eq!(
            stopgap_in(&local, &support, &hub),
            Some(v3_copy.to_string_lossy().into_owned()),
            "Sunoto's copy before the original"
        );
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("config.json"), "{}").unwrap();
        assert!(
            stopgap_in(&local, &support, &hub).is_some(),
            "a half-fetched copy does not count"
        );
        put_model(&local);
        assert_eq!(stopgap_in(&local, &support, &hub), None);
        std::fs::remove_dir_all(&local).unwrap();
        put_model(&v2_hub);
        assert_eq!(
            stopgap_in(&local, &support, &hub),
            None,
            "the original v2 is cached"
        );
        std::fs::remove_dir_all(&support).unwrap();
    }

    #[test]
    fn a_first_start_downloads_only_when_no_model_is_anywhere() {
        let support = std::env::temp_dir().join(format!("sunoto-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&support);
        let local = support.join(LOCAL_PARAKEET_MLX_MODEL);
        let hub = support.join("hf/hub");

        assert!(model_missing_in(&local, &support, &hub), "fresh Mac");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("config.json"), "{}").unwrap();
        assert!(
            model_missing_in(&local, &support, &hub),
            "a half-fetched copy is fetched again"
        );
        put_model(&local);
        assert!(!model_missing_in(&local, &support, &hub), "Sunoto's copy");
        std::fs::remove_dir_all(&local).unwrap();
        put_model(&hub.join("models--mlx-community--parakeet-tdt-0.6b-v2/snapshots/def"));
        assert!(
            !model_missing_in(&local, &support, &hub),
            "the original is cached"
        );
        std::fs::remove_dir_all(&hub).unwrap();
        put_model(&support.join(RETIRED_PARAKEET_MLX_COPY));
        assert!(
            !model_missing_in(&local, &support, &hub),
            "an update runs its old model meanwhile"
        );
        std::fs::remove_dir_all(&support).unwrap();
    }

    #[test]
    fn only_the_models_sunoto_retired_are_listed_for_deletion() {
        let support = std::env::temp_dir().join(format!("sunoto-retired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&support);
        let old_copy = support.join("models/parakeet-tdt-0.6b-v3-bf16");
        let old_hub = support.join("hf/hub/models--mlx-community--parakeet-tdt-0.6b-v3");
        let current = support.join(LOCAL_PARAKEET_MLX_MODEL);
        let other_hub = support.join("hf/hub/models--mlx-community--parakeet-tdt-0.6b-v2");
        for dir in [&old_copy, &old_hub, &current, &other_hub] {
            put_model(dir);
        }
        assert_eq!(retired_model_dirs_in(&support), vec![old_copy, old_hub]);
        std::fs::remove_dir_all(&support).unwrap();
    }

    #[test]
    fn sidecar_command_supports_parakeet_mlx_model_override() {
        for backend in ["parakeet_mlx_offline", "parakeet_mlx_streaming"] {
            let settings = Settings {
                backend: backend.to_string(),
                asr_model: Some("mlx-community/parakeet-tdt-1.1b".to_string()),
                ..Settings::default()
            };
            let (_python, args) = settings.sidecar_command().unwrap();
            assert_eq!(args.last().unwrap(), "mlx-community/parakeet-tdt-1.1b");
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
    fn auto_update_defaults_to_install_and_rejects_unknown_values() {
        assert_eq!(Settings::default().auto_update, "install");
        for value in ["install", "notify", "off"] {
            let settings = Settings {
                auto_update: value.to_string(),
                ..Settings::default()
            };
            assert!(settings.validate().is_ok(), "{value}");
        }
        let settings = Settings {
            auto_update: "always".to_string(),
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
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
    fn overlay_style_defaults_to_the_notch_and_accepts_only_auto_or_pill() {
        // Config files written before the setting existed get the island.
        let old: Settings = serde_json::from_str(r#"{"overlay_backend": "macos"}"#).unwrap();
        assert_eq!(old.overlay_style, "auto");
        assert!(old.validate().is_ok());

        let pill = Settings {
            overlay_style: "pill".to_string(),
            ..Settings::default()
        };
        assert!(pill.validate().is_ok());

        let island = Settings {
            overlay_style: "island".to_string(),
            ..Settings::default()
        };
        let error = island.validate().unwrap_err();
        assert!(error.contains("overlay_style"), "{error}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_overlay_gets_the_style_in_its_environment() {
        let settings = Settings {
            overlay_backend: "macos".to_string(),
            overlay_style: "pill".to_string(),
            ..Settings::default()
        };
        let (_program, _args, envs) = settings.overlay_command();
        assert!(
            envs.iter()
                .any(|(key, value)| key == "SUNOTO_OVERLAY_STYLE" && value == "pill")
        );
        assert!(
            envs.iter()
                .any(|(key, value)| key == "SUNOTO_OVERLAY_BACKEND" && value == "macos")
        );
    }

    #[test]
    fn validation_accepts_macos_overlay_backend() {
        let settings = Settings {
            overlay_backend: "macos".to_string(),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn mock_overlay_backend_is_headless_and_explicit() {
        let settings = Settings {
            overlay_backend: "mock".into(),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok());
        let (program, args, envs) = settings.overlay_command();
        assert_eq!(program, "python3");
        assert!(args[0].ends_with("services/ui/mock_overlay_sidecar.py"));
        assert!(envs.contains(&("SUNOTO_OVERLAY_BACKEND".into(), "mock".into())));
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

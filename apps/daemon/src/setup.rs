//! `sunoto-daemon setup`: build the app bundle, register it as a Login Item,
//! start it, and watch the running daemon's own health until it is ready.
//!
//! Why the daemon is the bundle's executable: macOS attaches Input
//! Monitoring and Accessibility grants to the process that uses them, and
//! disables a CGEventTap whose process has no responsible GUI context. With
//! the daemon launched directly by Launch Services there is exactly one
//! identity to grant ("Sunoto") and no wrapper process to keep alive.
//!
//! Why readiness is read over the control socket: permissions are per
//! identity, so a check run from a terminal would report the terminal's
//! grants, not the app's. The app reports its own probe results (hotkey
//! delivery, microphone capture, model load), and setup prints them.

use std::error::Error;
use std::ffi::{CString, c_char, c_int};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::settings::{self, Settings};

pub const BUNDLE_ID: &str = "com.earendil-works.sunoto";
pub const APP_NAME: &str = "Sunoto";
const LEGACY_LOGIN_ITEM: &str = "Sunoto Login";
const LEGACY_LAUNCHD_LABEL: &str = "com.earendil-works.sunoto";
/// Name of the file inside `Contents/Resources` that tells an app-bundle
/// launch where the repository (sidecar scripts, venvs, models) lives.
pub const ROOT_MARKER: &str = "sunoto-root";
/// A prebuilt (release) bundle carries the whole runtime here instead of a
/// marker: services/, src/, a relocatable Python with both sidecars'
/// packages, and the two venv-shaped symlinks the daemon resolves.
pub const EMBEDDED_ROOT: &str = "Contents/Resources/root";
/// Speech model fetched on first run; matches the daemon's default.
const ASR_MODEL_REPO: &str = "mlx-community/parakeet-tdt-0.6b-v2";
/// Sunoto's bf16 copy of ASR_MODEL_REPO, published as release assets: the
/// weights the sidecar ends up with after casting the original, at half the
/// 2.5 GB download. Name, SHA-256 and size of each file.
/// Made with tools/make_model_copy.py.
const ASR_MODEL_RELEASE: &str = "https://github.com/shakdwipeea/voice-dictation/releases/download/model-parakeet-tdt-0.6b-v2-bf16";
const ASR_MODEL_FILES: [(&str, &str, u64); 2] = [
    (
        "config.json",
        "9bd323e60afe2615c983a5d9fc3a2c0470df2a03edf90c0f861bd59509d07264",
        36_176,
    ),
    (
        "model.safetensors",
        "100067972767e538809d73f6c4b5c215ecbbedcd8b5d69e66ef10c1e699d946b",
        1_235_819_917,
    ),
];
const SIGNING_IDENTITY: &str = "Sunoto Local Code Signing";
const SIGNING_IDENTITY_MARKER: &str = "signing-identity";

/// Environment variable naming the bundle a detached installer works on.
pub(crate) const SETUP_BUNDLE_ENV: &str = "SUNOTO_SETUP_BUNDLE";

/// The installer must not run as the app's own executable: Launch Services
/// then counts it as "Sunoto is running" and ignores `open` for the real
/// app. When invoked from inside a bundle, copy this binary to a temporary
/// directory and exec the copy with the bundle path in the environment.
/// Never returns when it re-execs.
pub fn reexec_outside_bundle(command: &str, args: &[String]) -> Result<(), Box<dyn Error>> {
    use std::os::unix::process::CommandExt;
    let Some(bundle) = app_bundle_root() else {
        return Ok(());
    };
    if std::env::var_os(SETUP_BUNDLE_ENV).is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let dir = std::env::temp_dir().join(format!("sunoto-setup-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let copy = dir.join("sunoto-daemon");
    fs::copy(&exe, &copy)?;
    let error = Command::new(&copy)
        .arg(command)
        .args(args)
        .env(SETUP_BUNDLE_ENV, &bundle)
        .exec();
    Err(format!("could not re-exec the installer outside the bundle: {error}").into())
}

/// The prebuilt bundle this installer works on: the one it was detached
/// from, or the one it runs inside.
fn setup_bundle() -> Option<PathBuf> {
    std::env::var_os(SETUP_BUNDLE_ENV)
        .map(PathBuf::from)
        .or_else(app_bundle_root)
        .filter(|bundle| bundle_is_prebuilt(bundle))
}

/// `.../Sunoto.app` when this executable lives in an app bundle.
pub fn app_bundle_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    let bundle = contents.parent()?;
    (macos_dir.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|ext| ext == "app"))
    .then(|| bundle.to_path_buf())
}

/// True when the bundle ships its own runtime (a release build) rather
/// than pointing at a repository checkout.
pub fn bundle_is_prebuilt(bundle: &Path) -> bool {
    bundle
        .join(EMBEDDED_ROOT)
        .join("python/bin/python3")
        .is_file()
}

/// Point the daemon at the bundle's runtime. A marker file wins (developer
/// install from a checkout); otherwise the embedded root. A prebuilt bundle
/// also keeps the Hugging Face cache under Application Support so the
/// speech model lives next to the polish model and uninstall --purge
/// removes both.
pub fn apply_bundle_environment(bundle: &Path) {
    let resources = bundle.join("Contents/Resources");
    if std::env::var_os("SUNOTO_ROOT").is_none() {
        let root = fs::read_to_string(resources.join(ROOT_MARKER))
            .ok()
            .map(|marker| PathBuf::from(marker.trim()))
            .or_else(|| bundle_is_prebuilt(bundle).then(|| bundle.join(EMBEDDED_ROOT)));
        if let Some(root) = root {
            // SAFETY: called from main before any thread exists.
            unsafe { std::env::set_var("SUNOTO_ROOT", root) };
        }
    }
    if bundle_is_prebuilt(bundle)
        && std::env::var_os("HF_HOME").is_none()
        && let Ok(home) = std::env::var("HOME")
    {
        let hf_home = PathBuf::from(home).join("Library/Application Support/sunoto/hf");
        // SAFETY: as above.
        unsafe { std::env::set_var("HF_HOME", hf_home) };
    }
}

const USAGE: &str = "usage: sunoto-daemon setup [--dry-run] [--no-login-item] [--with-llm|--without-llm] [--timeout-secs N] [--update]

  --dry-run          assemble target/release/Sunoto.app only; install nothing
  --no-login-item    install and start the app without registering it at login
  --update           replace an installed release in place (used by the updater):
                     keep the Login Item, never prompt, and refuse to sign with
                     anything but the identity the installed app was signed with
  --with-llm         download the LLM polish model (about 2.7 GB) without asking
  --without-llm      skip the LLM polish model; dictation uses deterministic polish only
  --timeout-secs N   how long to wait for the daemon to report ready (default 600)
  --print-plist      print the bundle's Info.plist and exit (used by the release build)
";

/// Where the LLM polish model is fetched from. The file is the same one the
/// benchmarks ran against: its size and SHA-256 are pinned below.
const LLM_MODEL_URL: &str = "https://huggingface.co/bartowski/microsoft_Phi-4-mini-instruct-GGUF/resolve/main/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf";
const LLM_MODEL_SHA256: &str = "840ad85cff01e41701e2b2a3826016916f8e51242c8f25d62e59fa7eb93acbc5";
const LLM_MODEL_BYTES: u64 = 2_848_128_384;
const LLM_MODEL_RELATIVE: &str =
    "models/llm-polish-hf/phi-4-mini-q5/microsoft_Phi-4-mini-instruct-Q5_K_M.gguf";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LlmChoice {
    Ask,
    Download,
    Skip,
}

struct Args {
    dry_run: bool,
    login_item: bool,
    update: bool,
    llm: LlmChoice,
    timeout: Duration,
}

fn parse_args(args: &[String]) -> Result<Args, Box<dyn Error>> {
    let mut parsed = Args {
        dry_run: false,
        login_item: true,
        update: false,
        llm: LlmChoice::Ask,
        timeout: Duration::from_secs(600),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => parsed.dry_run = true,
            "--no-login-item" => parsed.login_item = false,
            // Stable since 0.1.11: older updaters call newer setups with it.
            "--update" => {
                parsed.update = true;
                parsed.login_item = false;
            }
            "--with-llm" => parsed.llm = LlmChoice::Download,
            "--without-llm" => parsed.llm = LlmChoice::Skip,
            "--print-plist" => {}
            "--timeout-secs" => {
                let value = iter.next().ok_or("--timeout-secs needs a value")?;
                parsed.timeout = Duration::from_secs(value.parse()?);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown setup option {other}\n{USAGE}").into()),
        }
    }
    Ok(parsed)
}

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    if !cfg!(target_os = "macos") {
        return Err("setup is macOS-only for now; on Linux use install.sh".into());
    }
    let args = parse_args(args)?;
    let root = settings::repo_root();
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let installed = home.join("Applications").join(format!("{APP_NAME}.app"));
    let log_path = home.join("Library/Logs/sunoto/daemon.log");
    // Prebuilt: a release bundle with its runtime inside. Nothing to
    // build; install the bundle as it is.
    let prebuilt = setup_bundle();
    let daemon = match &prebuilt {
        Some(bundle) => bundle.join("Contents/MacOS/sunoto-daemon"),
        None => std::env::current_exe()?.canonicalize()?,
    };
    let overlay = daemon.with_file_name("sunoto-overlay");
    let staging = match &prebuilt {
        Some(bundle) => bundle.clone(),
        None => root.join("target/release").join(format!("{APP_NAME}.app")),
    };

    section("preflight");
    if let Some(bundle) = &prebuilt {
        ok(&format!("prebuilt bundle: {}", bundle.display()));
    }
    if !overlay.is_file() {
        return Err(format!(
            "overlay binary missing at {}; build it with: swiftc -O services/macos/sunoto-overlay.swift -o {}",
            overlay.display(),
            overlay.display()
        )
        .into());
    }
    ok(&format!("daemon:  {}", daemon.display()));
    ok(&format!("overlay: {}", overlay.display()));
    let config_path = settings::config_path();
    let mut loaded = Settings::load(&config_path)?;
    if config_path.is_file() {
        let changes = retire_old_defaults(&mut loaded);
        if changes.is_empty() {
            ok(&format!(
                "config:  {} (left untouched)",
                config_path.display()
            ));
        }
        for (key, value, why) in changes {
            if !args.dry_run {
                set_config_value(&config_path, key, value)?;
            }
            ok(&format!(
                "config:  {} ({} {why})",
                config_path.display(),
                if args.dry_run {
                    "would change"
                } else {
                    "changed"
                }
            ));
        }
    } else if args.dry_run {
        note(&format!(
            "config:  {} would be created",
            config_path.display()
        ));
    } else {
        loaded.save_changes(&config_path)?;
        ok(&format!("config:  created {}", config_path.display()));
    }
    report_history(&loaded);
    let asr_python = preflight_asr_runtime(&root, &loaded)?;
    // An update leaves the polish model and its setting as the user had them.
    let loaded = if args.update {
        loaded
    } else {
        ensure_llm_model(loaded, &config_path, args.llm, args.dry_run)?
    };
    if let Some(python) = asr_python.filter(|_| loaded.backend.starts_with("parakeet_mlx")) {
        ensure_asr_model(&python, &loaded, args.update, args.dry_run)?;
    }

    section("bundle");
    if prebuilt.is_none() {
        assemble_bundle(&staging, &daemon, &overlay, &root)?;
        ok(&format!("assembled {}", staging.display()));
    } else {
        ok("release bundle used as is");
    }
    if args.dry_run {
        let identity = find_signing_identity().ok().flatten();
        codesign(&staging, identity.as_ref())?;
        if identity.is_some() {
            ok("signed with the local stable identity");
        } else {
            warn("local signing identity is not installed; dry-run bundle is ad-hoc signed");
        }
        note("dry run: nothing installed, nothing started");
        return Ok(());
    }

    section("install");
    if args.update {
        // A new identity would reset this app's permissions, for the old
        // copy as well, and the updater could not roll back to a working app.
        require_installed_identity(&home)?;
    } else {
        // Both talk to System Events, which an update started by the app
        // itself may not be allowed to do; the Login Item already exists.
        remove_legacy_login_item(&home);
    }
    stop_running_daemons();
    let same_place = staging
        .canonicalize()
        .ok()
        .zip(installed.canonicalize().ok())
        .is_some_and(|(a, b)| a == b);
    if same_place {
        ok(&format!("already at {}", installed.display()));
    } else {
        if installed.exists() {
            fs::remove_dir_all(&installed)?;
        }
        fs::create_dir_all(installed.parent().expect("~/Applications has a parent"))?;
        copy_dir(&staging, &installed)?;
        ok(&format!("installed {}", installed.display()));
    }
    // A downloaded bundle carries the quarantine flag; without a Developer
    // ID signature macOS would refuse to open it. The user chose to install
    // it, so lift the flag on the installed copy.
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&installed)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let identity = match ensure_signing_identity() {
        Ok(identity) => Some(identity),
        Err(error) => {
            warn(&format!(
                "could not create the stable signing identity ({error}); using an ad-hoc signature, so macOS may require permissions again after an upgrade"
            ));
            None
        }
    };
    codesign(&installed, identity.as_ref())?;
    let marker = home
        .join("Library/Application Support/sunoto")
        .join(SIGNING_IDENTITY_MARKER);
    let identity_changed = identity.as_ref().is_none_or(|identity| {
        fs::read_to_string(&marker)
            .map(|value| value.trim() != identity.fingerprint)
            .unwrap_or(true)
    });
    if identity_changed {
        // The first stable-signature install must replace records tied to the
        // old ad-hoc signature. Later rebuilds keep the same TCC identity.
        reset_own_permission_records();
    }
    if let Some(identity) = &identity {
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&marker, format!("{}\n", identity.fingerprint))?;
        ok(&format!(
            "signed with the stable local identity {}",
            identity.fingerprint
        ));
    } else {
        let _ = fs::remove_file(&marker);
    }
    if args.login_item {
        register_login_item(&installed)?;
        ok(&format!("{APP_NAME} registered in Login Items"));
    }
    // On updates too: that is how installs from before the command get it.
    install_command(&home);
    fs::create_dir_all(log_path.parent().expect("log path has a parent"))?;
    // An update keeps the log, so the old version's lines about it stay.
    if !args.update {
        let _ = fs::write(&log_path, b"");
    }
    launch_app(&installed)?;
    ok(&format!("started {APP_NAME}; log: {}", log_path.display()));

    section("permissions");
    watch_until_ready(args.timeout, &installed)
}

/// Change one key in the config file and leave the rest as written, so
/// settings the user never chose keep following the defaults.
fn set_config_value(
    path: &Path,
    key: &str,
    value: serde_json::Value,
) -> Result<(), Box<dyn Error>> {
    let mut config: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    config
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?
        .insert(key.to_string(), value);
    fs::write(path, serde_json::to_string_pretty(&config)? + "\n")?;
    Ok(())
}

/// `capture_idle_stop_secs` as installs from 0.1.0 to 0.1.6 saved it.
const OLD_CAPTURE_IDLE_STOP_SECS: u64 = 120;

/// Setup used to write every setting into the config, so an install kept
/// the defaults of the version that created it. Move the old defaults that
/// later versions changed on purpose, and return each changed key, its new
/// value, and why. A value the user picked differs from the old default and
/// is kept.
fn retire_old_defaults(settings: &mut Settings) -> Vec<(&'static str, serde_json::Value, String)> {
    let defaults = Settings::default();
    let mut changes = Vec::new();
    // Ctrl+F1 before 0.1.7.
    if is_old_default_shortcut(&settings.shortcut) {
        settings.shortcut = defaults.shortcut;
        changes.push((
            "shortcut",
            settings.shortcut.clone().into(),
            format!(
                "shortcut from Control-F1 to {}, the default since 0.1.7; set \"shortcut\" there to change it",
                settings::shortcut_label(&settings.shortcut)
            ),
        ));
    }
    // 120 kept the microphone, and its menu-bar indicator, on for two
    // minutes after every dictation; 0 turns it off right away.
    if settings.capture_idle_stop_secs == OLD_CAPTURE_IDLE_STOP_SECS {
        settings.capture_idle_stop_secs = defaults.capture_idle_stop_secs;
        changes.push((
            "capture_idle_stop_secs",
            settings.capture_idle_stop_secs.into(),
            "the microphone to turn off when a dictation ends instead of 2 minutes later, the default since 0.1.7; set \"capture_idle_stop_secs\" there to keep it on longer".to_string(),
        ));
    }
    changes
}

/// Ctrl+F1 in any spelling: the macOS default before 0.1.7.
fn is_old_default_shortcut(shortcut: &str) -> bool {
    let parts: Vec<String> = shortcut
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();
    matches!(parts.as_slice(), [modifier, key] if (modifier == "ctrl" || modifier == "control") && key == "f1")
}

/// Returns the ASR Python interpreter when the backend needs one.
fn preflight_asr_runtime(
    root: &Path,
    settings: &Settings,
) -> Result<Option<String>, Box<dyn Error>> {
    if !settings.backend.starts_with("parakeet_mlx") {
        note(&format!(
            "backend {}: Parakeet runtime preflight skipped",
            settings.backend
        ));
        return Ok(None);
    }
    let (python, _) = settings.sidecar_command()?;
    let python_path = Path::new(&python);
    if !python_path.is_file() || python == "python3" {
        return Err(format!(
            "ASR Python runtime missing under {}; run: brew install python@3.12 && bash services/asr/setup_macos_runtime.sh && .venv-nemotron-mac/bin/python -m pip install -U parakeet-mlx",
            root.display()
        )
        .into());
    }
    let status = Command::new(&python)
        .args(["-c", "import mlx, parakeet_mlx"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!(
            "{python} cannot import mlx and parakeet_mlx; repair .venv-nemotron-mac"
        )
        .into());
    }
    ok("ASR runtime imports: mlx + parakeet_mlx");
    Ok(Some(python))
}

/// Fetch the speech model now, with progress, instead of letting the first
/// daemon start do it silently behind a "loading speech model..." pill.
/// Prefers Sunoto's 1.24 GB bf16 copy; an install that already has the
/// float32 original in the Hugging Face cache keeps using it. Once the model
/// is in place, the copy of a model the app no longer uses is deleted. An
/// update that still has its old model leaves the fetch to the app.
fn ensure_asr_model(
    python: &str,
    settings: &Settings,
    update: bool,
    dry_run: bool,
) -> Result<(), Box<dyn Error>> {
    section("speech model");
    if let Some(model) = settings
        .asr_model
        .as_deref()
        .filter(|model| !model.is_empty())
    {
        note(&format!(
            "asr_model is set to {model}; the app fetches it on first start"
        ));
        return Ok(());
    }
    let local = settings::local_parakeet_model_dir();
    if let Some(dir) = local
        .as_deref()
        .filter(|dir| settings::model_dir_complete(dir))
    {
        ok(&format!("speech model ready: {}", dir.display()));
        remove_retired_models(dry_run);
        return Ok(());
    }
    if update && let Some(old) = settings::stopgap_parakeet_model() {
        // The updater gives the new version minutes to start, which 1.24 GB
        // may not fit in. The app runs the old model meanwhile.
        note(&format!(
            "the app keeps {old} until it has downloaded the new speech model (about 1.24 GB) in the background"
        ));
        return Ok(());
    }
    let script = format!(
        "from huggingface_hub import snapshot_download\nimport sys\np = snapshot_download('{ASR_MODEL_REPO}', local_files_only=True) if '--check' in sys.argv else snapshot_download('{ASR_MODEL_REPO}')\nprint(p)"
    );
    let cached = Command::new(python)
        .args(["-c", &script, "--check"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if cached {
        ok(&format!("{ASR_MODEL_REPO} already cached"));
        remove_retired_models(dry_run);
        return Ok(());
    }
    if dry_run {
        note("would download the speech model (about 1.24 GB)");
        return Ok(());
    }
    if let Some(dir) = &local {
        note("downloading the speech model (about 1.24 GB, one time)...");
        match fetch_speech_model(dir, Progress::Shown) {
            Ok(()) => {
                ok(&format!("speech model ready: {}", dir.display()));
                remove_retired_models(dry_run);
                return Ok(());
            }
            Err(error) => warn(&format!(
                "could not fetch Sunoto's copy of the speech model ({error}); using the Hugging Face original instead"
            )),
        }
    }
    note(&format!(
        "downloading {ASR_MODEL_REPO} (about 2.5 GB, one time)..."
    ));
    let status = Command::new(python).args(["-c", &script]).status()?;
    if !status.success() {
        return Err("speech model download failed; rerun setup to resume".into());
    }
    ok("speech model ready");
    remove_retired_models(dry_run);
    Ok(())
}

/// Fetch Sunoto's copy of the speech model into `dir`. A file already there
/// was checked before it was moved in, so it is kept. `SUNOTO_MODEL_RELEASE`
/// replaces the release URL, for testing with a `file://` folder.
pub(crate) fn fetch_speech_model(dir: &Path, progress: Progress) -> Result<(), Box<dyn Error>> {
    let release =
        std::env::var("SUNOTO_MODEL_RELEASE").unwrap_or_else(|_| ASR_MODEL_RELEASE.to_string());
    ASR_MODEL_FILES
        .iter()
        .try_for_each(|(name, sha256, bytes)| {
            let destination = dir.join(name);
            if destination.is_file() {
                return Ok(());
            }
            download_verified(
                &format!("{release}/{name}"),
                &destination,
                sha256,
                *bytes,
                progress,
            )
        })
}

/// Delete the models Sunoto downloaded and no longer uses (v3: its bf16
/// copy and, on release installs, the original in Sunoto's own Hugging Face
/// cache). Only folders under Sunoto's Application Support folder; the
/// shared ~/.cache/huggingface is left alone.
fn remove_retired_models(dry_run: bool) {
    for dir in settings::retired_model_dirs() {
        if dry_run {
            note(&format!(
                "would delete the old speech model at {}",
                dir.display()
            ));
        } else if let Err(error) = std::fs::remove_dir_all(&dir) {
            warn(&format!(
                "could not delete the old speech model at {}: {error}",
                dir.display()
            ));
        } else {
            ok(&format!(
                "deleted the old speech model at {}",
                dir.display()
            ));
        }
    }
}

/// Make sure the polish model exists, downloading it on request. Returns
/// the settings as they are on disk afterwards.
fn ensure_llm_model(
    mut settings: Settings,
    config_path: &Path,
    choice: LlmChoice,
    dry_run: bool,
) -> Result<Settings, Box<dyn Error>> {
    section("polish model");
    if !settings.llm_polish_enabled && choice != LlmChoice::Download {
        note(
            "LLM polish is off (the default); `sunoto setup --with-llm` downloads the 2.7 GB model and turns it on",
        );
        return Ok(settings);
    }
    if let Some(existing) = settings.llm_polish_model_file().filter(|p| p.is_file()) {
        ok(&format!("model present: {}", existing.display()));
        // Only reachable with --with-llm: the user asked for it on.
        if !settings.llm_polish_enabled {
            settings.llm_polish_enabled = true;
            if !dry_run {
                settings.save_changes(config_path)?;
            }
            ok("LLM polish switched on in the config");
        }
        return Ok(settings);
    }
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let destination = home
        .join("Library/Application Support/sunoto")
        .join(LLM_MODEL_RELATIVE);
    let decision = match choice {
        LlmChoice::Download => true,
        LlmChoice::Skip => false,
        LlmChoice::Ask => {
            if dry_run {
                note("would ask whether to download the 2.7 GB polish model (dry run: skipping)");
                return Ok(settings);
            }
            ask_yes_no(&format!(
                "Download the LLM polish model (2.7 GB, one time) to {}? It merges mid-sentence self-corrections; without it dictation still works with deterministic cleanup. [y/N] ",
                destination.display()
            ))
        }
    };
    if !decision {
        if settings.llm_polish_enabled {
            settings.llm_polish_enabled = false;
            if !dry_run {
                settings.save_changes(config_path)?;
            }
            note("LLM polish switched off in the config; run `setup --with-llm` later to add it");
        }
        return Ok(settings);
    }
    if dry_run {
        note(&format!(
            "would download {LLM_MODEL_URL} to {}",
            destination.display()
        ));
        return Ok(settings);
    }
    download_verified(
        LLM_MODEL_URL,
        &destination,
        LLM_MODEL_SHA256,
        LLM_MODEL_BYTES,
        Progress::Shown,
    )?;
    settings.llm_polish_model_path = Some(destination.to_string_lossy().into_owned());
    settings.llm_polish_enabled = true;
    settings.save_changes(config_path)?;
    ok(&format!("model ready: {}", destination.display()));
    Ok(settings)
}

fn ask_yes_no(prompt: &str) -> bool {
    use std::io::{BufRead, BufReader, IsTerminal};
    // Under `curl ... | bash` stdin is the script itself, so read the
    // answer from the controlling terminal when there is one. With no
    // terminal at all (CI, a service), skip rather than hang.
    let mut input: Box<dyn BufRead> = if std::io::stdin().is_terminal() {
        Box::new(BufReader::new(std::io::stdin()))
    } else if let Ok(tty) = fs::File::open("/dev/tty") {
        Box::new(BufReader::new(tty))
    } else {
        note("not a terminal; skipping the download (pass --with-llm to force it)");
        return false;
    };
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// How a download shows itself: in setup, with notes and curl's progress
/// bar; in the running app, quietly, and stopped when the app quits (the
/// next start resumes the partial file).
#[derive(Clone, Copy)]
pub(crate) enum Progress<'a> {
    Shown,
    Quiet { stop: &'a AtomicBool },
}

/// `curl` into a `.part` file, then verify the SHA-256 and size before
/// moving it into place. A verified file is never replaced.
fn download_verified(
    url: &str,
    destination: &Path,
    sha256: &str,
    expected_bytes: u64,
    progress: Progress,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut partial = destination.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let mut curl = Command::new("curl");
    curl.args(["-L", "--fail", "-C", "-", "-o"])
        .arg(&partial)
        .arg(url);
    let status = match progress {
        Progress::Shown => {
            note(&format!("downloading {url}"));
            curl.arg("--progress-bar").status()?
        }
        Progress::Quiet { stop } => {
            let child = curl
                .args(["--silent", "--show-error"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()?;
            wait_unless_stopped(child, stop)?
        }
    };
    if !status.success() {
        return Err(
            format!("download of {url} failed ({status}); it resumes on the next try").into(),
        );
    }
    let size = fs::metadata(&partial)?.len();
    if size != expected_bytes {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "downloaded {size} bytes, expected {expected_bytes}; the file was discarded"
        )
        .into());
    }
    if let Progress::Shown = progress {
        note("verifying checksum...");
    }
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&partial)
        .output()?;
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if digest != sha256 {
        let _ = fs::remove_file(&partial);
        return Err(
            format!("checksum mismatch ({digest} != {sha256}); the file was discarded").into(),
        );
    }
    fs::rename(&partial, destination)?;
    Ok(())
}

/// Wait for `child`, killing it first if `stop` is set.
fn wait_unless_stopped(mut child: Child, stop: &AtomicBool) -> Result<ExitStatus, Box<dyn Error>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("stopped".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `sunoto-daemon restart`: quit the installed app and open it again.
pub fn restart() -> Result<(), Box<dyn Error>> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let installed = home.join("Applications").join(format!("{APP_NAME}.app"));
    if !installed.exists() {
        return Err(format!(
            "{} is not installed; run `sunoto-daemon setup` first",
            installed.display()
        )
        .into());
    }
    stop_running_daemons();
    launch_app(&installed)?;
    ok(&format!(
        "restarted {APP_NAME}; `sunoto-daemon status` shows its health"
    ));
    Ok(())
}

/// `sunoto-daemon log`: follow the daemon log.
pub fn follow_log() -> Result<(), Box<dyn Error>> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let log_path = home.join("Library/Logs/sunoto/daemon.log");
    let status = Command::new("tail")
        .args(["-n", "80", "-f"])
        .arg(&log_path)
        .status()?;
    if !status.success() {
        return Err(format!("cannot follow {}", log_path.display()).into());
    }
    Ok(())
}

/// `sunoto-daemon uninstall`: stop the app, drop the Login Item, remove the
/// bundle and the `sunoto` command. Config, logs, and downloaded models stay
/// unless `--purge`.
const UNINSTALL_USAGE: &str = "usage: sunoto-daemon uninstall [--purge]

  --purge   also remove settings, logs, and downloaded models
";

/// `Some(purge)` to go ahead, `None` for help. This command deletes the
/// app, so anything it does not recognise, a typo included, stops it.
fn parse_uninstall_args(args: &[String]) -> Result<Option<bool>, String> {
    match args {
        [] => Ok(Some(false)),
        [flag] if flag == "--purge" => Ok(Some(true)),
        [flag] if flag == "-h" || flag == "--help" => Ok(None),
        _ => Err(format!("unexpected arguments {args:?}\n{UNINSTALL_USAGE}")),
    }
}

pub fn uninstall(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(purge) = parse_uninstall_args(args)? else {
        print!("{UNINSTALL_USAGE}");
        return Ok(());
    };
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let installed = home.join("Applications").join(format!("{APP_NAME}.app"));
    section("uninstall");
    stop_running_daemons();
    remove_legacy_login_item(&home);
    let script = format!(
        r#"tell application "System Events"
    try
        delete every login item whose name is "{APP_NAME}"
    end try
end tell"#
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if installed.exists() {
        fs::remove_dir_all(&installed)?;
        ok(&format!("removed {}", installed.display()));
    } else {
        note(&format!("{} was not installed", installed.display()));
    }
    for command in remove_own_commands(&command_dirs(&home)) {
        ok(&format!("removed {}", command.display()));
    }
    ok("Login Item removed");
    if purge {
        for path in [
            home.join("Library/Application Support/sunoto"),
            home.join("Library/Logs/sunoto"),
        ] {
            if path.exists() {
                fs::remove_dir_all(&path)?;
                ok(&format!("removed {}", path.display()));
            }
        }
    } else {
        note("config, logs, and downloaded models kept (use --purge to remove them)");
    }
    note("Privacy & Security still lists Sunoto; remove the entries there if you like.");
    Ok(())
}

fn assemble_bundle(
    staging: &Path,
    daemon: &Path,
    overlay: &Path,
    root: &Path,
) -> Result<(), Box<dyn Error>> {
    if staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    let macos_dir = staging.join("Contents/MacOS");
    let resources = staging.join("Contents/Resources");
    fs::create_dir_all(&macos_dir)?;
    fs::create_dir_all(&resources)?;
    fs::copy(daemon, macos_dir.join("sunoto-daemon"))?;
    fs::copy(overlay, macos_dir.join("sunoto-overlay"))?;
    fs::write(resources.join(ROOT_MARKER), format!("{}\n", root.display()))?;
    fs::write(staging.join("Contents/Info.plist"), info_plist())?;
    let lint = Command::new("plutil")
        .args(["-lint", "Contents/Info.plist"])
        .current_dir(staging)
        .stdout(Stdio::null())
        .status()?;
    if !lint.success() {
        return Err("generated Info.plist failed plutil -lint".into());
    }
    Ok(())
}

pub fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>sunoto-daemon</string>
    <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
    <key>CFBundleName</key><string>{APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>{APP_NAME}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>LSUIElement</key><true/>
    <key>NSAppSleepDisabled</key><true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sunoto records your voice only while you dictate with its shortcut.</string>
    <key>NSContactsUsageDescription</key>
    <string>If you turn it on, Sunoto spells the names in your contacts right when you dictate. They never leave this Mac.</string>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

#[derive(Debug)]
struct SigningIdentity {
    fingerprint: String,
    keychain: PathBuf,
}

fn login_keychain() -> Result<PathBuf, Box<dyn Error>> {
    let output = Command::new("security")
        .args(["default-keychain", "-d", "user"])
        .output()?;
    if !output.status.success() {
        return Err("security could not locate the login keychain".into());
    }
    let path = String::from_utf8(output.stdout)?;
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("security returned an empty login keychain path".into());
    }
    Ok(PathBuf::from(path))
}

fn parse_signing_identity(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        if !line.contains(&format!("\"{SIGNING_IDENTITY}\"")) {
            return None;
        }
        let fingerprint = line.split_whitespace().nth(1)?;
        (fingerprint.len() == 40 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| fingerprint.to_string())
    })
}

fn find_signing_identity() -> Result<Option<SigningIdentity>, Box<dyn Error>> {
    let keychain = login_keychain()?;
    let output = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .arg(&keychain)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    Ok(
        parse_signing_identity(&stdout).map(|fingerprint| SigningIdentity {
            fingerprint,
            keychain,
        }),
    )
}

fn ensure_signing_identity() -> Result<SigningIdentity, Box<dyn Error>> {
    if let Some(identity) = find_signing_identity()? {
        return Ok(identity);
    }
    let keychain = login_keychain()?;
    let work = std::env::temp_dir().join(format!("sunoto-signing-{}", std::process::id()));
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir(&work)?;
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700))?;
    let key = work.join("identity.key");
    let certificate = work.join("identity.pem");
    let archive = work.join("identity.p12");
    let password = format!("sunoto-local-{}", std::process::id());
    let creation = (|| -> Result<(), Box<dyn Error>> {
        let status = Command::new("/usr/bin/openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650",
            ])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&certificate)
            .args([
                "-subj",
                &format!("/CN={SIGNING_IDENTITY}/O=Earendil Works"),
                "-addext",
                "keyUsage=critical,digitalSignature",
                "-addext",
                "extendedKeyUsage=critical,codeSigning",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err("openssl could not create the local code-signing certificate".into());
        }
        let status = Command::new("/usr/bin/openssl")
            .args(["pkcs12", "-export", "-inkey"])
            .arg(&key)
            .arg("-in")
            .arg(&certificate)
            .arg("-out")
            .arg(&archive)
            .args([
                "-name",
                SIGNING_IDENTITY,
                "-passout",
                &format!("pass:{password}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err("openssl could not package the local signing identity".into());
        }
        let status = Command::new("security")
            .arg("import")
            .arg(&archive)
            .arg("-k")
            .arg(&keychain)
            .args(["-P", &password, "-A"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err("security could not import the local signing identity".into());
        }
        let status = Command::new("security")
            .args([
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-p",
                "codeSign",
                "-k",
            ])
            .arg(&keychain)
            .arg(&certificate)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err("security could not trust the local code-signing certificate".into());
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    creation?;
    find_signing_identity()?.ok_or_else(|| {
        "the local certificate was created but is not a valid code-signing identity".into()
    })
}

fn codesign(bundle: &Path, identity: Option<&SigningIdentity>) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("codesign");
    command.args(["--force", "--deep", "--sign"]);
    if let Some(identity) = identity {
        command
            .arg(&identity.fingerprint)
            .arg("--keychain")
            .arg(&identity.keychain);
    } else {
        command.arg("-");
    }
    let status = command
        .args(["--identifier", BUNDLE_ID])
        .arg(bundle)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err("codesign failed".into());
    }
    Ok(())
}

// Anchored so that `sunoto-daemon setup` (which may itself run from a
// bundle) never matches: a Launch Services start has no arguments.
// The overlay is included: Launch Services counts it as the app, and an
// `open` issued while it is still exiting is ignored.
const DAEMON_PATTERNS: [&str; 4] = [
    "sunoto-daemon run$",
    "Sunoto.app/Contents/MacOS/sunoto-daemon$",
    "Sunoto.app/Contents/MacOS/sunoto-overlay$",
    "Sunoto Login.app/Contents/MacOS/sunoto-login",
];

/// The stable signing identity exists and is the one the installed app was
/// signed with.
pub(crate) fn require_installed_identity(home: &Path) -> Result<(), Box<dyn Error>> {
    let identity = find_signing_identity()?
        .ok_or("the local signing identity is missing; run the installer once by hand")?;
    let marker = home
        .join("Library/Application Support/sunoto")
        .join(SIGNING_IDENTITY_MARKER);
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    if recorded.trim() != identity.fingerprint {
        return Err(
            "the installed app was signed with another identity; run the installer once by hand"
                .into(),
        );
    }
    Ok(())
}

fn daemons_running() -> bool {
    DAEMON_PATTERNS.iter().any(|pattern| {
        Command::new("pgrep")
            .args(["-f", pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Terminate every daemon and wait until they are really gone. Launch
/// Services refuses to relaunch an app it still considers quitting, so a
/// fixed sleep is not enough.
pub(crate) fn stop_running_daemons() {
    for pattern in DAEMON_PATTERNS {
        let _ = Command::new("pkill")
            .args(["-f", pattern])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemons_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Give Launch Services a moment to notice the exit.
    std::thread::sleep(Duration::from_millis(500));
}

/// `open` the bundle and confirm the new daemon answers on the control
/// socket (it binds within a second of starting, before the models load).
/// Process names are not trusted here: an installer running from a bundle
/// looked like the app once and left the user with nothing running.
pub(crate) fn launch_app(app: &Path) -> Result<(), Box<dyn Error>> {
    // Callers stop every daemon and overlay first, so `-n` (a new instance
    // regardless of what Launch Services believes) is always right here.
    let status = Command::new("open").arg("-n").arg(app).status()?;
    if !status.success() {
        return Err(format!("open {} failed ({status})", app.display()).into());
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if query_status().is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !daemons_running() {
        return Err(format!(
            "{} did not start; check ~/Library/Logs/sunoto/daemon.log",
            app.display()
        )
        .into());
    }
    Ok(())
}

/// Modification times of the two permission databases. Their contents are
/// off limits, but their metadata is readable, and every toggle in Privacy
/// & Security rewrites one of them. A running process cannot see its own
/// new grant, so this is how the daemon learns that the user just flipped
/// a switch and that a relaunch will now succeed.
pub fn permission_db_stamp() -> Option<Vec<u128>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let paths = [
        PathBuf::from("/Library/Application Support/com.apple.TCC/TCC.db"),
        PathBuf::from(home).join("Library/Application Support/com.apple.TCC/TCC.db"),
    ];
    let stamps: Vec<u128> = paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .filter_map(|meta| meta.modified().ok())
        .filter_map(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_nanos())
        })
        .collect();
    (!stamps.is_empty()).then_some(stamps)
}

/// Arrange for this app to be reopened after it exits. Returns false when
/// not running from a bundle (a terminal-launched daemon stays put). The
/// helper shell waits for the daemon to be gone first: Launch Services
/// ignores `open` for an app it still considers quitting.
pub fn relaunch_self_if_bundled() -> bool {
    let Some(bundle) = app_bundle_root() else {
        return false;
    };
    let script = format!(
        "while pgrep -f '[S]unoto.app/Contents/MacOS/sunoto-(daemon|overlay)$' >/dev/null; do sleep 0.2; done; open -n '{}'",
        bundle.display()
    );
    Command::new("sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Drop this bundle's own permission records. macOS keys grants to the
/// code signature, so a record left by an earlier build of the same bundle
/// id matches the identifier, fails the signature check, and denies
/// silently while the toggle shows "on". Scoped to our identifier only.
/// Must run before the app requests access, never after (see `run`).
fn reset_own_permission_records() {
    for service in ["Accessibility", "ListenEvent"] {
        let _ = Command::new("tccutil")
            .args(["reset", service, BUNDLE_ID])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn remove_legacy_login_item(home: &Path) {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LEGACY_LAUNCHD_LABEL}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = fs::remove_file(
        home.join("Library/LaunchAgents")
            .join(format!("{LEGACY_LAUNCHD_LABEL}.plist")),
    );
    let legacy_app = home
        .join("Applications")
        .join(format!("{LEGACY_LOGIN_ITEM}.app"));
    if legacy_app.exists() {
        let _ = fs::remove_dir_all(&legacy_app);
        note(&format!("removed {}", legacy_app.display()));
    }
    let script = format!(
        r#"tell application "System Events"
    try
        delete every login item whose name is "{LEGACY_LOGIN_ITEM}"
    end try
end tell"#
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn register_login_item(app: &Path) -> Result<(), Box<dyn Error>> {
    let script = format!(
        r#"on run argv
    set appPath to item 1 of argv
    tell application "System Events"
        try
            delete every login item whose name is "{APP_NAME}"
        end try
        make login item at end with properties {{name:"{APP_NAME}", path:appPath, hidden:true}}
    end tell
end run"#
    );
    let status = Command::new("osascript")
        .args(["-e", &script])
        .arg(app)
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err("could not register the Login Item (System Events refused)".into());
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), Box<dyn Error>> {
    // `cp -R` keeps the executable bits and the code signature intact; a
    // manual walk would need to reproduce both.
    let status = Command::new("cp").arg("-R").arg(from).arg(to).status()?;
    if !status.success() {
        return Err(format!("cannot copy {} to {}", from.display(), to.display()).into());
    }
    Ok(())
}

/// The `sunoto` command: a script that runs the installed app's daemon, so
/// `sunoto status` works in a new terminal. A script, not a symlink, so the
/// daemon still runs from the bundle path that `app_bundle_root` and the
/// updater read. It finds the app through `$HOME`, so updates keep it working.
const COMMAND_NAME: &str = "sunoto";
/// Marks the `sunoto` setup wrote: setup replaces and uninstall removes only
/// a file carrying it. Never change it; other versions look for it.
const COMMAND_MARKER: &str = "# sunoto command, installed by Sunoto setup";

fn command_script() -> String {
    format!(
        r#"#!/bin/sh
{COMMAND_MARKER}; `sunoto uninstall` removes it.
app="$HOME/Applications/{APP_NAME}.app/Contents/MacOS/sunoto-daemon"
if [ ! -x "$app" ]; then
    echo "sunoto: {APP_NAME} is not installed in ~/Applications" >&2
    exit 1
fi
# With no arguments the daemon would start as the app: show the help instead.
[ "$#" -gt 0 ] || set -- --help
exec "$app" "$@"
"#
    )
}

/// Where setup may put the command, best first. Homebrew's folder and
/// /usr/local/bin are on a terminal's PATH and need no sudo when the user
/// owns them; ~/.local/bin, the last, is the fallback and is created if
/// missing.
fn command_dirs(home: &Path) -> [PathBuf; 3] {
    [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Existing {
    Absent,
    Ours,
    Foreign,
}

/// What `dir/sunoto` is. A symlink or anything without the marker belongs
/// to someone else, as does a file that cannot be read. Nothing that cannot
/// even be looked up (a PATH entry that is a file or a folder closed to
/// this user) can be run or replaced, so it counts as absent.
fn existing_command(dir: &Path) -> Existing {
    let path = dir.join(COMMAND_NAME);
    match fs::symlink_metadata(&path) {
        Err(_) => Existing::Absent,
        Ok(meta)
            if meta.is_file()
                && fs::read(&path).is_ok_and(|bytes| {
                    String::from_utf8_lossy(&bytes).contains(COMMAND_MARKER)
                }) =>
        {
            Existing::Ours
        }
        _ => Existing::Foreign,
    }
}

unsafe extern "C" {
    fn access(path: *const c_char, mode: c_int) -> c_int;
}

fn writable_dir(dir: &Path) -> bool {
    const W_OK: c_int = 2;
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the call.
    dir.is_dir() && unsafe { access(path.as_ptr(), W_OK) } == 0
}

#[derive(Debug, PartialEq, Eq)]
enum CommandPlan {
    /// Write the command in this folder, or refresh it there.
    Install(PathBuf),
    /// Someone else's `sunoto` is here, first on PATH or in the only place
    /// left: leave it, and install nothing.
    Taken(PathBuf),
}

/// Where the command goes, given the PATH folders in order and `dirs`
/// (`command_dirs`). The first `sunoto` on PATH decides when there is one:
/// ours is refreshed in place, and beside anyone else's ours would either
/// never run or hide theirs. Otherwise the first of `dirs` that is on PATH
/// and writable; otherwise wherever ours already is; otherwise the fallback.
fn plan_command(
    path: &[PathBuf],
    dirs: &[PathBuf],
    existing: impl Fn(&Path) -> Existing,
    writable: impl Fn(&Path) -> bool,
) -> CommandPlan {
    for dir in path {
        match existing(dir) {
            Existing::Ours => return CommandPlan::Install(dir.clone()),
            Existing::Foreign => return CommandPlan::Taken(dir.clone()),
            Existing::Absent => {}
        }
    }
    if let Some(dir) = dirs.iter().find(|dir| path.contains(dir) && writable(dir)) {
        return CommandPlan::Install(dir.clone());
    }
    if let Some(dir) = dirs.iter().find(|dir| existing(dir) == Existing::Ours) {
        return CommandPlan::Install(dir.clone());
    }
    let fallback = dirs.last().expect("at least one folder");
    match existing(fallback) {
        Existing::Foreign => CommandPlan::Taken(fallback.clone()),
        _ => CommandPlan::Install(fallback.clone()),
    }
}

/// The PATH a new terminal gets, from the user's shell. Asked only with no
/// terminal attached, as in an update the app started: launchd's short PATH
/// lacks Homebrew. Interactive as well as login, because PATH is often set
/// in ~/.zshrc; with a terminal, an interactive shell could take it over.
fn shell_path() -> Vec<PathBuf> {
    const MARK: &str = "__SUNOTO_PATH__";
    if fs::File::open("/dev/tty").is_ok() {
        return Vec::new();
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".to_string());
    let Ok(mut child) = Command::new(shell)
        .args(["-i", "-l", "-c"])
        .arg(format!("printf '\\n{MARK}%s\\n' \"$PATH\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        let _ = child.wait();
        return Vec::new();
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    output
        .lines()
        .find_map(|line| line.strip_prefix(MARK))
        .map(|value| std::env::split_paths(value).collect())
        .unwrap_or_default()
}

/// Install or refresh the `sunoto` command. Never fails setup: the app
/// works without it, and a failed update would be rolled back.
fn install_command(home: &Path) {
    let dirs = command_dirs(home);
    let mut path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    path.retain(|dir| dir.is_absolute());
    let mut plan = plan_command(&path, &dirs, existing_command, writable_dir);
    if matches!(&plan, CommandPlan::Install(dir) if !path.contains(dir)) {
        path.extend(shell_path().into_iter().filter(|dir| dir.is_absolute()));
        plan = plan_command(&path, &dirs, existing_command, writable_dir);
    }
    let dir = match plan {
        CommandPlan::Install(dir) => dir,
        CommandPlan::Taken(dir) => {
            warn(&format!(
                "{} was not installed by Sunoto, so it was left alone and no sunoto command was added; Sunoto's commands run as {}/Applications/{APP_NAME}.app/Contents/MacOS/sunoto-daemon",
                dir.join(COMMAND_NAME).display(),
                home.display()
            ));
            return;
        }
    };
    if let Err(error) = write_command(&dir) {
        warn(&format!(
            "could not install the sunoto command in {}: {error}",
            dir.display()
        ));
        return;
    }
    remove_own_commands(dirs.iter().filter(|other| **other != dir));
    ok(&format!(
        "command: sunoto ({})",
        dir.join(COMMAND_NAME).display()
    ));
    if !path.contains(&dir) {
        let shell = std::env::var("SHELL").unwrap_or_default();
        let profile = match Path::new(&shell).file_name().and_then(|name| name.to_str()) {
            Some("bash") => "~/.bash_profile",
            _ => "~/.zshrc",
        };
        let entry = dir.strip_prefix(home).map_or_else(
            |_| dir.display().to_string(),
            |rest| format!("$HOME/{}", rest.display()),
        );
        warn(&format!(
            "{} is not on your PATH. To use `sunoto`, run this once, then open a new terminal: echo 'export PATH=\"{entry}:$PATH\"' >> {profile}",
            dir.display()
        ));
    }
}

/// Write the command into `dir` (created if missing) in one rename, so a
/// terminal never runs half a script.
fn write_command(dir: &Path) -> std::io::Result<()> {
    let target = dir.join(COMMAND_NAME);
    let script = command_script();
    if fs::read_to_string(&target).is_ok_and(|current| current == script) {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let partial = dir.join(format!(".{COMMAND_NAME}.{}", std::process::id()));
    fs::write(&partial, &script)?;
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))
        .and_then(|()| fs::rename(&partial, &target))
        .inspect_err(|_| {
            let _ = fs::remove_file(&partial);
        })
}

/// Delete the `sunoto` setup wrote in each of `dirs`, never anyone else's.
/// Returns what was removed.
fn remove_own_commands<'a>(dirs: impl IntoIterator<Item = &'a PathBuf>) -> Vec<PathBuf> {
    dirs.into_iter()
        .filter(|dir| existing_command(dir) == Existing::Ours)
        .map(|dir| dir.join(COMMAND_NAME))
        .filter(|command| fs::remove_file(command).is_ok())
        .collect()
}

/// Ask the running daemon for its health once. `None` while the socket is
/// not there yet or the daemon is not answering.
pub fn query_status() -> Option<serde_json::Value> {
    let path = settings::control_socket_path();
    let mut stream = UnixStream::connect(path).ok()?;
    // The event loop answers between events; a session's LLM polish can
    // hold it for a few seconds, so wait long enough not to call a busy
    // daemon dead.
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .ok()?;
    stream.write_all(b"{\"type\":\"status\"}\n").ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    serde_json::from_str(response.trim()).ok()
}

fn watch_until_ready(timeout: Duration, app: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<(String, String, String, String, String)> = None;
    let mut panes_opened = false;
    let mut last_reply = Instant::now();
    note("waiting for the app to report its own health over the control socket...");
    loop {
        let reply = query_status();
        if reply.is_some() {
            last_reply = Instant::now();
        } else if last_reply.elapsed() >= Duration::from_secs(12) {
            // The app went away (its own relaunch after a grant, or a
            // crash) and nothing brought it back. Do it here.
            note("the app is not answering; relaunching it");
            stop_running_daemons();
            launch_app(app)?;
            last_reply = Instant::now();
            continue;
        }
        if let Some(status) = reply {
            let field = |key: &str| {
                status
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
                    .to_string()
            };
            let snapshot = (
                field("health"),
                field("hotkey"),
                field("microphone"),
                field("asr"),
                field("polish"),
            );
            if last.as_ref() != Some(&snapshot) {
                note(&format!(
                    "hotkey: {} | microphone: {} | speech model: {} | polish: {} => {}",
                    snapshot.1, snapshot.2, snapshot.3, snapshot.4, snapshot.0
                ));
                last = Some(snapshot.clone());
            }
            if snapshot.1 == "blocked" && !panes_opened {
                panes_opened = true;
                let reason = field("hotkey_reason");
                warn(&format!("hotkey blocked: {reason}"));
                note(&format!(
                    "Use the {APP_NAME} onboarding panel to grant each permission in order. If the app is missing from a pane, press + and pick {}.",
                    app.display()
                ));
                note("When every row says Allowed, click Done; Sunoto will relaunch once.");
            }
            if snapshot.0 == "mic_starting" && snapshot.3 == "ready" {
                note("macOS should be showing the Microphone prompt; click Allow.");
            }
            if status
                .get("ready")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let shortcut = crate::settings::shortcut_label(&field("shortcut"));
                ok(&format!(
                    "{APP_NAME} {} is ready. In any app, tap {shortcut}, speak, and tap it again to paste. Esc cancels.",
                    env!("CARGO_PKG_VERSION")
                ));
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            let summary = last
                .map(|(health, hotkey, mic, asr, polish)| {
                    format!(
                        "last state: {health} (hotkey {hotkey}, microphone {mic}, speech model {asr}, polish {polish})"
                    )
                })
                .unwrap_or_else(|| "the app never answered on the control socket".to_string());
            return Err(format!(
                "{APP_NAME} did not become ready within {}s; {summary}. Check ~/Library/Logs/sunoto/daemon.log",
                timeout.as_secs()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// History is on by default, so setup says what is kept and where.
fn report_history(settings: &Settings) {
    if !settings.history_enabled {
        note("history: off");
        return;
    }
    let kept = match settings.history_retention_days {
        0 => "until deleted".to_string(),
        days => format!("{days} days"),
    };
    ok(&format!(
        "history: on; the text of each dictation (never audio) stays on this Mac for {kept} in {}",
        crate::history::history_dir().display()
    ));
    note(
        "set \"history_enabled\": false in the config to turn it off; `sunoto-daemon history clear --yes` deletes it",
    );
}

fn section(title: &str) {
    println!("== {title} ==");
}

fn note(message: &str) {
    println!("  {message}");
}

fn ok(message: &str) {
    println!("  \u{2713} {message}");
}

fn warn(message: &str) {
    println!("  ! {message}");
}

/// Print the running daemon's health in one screen.
pub fn print_status(json: bool) -> Result<(), Box<dyn Error>> {
    let Some(status) = query_status() else {
        return Err(format!(
            "no reply on {} within 15 s: the daemon is not running (or its socket file is stale)",
            settings::control_socket_path().display()
        )
        .into());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    if status.get("type").and_then(serde_json::Value::as_str) != Some("status") {
        let error = status
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unexpected reply");
        return Err(format!(
            "the running daemon did not answer the status command ({error}); it is probably an older build. Restart it after reinstalling."
        )
        .into());
    }
    let field = |key: &str| {
        status
            .get(key)
            .map(|value| match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "?".to_string())
    };
    // Daemons before 0.1.9 do not report a version.
    let version = status
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("before 0.1.9");
    println!("version:       {version}");
    println!("health:        {} {}", field("health"), field("detail"));
    println!("hotkey:        {} ({})", field("hotkey"), field("shortcut"));
    if field("hotkey") == "blocked" {
        println!("               {}", field("hotkey_reason"));
    }
    println!("microphone:    {}", field("microphone"));
    println!("speech model:  {} ({})", field("asr"), field("asr_backend"));
    println!("polish:        {}", field("polish"));
    // Daemons before 0.1.12 do not report history.
    if status.get("history").is_some() {
        println!("history:       {}", field("history"));
    }
    println!("overlay:       {}", field("overlay"));
    println!("session:       {}", field("session"));
    if status.get("updates").is_some() {
        println!("updates:       {}", field("updates"));
    }
    println!("pid / uptime:  {} / {}s", field("pid"), field("uptime_s"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_plist_names_the_daemon_as_the_executable() {
        let plist = info_plist();
        assert!(plist.contains("<key>CFBundleExecutable</key><string>sunoto-daemon</string>"));
        assert!(plist.contains(BUNDLE_ID));
        assert!(plist.contains("<key>LSUIElement</key><true/>"));
        assert!(plist.contains("NSMicrophoneUsageDescription"));
    }

    #[test]
    fn set_config_value_changes_only_that_key() {
        let path = std::env::temp_dir().join(format!("sunoto-config-{}.json", std::process::id()));
        fs::write(
            &path,
            r#"{"shortcut": "Ctrl+F1", "backend": "parakeet_mlx_streaming"}"#,
        )
        .unwrap();
        set_config_value(&path, "shortcut", "RightOption".into()).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(
            written,
            serde_json::json!({"shortcut": "RightOption", "backend": "parakeet_mlx_streaming"})
        );
    }

    #[test]
    #[cfg(target_os = "macos")] // setup is macOS-only; the shortcut default differs on Linux
    fn old_defaults_are_retired_and_chosen_values_kept() {
        let mut old_install = Settings {
            shortcut: "Ctrl+F1".to_string(),
            capture_idle_stop_secs: 120,
            ..Settings::default()
        };
        let changes = retire_old_defaults(&mut old_install);
        let keys: Vec<_> = changes
            .iter()
            .map(|(key, value, _)| (*key, value.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("shortcut", serde_json::json!("RightOption")),
                ("capture_idle_stop_secs", serde_json::json!(0)),
            ]
        );
        assert_eq!(old_install.capture_idle_stop_secs, 0);
        let mut chosen = Settings {
            shortcut: "Ctrl+F2".to_string(),
            capture_idle_stop_secs: 60,
            ..Settings::default()
        };
        assert!(retire_old_defaults(&mut chosen).is_empty());
        assert!(retire_old_defaults(&mut Settings::default()).is_empty());
    }

    #[test]
    fn only_the_old_default_shortcut_is_migrated() {
        for old in ["Ctrl+F1", "ctrl+f1", "Control + F1"] {
            assert!(is_old_default_shortcut(old), "{old}");
        }
        for kept in ["RightOption", "Ctrl+F2", "Cmd+F1", "Ctrl+Shift+F1", "F1"] {
            assert!(!is_old_default_shortcut(kept), "{kept}");
        }
    }

    #[test]
    fn uninstall_runs_only_on_known_arguments() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_uninstall_args(&args(&[])), Ok(Some(false)));
        assert_eq!(parse_uninstall_args(&args(&["--purge"])), Ok(Some(true)));
        assert_eq!(parse_uninstall_args(&args(&["--help"])), Ok(None));
        assert_eq!(parse_uninstall_args(&args(&["-h"])), Ok(None));
        assert!(parse_uninstall_args(&args(&["--pruge"])).is_err());
        assert!(parse_uninstall_args(&args(&["--purge", "now"])).is_err());
    }

    #[test]
    fn setup_args_parse_flags_and_reject_unknown() {
        let args = parse_args(&["--dry-run".into(), "--timeout-secs".into(), "9".into()]).unwrap();
        assert!(args.dry_run);
        assert!(args.login_item);
        assert_eq!(args.timeout, Duration::from_secs(9));
        let args = parse_args(&["--no-login-item".into()]).unwrap();
        assert!(!args.login_item);
        assert!(!args.update);
        assert_eq!(args.llm, LlmChoice::Ask);
        let args = parse_args(&["--update".into()]).unwrap();
        assert!(args.update && !args.login_item);
        assert_eq!(args.timeout, Duration::from_secs(600));
        assert_eq!(
            parse_args(&["--with-llm".into()]).unwrap().llm,
            LlmChoice::Download
        );
        assert_eq!(
            parse_args(&["--without-llm".into()]).unwrap().llm,
            LlmChoice::Skip
        );
        assert!(parse_args(&["--bogus".into()]).is_err());
        assert!(parse_args(&["--timeout-secs".into()]).is_err());
    }

    #[test]
    fn parses_matching_valid_signing_identity() {
        let output = format!(
            "  1) B7B77719D4A88C3F509F8539561F6DC096CBB22A \"{SIGNING_IDENTITY}\"\n     1 valid identities found\n"
        );
        assert_eq!(
            parse_signing_identity(&output).as_deref(),
            Some("B7B77719D4A88C3F509F8539561F6DC096CBB22A")
        );
        assert_eq!(parse_signing_identity("0 valid identities found\n"), None);
    }

    /// `plan_command` over made-up folders: which hold a `sunoto` (ours or
    /// not) and which are writable.
    fn plan(path: &[&str], commands: &[(&str, Existing)], writable: &[&str]) -> CommandPlan {
        let path: Vec<PathBuf> = path.iter().map(PathBuf::from).collect();
        let dirs = command_dirs(Path::new("/Users/u"));
        let existing = |dir: &Path| {
            commands
                .iter()
                .find(|(at, _)| Path::new(at) == dir)
                .map_or(Existing::Absent, |(_, existing)| *existing)
        };
        plan_command(&path, &dirs, existing, |dir: &Path| {
            writable.iter().any(|at| Path::new(at) == dir)
        })
    }

    fn install(dir: &str) -> CommandPlan {
        CommandPlan::Install(PathBuf::from(dir))
    }

    #[test]
    fn the_command_goes_to_the_first_writable_folder_on_path() {
        let brew = "/opt/homebrew/bin";
        let local = "/usr/local/bin";
        let home = "/Users/u/.local/bin";
        let system = ["/usr/bin", "/bin"];
        // Homebrew's folder first, then /usr/local/bin, when on PATH and ours.
        assert_eq!(
            plan(&[local, brew, system[0]], &[], &[brew, local]),
            install(brew)
        );
        assert_eq!(plan(&[local, brew], &[], &[local]), install(local));
        // Writable but not on PATH does not count.
        assert_eq!(plan(&[local], &[], &[brew]), install(home));
        // ~/.local/bin when it is on PATH and nothing better is.
        assert_eq!(plan(&[home, local], &[], &[home]), install(home));
        // Nothing usable: the fallback, which the caller reports off PATH.
        assert_eq!(plan(&system, &[], &[]), install(home));
    }

    #[test]
    fn the_command_stays_where_setup_put_it() {
        let brew = "/opt/homebrew/bin";
        let local = "/usr/local/bin";
        let home = "/Users/u/.local/bin";
        // Ours first on PATH is refreshed in place, even before Homebrew's.
        assert_eq!(
            plan(&[home, brew], &[(home, Existing::Ours)], &[brew, home]),
            install(home)
        );
        // Ours off PATH stays put rather than moving to the fallback, as
        // in an update run with launchd's PATH that the shell could not fix.
        assert_eq!(
            plan(&["/usr/bin"], &[(local, Existing::Ours)], &[]),
            install(local)
        );
        // A better folder on PATH wins over ours off PATH; the caller then
        // removes the old one.
        assert_eq!(
            plan(&[brew], &[(home, Existing::Ours)], &[brew, home]),
            install(brew)
        );
    }

    #[test]
    fn someone_elses_sunoto_is_never_replaced() {
        let brew = "/opt/homebrew/bin";
        let local = "/usr/local/bin";
        let home = "/Users/u/.local/bin";
        // First on PATH: ours would never run, or would hide it.
        assert_eq!(
            plan(&[brew, local], &[(brew, Existing::Foreign)], &[brew, local]),
            CommandPlan::Taken(PathBuf::from(brew))
        );
        assert_eq!(
            plan(
                &["/Users/u/bin", brew],
                &[("/Users/u/bin", Existing::Foreign)],
                &[brew]
            ),
            CommandPlan::Taken(PathBuf::from("/Users/u/bin"))
        );
        // Off PATH it is skipped like any folder off PATH.
        assert_eq!(
            plan(&[local], &[(brew, Existing::Foreign)], &[brew, local]),
            install(local)
        );
        // In the fallback, with nothing else left: nothing is installed.
        assert_eq!(
            plan(&["/usr/bin"], &[(home, Existing::Foreign)], &[home]),
            CommandPlan::Taken(PathBuf::from(home))
        );
    }

    #[test]
    fn only_setups_own_command_is_recognised_and_removed() {
        let root = std::env::temp_dir().join(format!("sunoto-command-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (ours, theirs, linked, missing) = (
            root.join("ours"),
            root.join("theirs"),
            root.join("linked"),
            root.join("missing/bin"),
        );
        for dir in [&ours, &theirs, &linked] {
            fs::create_dir_all(dir).unwrap();
        }
        write_command(&ours).unwrap();
        write_command(&missing).unwrap();
        fs::write(
            theirs.join(COMMAND_NAME),
            "#!/bin/sh\necho another sunoto\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(ours.join(COMMAND_NAME), linked.join(COMMAND_NAME)).unwrap();

        let script = ours.join(COMMAND_NAME);
        assert_eq!(fs::read_to_string(&script).unwrap(), command_script());
        assert_eq!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(existing_command(&ours), Existing::Ours);
        assert_eq!(existing_command(&missing), Existing::Ours);
        assert_eq!(existing_command(&theirs), Existing::Foreign);
        assert_eq!(existing_command(&linked), Existing::Foreign);
        assert_eq!(existing_command(&root), Existing::Absent);
        // A PATH entry that is a file holds no command.
        assert_eq!(
            existing_command(&theirs.join(COMMAND_NAME)),
            Existing::Absent
        );

        let dirs = [
            ours.clone(),
            theirs.clone(),
            linked.clone(),
            missing.clone(),
            root.clone(),
        ];
        let removed = remove_own_commands(&dirs);
        assert_eq!(
            removed,
            vec![ours.join(COMMAND_NAME), missing.join(COMMAND_NAME)]
        );
        assert_eq!(existing_command(&ours), Existing::Absent);
        assert_eq!(
            fs::read_to_string(theirs.join(COMMAND_NAME)).unwrap(),
            "#!/bin/sh\necho another sunoto\n"
        );
        assert!(fs::symlink_metadata(linked.join(COMMAND_NAME)).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_command_runs_the_installed_app_and_never_starts_it_bare() {
        let script = command_script();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(COMMAND_MARKER));
        assert!(script.contains(r#""$HOME/Applications/Sunoto.app/Contents/MacOS/sunoto-daemon""#));
        assert!(script.contains(r#"[ "$#" -gt 0 ] || set -- --help"#));
        assert!(script.trim_end().ends_with(r#"exec "$app" "$@""#));
    }

    #[test]
    fn a_background_download_stops_when_the_app_quits() {
        let stop = AtomicBool::new(false);
        let quick = Command::new("true").spawn().unwrap();
        assert!(wait_unless_stopped(quick, &stop).unwrap().success());
        let slow = Command::new("sleep").arg("30").spawn().unwrap();
        stop.store(true, Ordering::SeqCst);
        let started = Instant::now();
        assert!(wait_unless_stopped(slow, &stop).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

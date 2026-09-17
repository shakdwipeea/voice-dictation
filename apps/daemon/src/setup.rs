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
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
const ASR_MODEL_REPO: &str = "mlx-community/parakeet-tdt-0.6b-v3";

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

const USAGE: &str = "usage: sunoto-daemon setup [--dry-run] [--no-login-item] [--with-llm|--without-llm] [--timeout-secs N]

  --dry-run          assemble target/release/Sunoto.app only; install nothing
  --no-login-item    install and start the app without registering it at login
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
    llm: LlmChoice,
    timeout: Duration,
}

fn parse_args(args: &[String]) -> Result<Args, Box<dyn Error>> {
    let mut parsed = Args {
        dry_run: false,
        login_item: true,
        llm: LlmChoice::Ask,
        timeout: Duration::from_secs(600),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => parsed.dry_run = true,
            "--no-login-item" => parsed.login_item = false,
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
    let daemon = std::env::current_exe()?.canonicalize()?;
    let overlay = daemon.with_file_name("sunoto-overlay");
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set")?);
    let installed = home.join("Applications").join(format!("{APP_NAME}.app"));
    let log_path = home.join("Library/Logs/sunoto/daemon.log");
    // Prebuilt: this binary already sits in a release bundle with its
    // runtime inside. Nothing to build; install the bundle as it is.
    let prebuilt = app_bundle_root().filter(|bundle| bundle_is_prebuilt(bundle));
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
    let loaded = Settings::load(&config_path)?;
    if config_path.is_file() {
        ok(&format!(
            "config:  {} (left untouched)",
            config_path.display()
        ));
    } else if args.dry_run {
        note(&format!(
            "config:  {} would be created",
            config_path.display()
        ));
    } else {
        loaded.save(&config_path)?;
        ok(&format!("config:  created {}", config_path.display()));
    }
    let asr_python = preflight_asr_runtime(&root, &loaded)?;
    let loaded = ensure_llm_model(loaded, &config_path, args.llm, args.dry_run)?;
    if let Some(python) = asr_python.filter(|_| loaded.backend.starts_with("parakeet_mlx")) {
        ensure_asr_model(&python, args.dry_run)?;
    }

    section("bundle");
    if prebuilt.is_none() {
        assemble_bundle(&staging, &daemon, &overlay, &root)?;
        codesign(&staging)?;
        ok(&format!("assembled and signed {}", staging.display()));
    } else {
        ok("release bundle used as is");
    }
    if args.dry_run {
        note("dry run: nothing installed, nothing started");
        return Ok(());
    }

    section("install");
    stop_running_daemons();
    remove_legacy_login_item(&home);
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
    if args.login_item {
        register_login_item(&installed)?;
        ok(&format!("{APP_NAME} registered in Login Items"));
    }
    fs::create_dir_all(log_path.parent().expect("log path has a parent"))?;
    let _ = fs::write(&log_path, b"");
    // Clear records an earlier build left under our identifier before the
    // app asks for access. The app's own request then pre-lists it in each
    // pane with the switch off, so the user only flips switches. A reset
    // after the request would remove that listing again.
    reset_own_permission_records();
    launch_app(&installed)?;
    ok(&format!("started {APP_NAME}; log: {}", log_path.display()));

    section("permissions");
    watch_until_ready(args.timeout, &installed)
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

/// Fetch the speech model into the Hugging Face cache now, with progress,
/// instead of letting the first daemon start do it silently behind a
/// "loading speech model..." pill. No-op when it is already cached.
fn ensure_asr_model(python: &str, dry_run: bool) -> Result<(), Box<dyn Error>> {
    section("speech model");
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
        return Ok(());
    }
    if dry_run {
        note(&format!("would download {ASR_MODEL_REPO} (about 600 MB)"));
        return Ok(());
    }
    note(&format!(
        "downloading {ASR_MODEL_REPO} (about 600 MB, one time)..."
    ));
    let status = Command::new(python).args(["-c", &script]).status()?;
    if !status.success() {
        return Err("speech model download failed; rerun setup to resume".into());
    }
    ok("speech model ready");
    Ok(())
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
        note("LLM polish is off in the config; deterministic polish only");
        return Ok(settings);
    }
    if let Some(existing) = settings.llm_polish_model_file().filter(|p| p.is_file()) {
        ok(&format!("model present: {}", existing.display()));
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
                settings.save(config_path)?;
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
    )?;
    settings.llm_polish_model_path = Some(destination.to_string_lossy().into_owned());
    settings.llm_polish_enabled = true;
    settings.save(config_path)?;
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

/// `curl` with a progress bar into a `.part` file, then verify the SHA-256
/// and size before moving it into place. A verified file is never replaced.
fn download_verified(
    url: &str,
    destination: &Path,
    sha256: &str,
    expected_bytes: u64,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = destination.with_extension("gguf.part");
    note(&format!("downloading {url}"));
    let status = Command::new("curl")
        .args(["-L", "--fail", "--progress-bar", "-C", "-", "-o"])
        .arg(&partial)
        .arg(url)
        .status()?;
    if !status.success() {
        return Err(format!("download failed ({status}); rerun setup to resume").into());
    }
    let size = fs::metadata(&partial)?.len();
    if size != expected_bytes {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "downloaded {size} bytes, expected {expected_bytes}; the file was discarded"
        )
        .into());
    }
    note("verifying checksum...");
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
/// bundle. Config, logs, and downloaded models stay unless `--purge`.
pub fn uninstall(args: &[String]) -> Result<(), Box<dyn Error>> {
    let purge = args.iter().any(|arg| arg == "--purge");
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
    <key>NSMicrophoneUsageDescription</key>
    <string>Sunoto records microphone audio while you hold the push-to-talk shortcut.</string>
    <key>NSInputMonitoringUsageDescription</key>
    <string>Sunoto listens for the global push-to-talk shortcut while you use other applications.</string>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

fn codesign(bundle: &Path) -> Result<(), Box<dyn Error>> {
    // Ad-hoc, with a stable identifier. Every rebuild still changes the
    // cdhash, so permission grants do not survive an upgrade; that is the
    // documented limit until a Developer ID signs the bundle.
    let status = Command::new("codesign")
        .args([
            "--force",
            "--deep",
            "--sign",
            "-",
            "--identifier",
            BUNDLE_ID,
        ])
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
const DAEMON_PATTERNS: [&str; 3] = [
    "sunoto-daemon run$",
    "Sunoto.app/Contents/MacOS/sunoto-daemon$",
    "Sunoto Login.app/Contents/MacOS/sunoto-login",
];

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
fn stop_running_daemons() {
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

/// `open` the bundle and confirm a daemon process appeared. Launch Services
/// may treat another process from a bundle with the same identifier (this
/// installer, when it runs from a release bundle) as "already running" and
/// ignore the first `open`; `open -n` forces a new instance on retry.
fn launch_app(app: &Path) -> Result<(), Box<dyn Error>> {
    for attempt in 0..2 {
        let mut command = Command::new("open");
        if attempt == 1 {
            command.arg("-n");
        }
        let status = command.arg(app).status()?;
        if !status.success() {
            return Err(format!("open {} failed ({status})", app.display()).into());
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        while !daemons_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        if daemons_running() {
            break;
        }
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
        "while pgrep -f '[S]unoto.app/Contents/MacOS/sunoto-daemon$' >/dev/null; do sleep 0.2; done; open '{}' || open -n '{}'",
        bundle.display(),
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

/// Ask the running daemon for its health once. `None` while the socket is
/// not there yet or the daemon is not answering.
pub fn query_status() -> Option<serde_json::Value> {
    let path = settings::control_socket_path();
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.write_all(b"{\"type\":\"status\"}\n").ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    serde_json::from_str(response.trim()).ok()
}

/// How often the watcher relaunches a blocked app. A grant given in System
/// Settings only applies to processes started after it, so a relaunch is
/// what turns the user's toggle into a verified hotkey.
const BLOCKED_RELAUNCH_INTERVAL: Duration = Duration::from_secs(15);

fn watch_until_ready(timeout: Duration, app: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<(String, String, String, String, String)> = None;
    let mut panes_opened = false;
    let mut last_relaunch = Instant::now();
    note("waiting for the app to report its own health over the control socket...");
    loop {
        if let Some(status) = query_status() {
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
                    "{APP_NAME} is already listed in each pane with its switch off; switch it on in Input Monitoring, then in Accessibility. If it is missing, press + and pick {}.",
                    app.display()
                ));
                note(
                    "The app relaunches itself as soon as both switches are on (fallback: every 15 s).",
                );
                open_pane("Privacy_ListenEvent");
                std::thread::sleep(Duration::from_secs(2));
                open_pane("Privacy_Accessibility");
            } else if snapshot.1 == "blocked"
                && last_relaunch.elapsed() >= BLOCKED_RELAUNCH_INTERVAL
            {
                stop_running_daemons();
                launch_app(app)?;
                last_relaunch = Instant::now();
                note("relaunched to pick up new grants; still waiting...");
            }
            if snapshot.0 == "mic_starting" && snapshot.3 == "ready" {
                note("macOS should be showing the Microphone prompt; click Allow.");
            }
            if status
                .get("ready")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                ok(&format!(
                    "{APP_NAME} is ready. Hold {} in any app, speak, release.",
                    field("shortcut")
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

fn open_pane(anchor: &str) {
    let _ = Command::new("open")
        .arg(format!(
            "x-apple.systempreferences:com.apple.preference.security?{anchor}"
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
            "no daemon is answering on {}",
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
    println!("health:        {} {}", field("health"), field("detail"));
    println!("hotkey:        {} ({})", field("hotkey"), field("shortcut"));
    if field("hotkey") == "blocked" {
        println!("               {}", field("hotkey_reason"));
    }
    println!("microphone:    {}", field("microphone"));
    println!("speech model:  {} ({})", field("asr"), field("asr_backend"));
    println!("polish:        {}", field("polish"));
    println!("overlay:       {}", field("overlay"));
    println!("session:       {}", field("session"));
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
    fn setup_args_parse_flags_and_reject_unknown() {
        let args = parse_args(&["--dry-run".into(), "--timeout-secs".into(), "9".into()]).unwrap();
        assert!(args.dry_run);
        assert!(args.login_item);
        assert_eq!(args.timeout, Duration::from_secs(9));
        let args = parse_args(&["--no-login-item".into()]).unwrap();
        assert!(!args.login_item);
        assert_eq!(args.llm, LlmChoice::Ask);
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
}

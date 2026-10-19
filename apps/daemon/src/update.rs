//! Automatic updates for release installs of Sunoto.app (macOS).
//!
//! A background thread asks GitHub for the latest release a minute after
//! launch and then daily. A newer release is downloaded and checked against
//! the release key built into this binary. Its signed trusted comment must
//! name the version and the asset, so an older signed zip cannot pose as a
//! newer one. It is then unpacked and handed to the daemon loop. The loop
//! installs it only after ten minutes without dictation, by starting a
//! detached copy of this binary: `apply-update` moves the installed app
//! aside, runs the new app's `setup --update`, and puts the old app back if
//! that fails. Builds from a checkout never update themselves.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};

use crate::events::DaemonEvent;
use crate::{logging, setup};

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/shakdwipeea/voice-dictation/releases/latest";
const ASSET: &str = "Sunoto-macos-arm64.zip";
/// The public half of the key the release workflow signs with.
const RELEASE_KEY: &str = include_str!("../../../packaging/macos/release-signing.pub");
/// Test hooks: a URL (file:// works) answering like GitHub's latest-release
/// API, the public key file its releases are signed with, and a shorter idle
/// wait before installing.
const FEED_ENV: &str = "SUNOTO_UPDATE_FEED";
const KEY_ENV: &str = "SUNOTO_UPDATE_KEY";
const IDLE_ENV: &str = "SUNOTO_UPDATE_IDLE_SECS";
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const IDLE_BEFORE_INSTALL: Duration = Duration::from_secs(10 * 60);
/// How long the new app's setup may wait for it to report ready, and the
/// hard limit after which the old app is put back regardless.
const SETUP_READY_SECS: &str = "180";
const SETUP_DEADLINE: Duration = Duration::from_secs(240);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// `0.1.11` or `v0.1.11`. Anything else, pre-releases included, is None.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let version = Version(
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        parts.next().is_none().then_some(version)
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is major.minor.patch")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The `auto_update` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Notify,
    Off,
}

impl Mode {
    pub fn from_setting(value: &str) -> Self {
        match value {
            "notify" => Self::Notify,
            "off" => Self::Off,
            _ => Self::Install,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    zip_url: String,
    signature_url: String,
}

/// A verified release, unpacked and ready for `apply`.
#[derive(Debug)]
pub struct Staged {
    pub version: Version,
    pub app: PathBuf,
}

fn home() -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(
        std::env::var("HOME").map_err(|_| "HOME is not set")?,
    ))
}

fn installed_app() -> Result<PathBuf, Box<dyn Error>> {
    Ok(home()?.join("Applications/Sunoto.app"))
}

fn updates_dir() -> Result<PathBuf, Box<dyn Error>> {
    Ok(home()?.join("Library/Caches/sunoto/updates"))
}

/// "10 minutes", for messages about when an update installs.
pub fn idle_wait_label() -> String {
    match idle_before_install().as_secs().div_ceil(60) {
        1 => "1 minute".to_string(),
        minutes => format!("{minutes} minutes"),
    }
}

pub fn idle_before_install() -> Duration {
    std::env::var(IDLE_ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(IDLE_BEFORE_INSTALL)
}

/// Why this daemon will not update itself, or None when it will.
pub fn blocker(mode: Mode) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return Some("automatic updates are macOS-only".to_string());
    }
    if mode == Mode::Off {
        return Some("off (auto_update is \"off\" in the config)".to_string());
    }
    let Some(bundle) = setup::app_bundle_root() else {
        return Some("off (not running from Sunoto.app)".to_string());
    };
    if !setup::bundle_is_prebuilt(&bundle) {
        return Some(
            "off (built from a checkout; update with git pull and install-macos.sh)".to_string(),
        );
    }
    let installed = installed_app().ok()?;
    if bundle.canonicalize().ok() != installed.canonicalize().ok() {
        return Some(format!(
            "off (only {} updates itself, not {})",
            installed.display(),
            bundle.display()
        ));
    }
    None
}

fn parse_release(reply: &serde_json::Value) -> Result<Release, String> {
    let tag = reply
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or("the release reply has no tag_name")?;
    let version =
        Version::parse(tag).ok_or_else(|| format!("release tag {tag} is not a version"))?;
    let url_of = |name: &str| {
        reply
            .get("assets")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|asset| asset.get("name").and_then(serde_json::Value::as_str) == Some(name))
            .and_then(|asset| asset.get("browser_download_url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("release {tag} has no {name}"))
    };
    Ok(Release {
        version,
        zip_url: url_of(ASSET)?,
        signature_url: url_of(&format!("{ASSET}.minisig"))?,
    })
}

pub fn latest_release() -> Result<Release, Box<dyn Error>> {
    let url = std::env::var(FEED_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| LATEST_RELEASE.to_string());
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "30"])
        .args(["-H", "Accept: application/vnd.github+json", "-A"])
        .arg(format!("Sunoto/{}", env!("CARGO_PKG_VERSION")))
        .arg(&url)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "could not reach {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(parse_release(&reply)?)
}

/// The trusted comment the release workflow signs: it names the version so
/// that an older signed zip cannot be served as a newer one.
fn signed_comment(version: Version) -> String {
    format!("Sunoto {version} {ASSET}")
}

fn release_key() -> Result<PublicKey, Box<dyn Error>> {
    let text = match std::env::var_os(KEY_ENV) {
        Some(path) => fs::read_to_string(path)?,
        None => RELEASE_KEY.to_string(),
    };
    PublicKey::decode(&text).map_err(|error| format!("unreadable release key: {error}").into())
}

fn verify(
    file: &Path,
    signature: &str,
    version: Version,
    key: &PublicKey,
) -> Result<(), Box<dyn Error>> {
    let signature =
        Signature::decode(signature).map_err(|error| format!("unreadable signature: {error}"))?;
    let expected = signed_comment(version);
    if signature.trusted_comment() != expected {
        return Err(format!(
            "the signature is for {:?}, not {expected:?}",
            signature.trusted_comment()
        )
        .into());
    }
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|error| format!("the signature is not from the release key: {error}"))?;
    let mut input = fs::File::open(file)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier
        .finalize()
        .map_err(|error| format!("the download does not match its signature: {error}"))?;
    Ok(())
}

fn download(url: &str, to: &Path, progress: bool) -> Result<(), Box<dyn Error>> {
    let status = Command::new("curl")
        .args(["-fL", "--max-time", "1800"])
        .arg(if progress { "--progress-bar" } else { "-sS" })
        .arg("-o")
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("download of {url} failed ({status})").into());
    }
    Ok(())
}

/// Download, verify, and unpack `release`. Nothing is left behind if any
/// step fails.
pub fn prepare(release: &Release, progress: bool) -> Result<Staged, Box<dyn Error>> {
    let dir = updates_dir()?.join(release.version.to_string());
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    let staged = stage(release, &dir, progress);
    if staged.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    staged
}

fn stage(release: &Release, dir: &Path, progress: bool) -> Result<Staged, Box<dyn Error>> {
    let zip = dir.join(ASSET);
    let signature = dir.join(format!("{ASSET}.minisig"));
    download(&release.zip_url, &zip, progress)?;
    download(&release.signature_url, &signature, false)?;
    verify(
        &zip,
        &fs::read_to_string(&signature)?,
        release.version,
        &release_key()?,
    )?;
    let unpacked = dir.join("unpacked");
    let status = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&unpacked)
        .status()?;
    if !status.success() {
        return Err(format!("could not unpack {}", zip.display()).into());
    }
    fs::remove_file(&zip)?;
    let app = unpacked.join("Sunoto.app");
    if !setup::bundle_is_prebuilt(&app) {
        return Err(format!("{} is not a release bundle", app.display()).into());
    }
    let reported = bundle_version(&app)?;
    if reported != release.version {
        return Err(format!("the bundle reports {reported}, not {}", release.version).into());
    }
    Ok(Staged {
        version: release.version,
        app,
    })
}

fn bundle_version(app: &Path) -> Result<Version, Box<dyn Error>> {
    let output = Command::new(app.join("Contents/MacOS/sunoto-daemon"))
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .strip_prefix("Sunoto ")
        .and_then(Version::parse)
        .ok_or_else(|| format!("cannot read a version from {:?}", text.trim()).into())
}

/// Versions that failed to install here, so the checker does not retry them
/// every day. `sunoto update` still installs them on request.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    failed: Vec<String>,
}

fn state_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(home()?.join("Library/Application Support/sunoto/update-state.json"))
}

fn load_state() -> State {
    state_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(state: &State) {
    let Ok(path) = state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = fs::write(path, text + "\n");
    }
}

fn record_outcome(version: &str, installed: bool) {
    let mut state = load_state();
    state.failed.retain(|failed| failed != version);
    if !installed {
        state.failed.push(version.to_string());
    }
    save_state(&state);
}

/// Look for a newer release a minute after launch and then daily. In install
/// mode a verified, unpacked release goes to the daemon loop as
/// `DaemonEvent::UpdateReady`; in notify mode it is only logged.
pub fn spawn_checker(mode: Mode, events: Sender<DaemonEvent>) {
    let spawned = std::thread::Builder::new()
        .name("sunoto-update".to_string())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK_AFTER);
            loop {
                match check(mode) {
                    Ok(Some(staged)) => {
                        let _ = events.send(DaemonEvent::UpdateReady(staged));
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => logging::warn(&format!("update check failed: {error}")),
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
    if let Err(error) = spawned {
        logging::warn(&format!("could not start the update checker: {error}"));
    }
}

fn check(mode: Mode) -> Result<Option<Staged>, Box<dyn Error>> {
    let release = latest_release()?;
    let current = Version::current();
    let version = release.version;
    if version <= current {
        logging::info(&format!(
            "update check: Sunoto {current} is the latest release"
        ));
        return Ok(None);
    }
    if load_state().failed.contains(&version.to_string()) {
        logging::warn(&format!(
            "Sunoto {version} is out but failed to install here before; `sunoto update` retries it"
        ));
        return Ok(None);
    }
    if mode == Mode::Notify {
        logging::info(&format!(
            "Sunoto {version} is out (this is {current}); `sunoto update` installs it"
        ));
        return Ok(None);
    }
    if let Err(error) = setup::require_installed_identity(&home()?) {
        logging::warn(&format!(
            "Sunoto {version} is out but cannot install itself: {error}"
        ));
        return Ok(None);
    }
    logging::info(&format!("downloading Sunoto {version}"));
    let staged = prepare(&release, false)?;
    logging::info(&format!(
        "Sunoto {version} is downloaded and verified; it installs after {} without dictation",
        idle_wait_label()
    ));
    Ok(Some(staged))
}

fn open_update_log() -> Result<fs::File, Box<dyn Error>> {
    let path = home()?.join("Library/Logs/sunoto/update.log");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?)
}

/// Start the detached installer for `staged` and return. The caller then
/// exits, so the installer can replace this app. The installer is a copy of
/// this binary, outside the bundle it is about to move.
pub fn start_install(staged: &Staged) -> Result<(), Box<dyn Error>> {
    let dir = updates_dir()?.join("installer");
    fs::create_dir_all(&dir)?;
    let installer = dir.join("sunoto-daemon");
    let _ = fs::remove_file(&installer);
    fs::copy(std::env::current_exe()?, &installer)?;
    let log = open_update_log()?;
    Command::new(&installer)
        .arg("apply-update")
        .arg(&staged.app)
        .arg(staged.version.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        // Its own process group, so nothing sent to the daemon reaches it.
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// `sunoto-daemon apply-update APP VERSION`: replace the installed app with
/// APP. On any failure the previous app goes back in place and starts.
pub fn apply(app: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    let installed = installed_app()?;
    let previous = updates_dir()?.join("previous/Sunoto.app");
    logging::info(&format!(
        "installing Sunoto {version} from {}",
        app.display()
    ));
    // The daemon that started this exits on its own; make sure nothing of
    // the old app still runs before it moves.
    setup::stop_running_daemons();
    let mut moved = false;
    let outcome = (|| -> Result<(), Box<dyn Error>> {
        setup::require_installed_identity(&home()?)?;
        if previous.exists() {
            fs::remove_dir_all(&previous)?;
        }
        fs::create_dir_all(previous.parent().expect("has a parent"))?;
        fs::rename(&installed, &previous)?;
        moved = true;
        run_new_setup(app)
    })();
    match outcome {
        Ok(()) => {
            let _ = fs::remove_dir_all(previous.parent().expect("has a parent"));
            remove_staging(app);
            record_outcome(version, true);
            logging::info(&format!("Sunoto {version} installed"));
            Ok(())
        }
        Err(error) => {
            logging::error(&format!(
                "Sunoto {version} did not install: {error}; starting the previous version again"
            ));
            setup::stop_running_daemons();
            if moved {
                if installed.exists() {
                    fs::remove_dir_all(&installed)?;
                }
                fs::rename(&previous, &installed)?;
            }
            // The failed version is not retried, so do not keep its copy.
            remove_staging(app);
            let _ = fs::remove_dir_all(previous.parent().expect("has a parent"));
            setup::launch_app(&installed)?;
            record_outcome(version, false);
            logging::info("previous version restored and started");
            Err(error)
        }
    }
}

fn run_new_setup(app: &Path) -> Result<(), Box<dyn Error>> {
    // The bundle paths in this environment name the old app, which has just
    // moved away; the new setup derives them from its own bundle.
    let mut child =
        setup::without_bundle_env(&mut Command::new(app.join("Contents/MacOS/sunoto-daemon")))
            .args(["setup", "--update", "--timeout-secs", SETUP_READY_SECS])
            .stdin(Stdio::null())
            .spawn()?;
    let deadline = Instant::now() + SETUP_DEADLINE;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("the new setup exited with {status}").into())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the new setup did not finish within 4 minutes".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Remove `.../updates/<version>` once its app is installed.
fn remove_staging(app: &Path) {
    let (Ok(updates), Some(version_dir)) = (updates_dir(), app.parent().and_then(Path::parent))
    else {
        return;
    };
    if version_dir.starts_with(&updates) && version_dir != updates {
        let _ = fs::remove_dir_all(version_dir);
    }
}

/// `sunoto-daemon update [--check]`.
pub fn run_cli(args: &[String]) -> Result<(), Box<dyn Error>> {
    let check_only = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => return Err("usage: sunoto-daemon update [--check]".into()),
    };
    let installed = installed_app()?;
    if !installed.exists() {
        return Err(format!(
            "{} is not installed; run the installer first",
            installed.display()
        )
        .into());
    }
    if !setup::bundle_is_prebuilt(&installed) {
        return Err(
            "this Sunoto was built from a checkout; update it with `git pull` and `bash install-macos.sh`"
                .into(),
        );
    }
    let current = bundle_version(&installed)?;
    let release = latest_release()?;
    if release.version <= current {
        println!("Sunoto {current} is up to date");
        return Ok(());
    }
    if check_only {
        println!(
            "Sunoto {} is out (installed: {current}); `sunoto update` installs it",
            release.version
        );
        return Ok(());
    }
    println!("downloading Sunoto {}", release.version);
    let staged = prepare(&release, true)?;
    println!("signature verified; installing");
    apply(&staged.app, &staged.version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway key, and a signature it made over PAYLOAD with the trusted
    // comment the release workflow uses for version 9.9.9.
    const TEST_KEY: &str = "untrusted comment: minisign public key B4BAE904F12810E5
RWTlECjxBOm6tM1odWbNw/pKdsIiOMsh+2WG9t6c/q3YMOKeThGdBhPu
";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUTlECjxBOm6tDv6gGRqad7RkjK5LJXaNYlx+o3ULsFeY53mxc0fWLt8765f5gd7CRdgBy98EJesdvCeJBxaC/JdbjJSJGWBPw0=
trusted comment: Sunoto 9.9.9 Sunoto-macos-arm64.zip
GZjnGLtH1u9O3CeHnq8sQ0Huk70gmAHBZyMayPbD627h684+JYCSggCGC9JtsB6Dq/1k8ynfVE+U79zQ1d6MDA==
";
    const PAYLOAD: &[u8] = b"sunoto update test\n";

    fn payload_file(name: &str, contents: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("sunoto-update-test-{}-{name}", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(Version::parse("v0.1.10"), Some(Version(0, 1, 10)));
        assert_eq!(Version::parse("0.1.9"), Some(Version(0, 1, 9)));
        assert!(Version::parse("0.1.10").unwrap() > Version::parse("0.1.9").unwrap());
        assert!(Version::parse("1.0.0").unwrap() > Version::parse("0.99.99").unwrap());
        for bad in ["0.1", "0.1.10-rc1", "0.1.10.1", "latest", ""] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version(0, 1, 11).to_string(), "0.1.11");
        assert!(Version::current() >= Version(0, 1, 10));
    }

    #[test]
    fn modes_follow_the_setting() {
        assert_eq!(Mode::from_setting("install"), Mode::Install);
        assert_eq!(Mode::from_setting("notify"), Mode::Notify);
        assert_eq!(Mode::from_setting("off"), Mode::Off);
        assert!(blocker(Mode::Off).is_some());
    }

    #[test]
    fn release_reply_needs_the_zip_and_its_signature() {
        let reply = serde_json::json!({
            "tag_name": "v0.1.11",
            "assets": [
                {"name": "Sunoto-macos-arm64.zip", "browser_download_url": "https://x/zip"},
                {"name": "Sunoto-macos-arm64.zip.minisig", "browser_download_url": "https://x/sig"},
                {"name": "Sunoto-macos-arm64.zip.sha256", "browser_download_url": "https://x/sha"},
            ],
        });
        assert_eq!(
            parse_release(&reply).unwrap(),
            Release {
                version: Version(0, 1, 11),
                zip_url: "https://x/zip".to_string(),
                signature_url: "https://x/sig".to_string(),
            }
        );
        let unsigned = serde_json::json!({
            "tag_name": "v0.1.11",
            "assets": [{"name": "Sunoto-macos-arm64.zip", "browser_download_url": "https://x/zip"}],
        });
        assert!(parse_release(&unsigned).unwrap_err().contains(".minisig"));
        assert!(parse_release(&serde_json::json!({"tag_name": "nightly"})).is_err());
    }

    #[test]
    fn the_release_key_is_a_minisign_public_key() {
        assert!(PublicKey::decode(RELEASE_KEY).is_ok());
    }

    #[test]
    fn a_signed_release_verifies_only_as_its_own_version() {
        let key = PublicKey::decode(TEST_KEY).unwrap();
        let file = payload_file("own-version", PAYLOAD);
        verify(&file, TEST_SIGNATURE, Version(9, 9, 9), &key).unwrap();
        // The same signed file offered as another version is refused.
        let error = verify(&file, TEST_SIGNATURE, Version(9, 9, 10), &key).unwrap_err();
        assert!(error.to_string().contains("not \"Sunoto 9.9.10"), "{error}");
        let _ = fs::remove_file(file);
    }

    #[test]
    fn a_changed_download_or_another_key_is_refused() {
        let key = PublicKey::decode(TEST_KEY).unwrap();
        let tampered = payload_file("tampered", b"sunoto update test, changed\n");
        assert!(verify(&tampered, TEST_SIGNATURE, Version(9, 9, 9), &key).is_err());
        let _ = fs::remove_file(tampered);
        let file = payload_file("other-key", PAYLOAD);
        let release_key = PublicKey::decode(RELEASE_KEY).unwrap();
        assert!(verify(&file, TEST_SIGNATURE, Version(9, 9, 9), &release_key).is_err());
        let _ = fs::remove_file(file);
    }

    #[test]
    fn a_forged_trusted_comment_breaks_the_signature() {
        let key = PublicKey::decode(TEST_KEY).unwrap();
        let file = payload_file("forged", PAYLOAD);
        let forged = TEST_SIGNATURE.replace("Sunoto 9.9.9", "Sunoto 9.9.10");
        assert!(verify(&file, &forged, Version(9, 9, 10), &key).is_err());
        let _ = fs::remove_file(file);
    }
}

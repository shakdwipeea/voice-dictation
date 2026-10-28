//! What the app does for itself when Launch Services starts it, so that
//! opening Sunoto.app is the whole install, with no Terminal: move itself to
//! an Applications folder when opened from a disk image or Downloads, create
//! the settings file, and add itself to Login Items once. The speech model is
//! fetched by the daemon itself (`Settings::needs_model_download`), with
//! progress in the setup panel.
//!
//! Login Items go through `SMAppService`, which asks for no permission. The
//! AppleScript route older setups used needed Automation access to System
//! Events, a prompt that named the Terminal. Which route an install is on is
//! kept in `login-item` in Application Support: "app" (the app added
//! itself), "legacy" (an older setup added it through System Events; it stays
//! as it is, since removing it would need that prompt), or "off"
//! (`setup --no-login-item`). With no file the app adds itself and writes
//! "app"; it never adds itself again, so switching it off in System Settings
//! sticks.

use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::logging;
use crate::settings::{self, Settings};
use crate::setup::{self, APP_NAME};

const LOGIN_ITEM_FILE: &str = "login-item";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemRoute {
    App,
    Legacy,
    Off,
}

impl LoginItemRoute {
    fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "app" => Some(Self::App),
            "legacy" => Some(Self::Legacy),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Legacy => "legacy",
            Self::Off => "off",
        }
    }
}

fn login_item_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/sunoto")
            .join(LOGIN_ITEM_FILE),
    )
}

/// How this install's Login Item was made, if it has been decided.
pub fn login_item_route() -> Option<LoginItemRoute> {
    fs::read_to_string(login_item_file()?)
        .ok()
        .and_then(|text| LoginItemRoute::parse(&text))
}

pub fn set_login_item_route(route: LoginItemRoute) -> Result<(), Box<dyn Error>> {
    let path = login_item_file().ok_or("HOME is not set")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", route.name()))?;
    Ok(())
}

pub fn clear_login_item_route() {
    if let Some(path) = login_item_file() {
        let _ = fs::remove_file(path);
    }
}

/// Whether the daemon should start after `prepare`.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    Run,
    /// The app moved itself (the moved copy starts on its own) or the user
    /// chose Quit.
    Exit,
}

/// The first-start chores, run on every Launch Services start before the
/// daemon; each does nothing once done.
pub fn prepare(bundle: &Path) -> Start {
    if setup::bundle_is_prebuilt(bundle) && offer_move(bundle) == Start::Exit {
        return Start::Exit;
    }
    create_settings();
    add_login_item_once();
    Start::Run
}

fn create_settings() {
    let path = settings::config_path();
    if path.exists() {
        return;
    }
    match Settings::default().save_changes(&path) {
        Ok(()) => logging::info(&format!("created the settings file {}", path.display())),
        Err(error) => logging::warn(&format!("could not create the settings file: {error}")),
    }
}

fn add_login_item_once() {
    if login_item_route().is_some() {
        return;
    }
    match set_login_item(true) {
        Ok(()) => {
            logging::info("added Sunoto to Login Items; it starts when you log in");
            if let Err(error) = set_login_item_route(LoginItemRoute::App) {
                logging::warn(&format!("could not note the Login Item: {error}"));
            }
        }
        Err(error) => logging::warn(&format!(
            "could not add Sunoto to Login Items ({error}); trying again at the next start"
        )),
    }
}

#[cfg(target_os = "macos")]
fn set_login_item(on: bool) -> Result<(), String> {
    sunoto_desktop::set_login_item(on)
}

#[cfg(not(target_os = "macos"))]
fn set_login_item(_on: bool) -> Result<(), String> {
    Err("Login Items are macOS only".into())
}

/// `sunoto-daemon login-item [on|off|status]`, run from inside the bundle:
/// macOS ties the Login Item to the app the calling executable is in.
pub fn run_cli(args: &[String]) -> Result<(), Box<dyn Error>> {
    if setup::app_bundle_root().is_none() {
        return Err(format!(
            "run this as {APP_NAME}.app/Contents/MacOS/sunoto-daemon login-item ...; the Login Item belongs to the app"
        )
        .into());
    }
    match args.first().map(String::as_str) {
        Some("on") => {
            set_login_item(true)?;
            set_login_item_route(LoginItemRoute::App)?;
            println!("{APP_NAME} opens at login");
        }
        Some("off") => {
            set_login_item(false)?;
            set_login_item_route(LoginItemRoute::Off)?;
            println!("{APP_NAME} no longer opens at login");
        }
        None | Some("status") => {
            #[cfg(target_os = "macos")]
            println!("status: {:?}", sunoto_desktop::login_item_status());
            println!(
                "route:  {}",
                login_item_route().map_or("undecided", LoginItemRoute::name)
            );
        }
        Some(other) => {
            return Err(
                format!("usage: sunoto-daemon login-item [on|off|status] (got {other})").into(),
            );
        }
    }
    Ok(())
}

// ----- Move to Applications ------------------------------------------------------

/// Where to move an app that is really at `original`, or `None` when it is
/// already in an Applications folder. An existing copy in ~/Applications
/// (the curl installer's place) is replaced where it is, so its Login Item
/// and settings stay; otherwise /Applications when this user may write it.
fn move_destination(
    original: &Path,
    home: &Path,
    user_copy_exists: bool,
    system_writable: bool,
) -> Option<PathBuf> {
    let in_applications = original
        .components()
        .any(|part| part == Component::Normal("Applications".as_ref()));
    if in_applications {
        return None;
    }
    let app = format!("{APP_NAME}.app");
    Some(if user_copy_exists || !system_writable {
        home.join("Applications").join(app)
    } else {
        Path::new("/Applications").join(app)
    })
}

/// The mounted disk image `original` is on, to eject once the copy is out.
fn disk_image_volume(original: &Path) -> Option<PathBuf> {
    let mut parts = original.components();
    match (parts.next(), parts.next(), parts.next()) {
        (
            Some(Component::RootDir),
            Some(Component::Normal(volumes)),
            Some(Component::Normal(name)),
        ) if volumes == "Volumes" => Some(Path::new("/Volumes").join(name)),
        _ => None,
    }
}

fn offer_move(bundle: &Path) -> Start {
    let original = translocated_original(bundle).unwrap_or_else(|| bundle.to_path_buf());
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Start::Run;
    };
    let user_copy = home.join("Applications").join(format!("{APP_NAME}.app"));
    let Some(destination) = move_destination(
        &original,
        &home,
        user_copy.exists(),
        can_write(Path::new("/Applications")),
    ) else {
        return Start::Run;
    };
    logging::info(&format!(
        "{APP_NAME} runs from {}; offering to move it to {}",
        original.display(),
        destination.display()
    ));
    match ask_to_move(bundle) {
        Some(true) => match move_app(bundle, &original, &destination) {
            Ok(()) => Start::Exit,
            Err(error) => {
                logging::error(&format!(
                    "could not move {APP_NAME} to {}: {error}; running it where it is",
                    destination.display()
                ));
                Start::Run
            }
        },
        Some(false) => {
            logging::info("the user chose not to move the app; quitting");
            Start::Exit
        }
        // No answer (the overlay could not start): better running from
        // here than not at all.
        None => Start::Run,
    }
}

#[cfg(target_os = "macos")]
fn translocated_original(bundle: &Path) -> Option<PathBuf> {
    sunoto_desktop::translocated_original(bundle)
}

#[cfg(not(target_os = "macos"))]
fn translocated_original(_bundle: &Path) -> Option<PathBuf> {
    None
}

fn can_write(dir: &Path) -> bool {
    let probe = dir.join(format!(".sunoto-write-test-{}", std::process::id()));
    let writable = fs::File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    writable
}

/// Ask with the overlay's native alert: Move to Applications, or Quit.
/// `None` when it gives no answer.
fn ask_to_move(bundle: &Path) -> Option<bool> {
    let overlay = bundle.join("Contents/MacOS/sunoto-overlay");
    let output = Command::new(&overlay)
        .arg("--ask-move-to-applications")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output
        .as_ref()
        .map(|output| String::from_utf8_lossy(&output.stdout))
    {
        Ok(answer) if answer.trim() == "move" => Some(true),
        Ok(answer) if answer.trim() == "quit" => Some(false),
        _ => {
            logging::warn(&format!(
                "the Move to Applications question got no answer from {}",
                overlay.display()
            ));
            None
        }
    }
}

fn move_app(bundle: &Path, original: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    stop_app_at(destination);
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    // ditto keeps the signature and the bundle's symlinks intact.
    let status = Command::new("ditto")
        .arg(bundle)
        .arg(destination)
        .status()?;
    if !status.success() {
        let _ = fs::remove_dir_all(destination);
        return Err(format!("ditto failed ({status})").into());
    }
    // The user opened this copy, so Gatekeeper has already checked it; a
    // quarantined copy would be translocated again and asked about again.
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(destination)
        .status();
    logging::info(&format!("moved {APP_NAME} to {}", destination.display()));
    let volume = disk_image_volume(original);
    if volume.is_none() {
        // A copy in Downloads: move means it is gone from there.
        #[cfg(target_os = "macos")]
        if let Err(error) = sunoto_desktop::move_to_trash(original) {
            logging::warn(&format!(
                "could not move {} to the Trash: {error}",
                original.display()
            ));
        }
    }
    // Start the moved copy once this one has exited (Launch Services
    // ignores `open` for an app it still sees running), then eject the disk
    // image this one ran from.
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
open -n "$2"
if [ -n "$3" ]; then sleep 2; hdiutil detach -quiet "$3"; fi"#;
    setup::without_bundle_env(&mut Command::new("sh"))
        .args(["-c", script, "sh"])
        .arg(std::process::id().to_string())
        .arg(destination)
        .arg(volume.unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Stop a copy of the app running from `bundle`, matched by its full path so
/// this process, the same binary elsewhere, is spared.
fn stop_app_at(bundle: &Path) {
    let pattern = format!(
        "^{}/Contents/MacOS/sunoto-(daemon|overlay)$",
        bundle.display()
    );
    let running = || {
        Command::new("pgrep")
            .args(["-f", &pattern])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if !running() {
        return;
    }
    let _ = Command::new("pkill").args(["-f", &pattern]).status();
    let deadline = Instant::now() + Duration::from_secs(10);
    while running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_item_route_round_trips_and_ignores_junk() {
        for route in [
            LoginItemRoute::App,
            LoginItemRoute::Legacy,
            LoginItemRoute::Off,
        ] {
            assert_eq!(
                LoginItemRoute::parse(&format!("{}\n", route.name())),
                Some(route)
            );
        }
        assert_eq!(LoginItemRoute::parse("yes"), None);
        assert_eq!(LoginItemRoute::parse(""), None);
    }

    #[test]
    fn only_an_app_outside_applications_is_moved() {
        let home = Path::new("/Users/someone");
        for inside in [
            "/Applications/Sunoto.app",
            "/Applications/Utilities/Sunoto.app",
            "/Users/someone/Applications/Sunoto.app",
            "/Volumes/External/Applications/Sunoto.app",
        ] {
            assert_eq!(
                move_destination(Path::new(inside), home, false, true),
                None,
                "{inside}"
            );
        }
        let dmg = Path::new("/Volumes/Sunoto/Sunoto.app");
        assert_eq!(
            move_destination(dmg, home, false, true),
            Some(PathBuf::from("/Applications/Sunoto.app"))
        );
        assert_eq!(
            move_destination(dmg, home, false, false),
            Some(PathBuf::from("/Users/someone/Applications/Sunoto.app")),
            "no write access to /Applications"
        );
        assert_eq!(
            move_destination(
                Path::new("/Users/someone/Downloads/Sunoto.app"),
                home,
                true,
                true
            ),
            Some(PathBuf::from("/Users/someone/Applications/Sunoto.app")),
            "an existing install is replaced where it is"
        );
    }

    #[test]
    fn the_disk_image_is_the_volume_the_app_came_from() {
        assert_eq!(
            disk_image_volume(Path::new("/Volumes/Sunoto 0.2/Sunoto.app")),
            Some(PathBuf::from("/Volumes/Sunoto 0.2"))
        );
        assert_eq!(
            disk_image_volume(Path::new("/Users/someone/Downloads/Sunoto.app")),
            None
        );
        assert_eq!(disk_image_volume(Path::new("/Volumes")), None);
    }
}

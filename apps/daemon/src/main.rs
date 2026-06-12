mod bench;
mod daemon;
mod eval;
mod logging;
mod settings;

use std::error::Error;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use sunoto_ipc::{SidecarClient, SidecarEvent, SidecarMessage, SidecarRequest};
use sunoto_linux::x11::{HotkeyListener, Shortcut, UiAdapter};

use settings::Settings;

const USAGE: &str = "Usage: sunoto-daemon COMMAND [OPTIONS]

Commands:
  check                     verify X11, XTEST, shortcut, and the mock sidecar
  selftest                  run X11 insertion, push-to-talk, clipboard self-tests
  insert TEXT               type TEXT at the focused cursor
  run                       run the dictation daemon
  bench [OPTIONS]           measure release-to-insertion latency percentiles
  eval [OPTIONS]            measure the pipeline's zero-edit rate on a corpus
  config show               print the effective settings as JSON
  config init               write a default settings file if none exists

Common options:
  --config PATH             settings file (default ~/.config/sunoto/config.json)
  --backend mock|nemotron   override the configured ASR backend
  --profile-ms N            override the streaming profile (80|160|560|1120)

Bench options:
  --sessions N              number of dictation sessions (default 10)
  --audio PATH              16 kHz mono WAV input (default tests/corpus/hf-sample1.wav)
  --unpaced                 send audio as fast as possible instead of real time
  --output PATH             write the JSON report to PATH

Eval options (always runs the default pipeline config plus the corpus's own
dictionary and snippets, so results are machine-independent):
  --corpus PATH             corpus manifest (default tests/corpus/phase2-text-cases.json)
  --output PATH             write the JSON report to PATH
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match dispatch(&args) {
        Ok(()) => 0,
        Err(error) => {
            logging::error(&error.to_string());
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(command) = args.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return Err("missing command".into());
    };
    let rest = &args[1..];
    match command {
        "check" => check(rest),
        "selftest" => selftest(),
        "insert" if rest.len() == 1 => {
            let mut ui = UiAdapter::open()?;
            let text = settings::sanitize_for_insertion(&rest[0], false);
            match ui.insert_direct(&text) {
                Ok(()) => Ok(()),
                Err(sunoto_linux::x11::X11Error::UnsupportedCharacter(_)) => {
                    ui.insert_via_clipboard(&text)?;
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
        "run" => daemon::run(load_settings(rest)?),
        "bench" => bench::run(load_settings(rest)?, parse_bench_args(rest)?),
        "eval" => eval::run(parse_eval_args(rest)),
        "config" => config(rest),
        _ => {
            eprint!("{USAGE}");
            Err(format!("unknown command: {command}").into())
        }
    }
}

fn option_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn load_settings(args: &[String]) -> Result<Settings, Box<dyn Error>> {
    let path = option_value(args, "--config")
        .map(PathBuf::from)
        .unwrap_or_else(settings::config_path);
    let mut loaded = Settings::load(&path)?;
    if let Some(backend) = option_value(args, "--backend") {
        loaded.backend = backend.to_string();
    }
    if let Some(profile) = option_value(args, "--profile-ms") {
        loaded.profile_ms = profile.parse()?;
    }
    match loaded.profile_ms {
        80 | 160 | 560 | 1120 => Ok(loaded),
        other => Err(format!("unsupported profile_ms {other}; use 80, 160, 560, or 1120").into()),
    }
}

fn parse_bench_args(args: &[String]) -> Result<bench::BenchArgs, Box<dyn Error>> {
    let mut parsed = bench::BenchArgs::default();
    if let Some(sessions) = option_value(args, "--sessions") {
        parsed.sessions = sessions.parse()?;
    }
    if let Some(audio) = option_value(args, "--audio") {
        parsed.audio = PathBuf::from(audio);
    }
    if args.iter().any(|arg| arg == "--unpaced") {
        parsed.paced = false;
    }
    parsed.output = option_value(args, "--output").map(PathBuf::from);
    Ok(parsed)
}

fn parse_eval_args(args: &[String]) -> eval::EvalArgs {
    let mut parsed = eval::EvalArgs::default();
    if let Some(corpus) = option_value(args, "--corpus") {
        parsed.corpus = PathBuf::from(corpus);
    }
    parsed.output = option_value(args, "--output").map(PathBuf::from);
    parsed
}

fn config(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = option_value(args, "--config")
        .map(PathBuf::from)
        .unwrap_or_else(settings::config_path);
    match args.first().map(String::as_str) {
        Some("show") => {
            let loaded = Settings::load(&path)?;
            println!("{}", serde_json::to_string_pretty(&loaded)?);
            Ok(())
        }
        Some("init") => {
            if path.exists() {
                return Err(format!("{} already exists", path.display()).into());
            }
            Settings::default().save(&path)?;
            println!("wrote {}", path.display());
            Ok(())
        }
        _ => Err("usage: sunoto-daemon config show|init [--config PATH]".into()),
    }
}

fn check(args: &[String]) -> Result<(), Box<dyn Error>> {
    let loaded = load_settings(args)?;
    let shortcut = Shortcut::parse(&loaded.shortcut)?;
    drop(HotkeyListener::open(&shortcut)?);
    println!("X11 hotkey grab: ok ({})", loaded.shortcut);
    let ui = UiAdapter::open()?;
    println!("X11 XTEST/UI connection: ok");
    match ui.window_class(ui.focused_window()) {
        Some((instance, class)) => println!("focused window class: {instance} / {class}"),
        None => println!("focused window class: unavailable"),
    }
    drop(ui);

    let mock = settings::repo_root().join("services/asr/mock_sidecar.py");
    let (tx, rx) = mpsc::channel::<SidecarMessage>();
    let mut sidecar = SidecarClient::spawn(
        "python3",
        &[mock.to_str().ok_or("bad sidecar path")?],
        move |message| tx.send(message).is_ok(),
    )?;
    sidecar.send(&SidecarRequest::Health)?;
    match rx.recv_timeout(Duration::from_secs(10))? {
        SidecarMessage::Event(SidecarEvent::Ready { backend }) => {
            println!("ASR sidecar protocol: ok ({backend})");
        }
        other => return Err(format!("unexpected sidecar response: {other:?}").into()),
    }
    println!("check passed");
    Ok(())
}

fn selftest() -> Result<(), Box<dyn Error>> {
    let mut ui = UiAdapter::open()?;
    ui.selftest_insert("sunoto phase one")?;
    println!("X11 focused-cursor insertion self-test: passed");
    ui.selftest_clipboard()?;
    println!("X11 clipboard round-trip self-test: passed");
    ui.selftest_window_class()?;
    println!("X11 window-class lookup self-test: passed");
    let listener = HotkeyListener::open(&Shortcut::default())?;
    listener.selftest_push_to_talk()?;
    println!("X11 push-to-talk self-test (modifier released first): passed");
    Ok(())
}

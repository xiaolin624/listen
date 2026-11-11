#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;

use listen::{audio::WasapiPlayback, whisper::Whisper};
use std::{env, path::PathBuf};

pub struct Config {
    engine: PathBuf,
    model: PathBuf,
    vad_model: PathBuf,
    runtime: PathBuf,
    language: String,
    threads: usize,
    hidden: bool,
}

enum LaunchAction {
    Run(Config),
    Control(String),
    Help,
}

const HELP: &str = "Listen: Ctrl+Alt+R starts/stops default multimedia playback capture.\nOptions: --engine <whisper-cli.exe> --model <ggml.bin> --vad-model <ggml-silero.bin> --runtime <directory> --language <zh|auto|...> --threads <count> --hidden\nThe engine package must include vad-speech-segments.exe.\nControl running application: --control <start|stop|toggle|show|quit>\nResults are copyable in the application. Messages are never sent automatically.";

fn parse() -> Result<LaunchAction, String> {
    let executable = env::current_exe().map_err(|e| e.to_string())?;
    let root = executable
        .ancestors()
        .find(|path| path.join(".tools").is_dir())
        .map(PathBuf::from)
        .unwrap_or(env::current_dir().map_err(|e| e.to_string())?);
    let mut config = Config {
        engine: root.join(".tools/whisper-v1.8.2-cpu/bin/Release/whisper-cli.exe"),
        model: root.join(".tools/models/ggml-small.bin"),
        vad_model: root.join(".tools/models/ggml-silero-v5.1.2.bin"),
        runtime: root.join(".artifacts/listen"),
        language: "zh".into(),
        threads: 6,
        hidden: false,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--hidden" {
            config.hidden = true;
            continue;
        }
        if arg == "--help" || arg == "-h" {
            return Ok(LaunchAction::Help);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {arg}"))?;
        match arg.as_str() {
            "--control" => {
                if args.next().is_some() {
                    return Err("--control must be used on its own".into());
                }
                return Ok(LaunchAction::Control(value));
            }
            "--engine" => config.engine = PathBuf::from(value),
            "--model" => config.model = PathBuf::from(value),
            "--vad-model" => config.vad_model = PathBuf::from(value),
            "--runtime" => config.runtime = PathBuf::from(value),
            "--language" => config.language = value,
            "--threads" => {
                config.threads = value
                    .parse()
                    .map_err(|_| "--threads must be a positive integer")?;
                if config.threads == 0 {
                    return Err("--threads must be positive".into());
                }
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    // Resolve once so changing the foreground application cannot change file interpretation.
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    for path in [
        &mut config.engine,
        &mut config.model,
        &mut config.vad_model,
        &mut config.runtime,
    ] {
        if !path.is_absolute() {
            *path = cwd.join(&*path);
        }
    }
    Ok(LaunchAction::Run(config))
}

fn main() {
    let result = match parse() {
        Ok(LaunchAction::Run(config)) => gui::run(config),
        Ok(LaunchAction::Control(control)) => gui::control(&control),
        Ok(LaunchAction::Help) => {
            gui::show_help(HELP);
            Ok(())
        }
        Err(error) => {
            gui::show_error(&error);
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        gui::show_error(&error);
        std::process::exit(1);
    }
}

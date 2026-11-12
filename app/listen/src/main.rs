#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capsule;
mod capsule_window;
mod gui;
mod panel;
mod panel_window;

use listen::{
    LocalTranscriber,
    audio::WasapiPlayback,
    sensevoice::SenseVoice,
    whisper::{Backend, Whisper},
};
use std::{env, path::PathBuf, sync::Arc};

#[derive(Clone, Copy)]
enum Engine {
    SenseVoice,
    Whisper,
}

pub struct Config {
    /// Whisper.cpp executable; only used when the fallback engine is selected.
    engine: PathBuf,
    /// Whisper ggml model; only used when the fallback engine is selected.
    model: PathBuf,
    /// Whisper ggml Silero VAD model; only used when the fallback engine is selected.
    vad_model: PathBuf,
    runtime: PathBuf,
    /// Whisper language hint; SenseVoice has its own default `auto`.
    language: String,
    threads: usize,
    hidden: bool,
    backend: Backend,
    glossary: PathBuf,
    engine_kind: Engine,
    sherpa_lib: PathBuf,
    sensevoice_model: PathBuf,
    sensevoice_tokens: PathBuf,
    sensevoice_vad: PathBuf,
    sensevoice_language: String,
}

impl Config {
    /// Builds the configured local engine. The default is the resident
    /// SenseVoice engine; whisper.cpp is only used when explicitly selected and
    /// is never a silent fallback inside the SenseVoice module.
    pub fn transcriber(&self) -> Arc<dyn LocalTranscriber> {
        match self.engine_kind {
            Engine::SenseVoice => Arc::new(SenseVoice::new(
                self.sherpa_lib.clone(),
                self.sensevoice_model.clone(),
                self.sensevoice_tokens.clone(),
                self.sensevoice_vad.clone(),
                self.sensevoice_language.clone(),
                self.threads,
            )),
            Engine::Whisper => Arc::new(Whisper::with_backend(
                self.engine.clone(),
                self.model.clone(),
                self.vad_model.clone(),
                self.language.clone(),
                self.threads,
                self.backend,
            )),
        }
    }
}

enum LaunchAction {
    Run(Config),
    Control(String),
    Help,
}

const HELP: &str = "Listen: Ctrl+Alt+R starts/stops capture of the default multimedia render endpoint.
Only that endpoint is captured; audio routed only to the default communication endpoint or to another device is outside the capture. A default-route switch or endpoint removal interrupts the capture, retains received audio/checkpoint, and marks the result incomplete.

Recognition: SenseVoice q8 through sherpa-onnx, resident in this process on the CPU (AVX2 required). It is prepared on a background thread; capture can start immediately and is buffered until the model reports ready. Silence, music and notification windows are gated by Silero VAD and are never decoded into fabricated text. No cloud service is used and whisper is not a silent fallback.

Options:
  --engine <sensevoice|whisper|whisper-cli.exe>
        sensevoice (default) or whisper select the engine; a path selects whisper.cpp and uses that executable.
  --sherpa-lib <directory>
        directory holding sherpa-onnx-c-api.dll and onnxruntime.dll.
  --sensevoice-model <model.int8.onnx>
        pinned SenseVoice q8 model.
  --sensevoice-tokens <tokens.txt>
        SenseVoice tokens.
  --sensevoice-vad <silero_vad.onnx>
        pinned Silero VAD model used as the non-speech gate.
  --sensevoice-language <auto|zh|en|...>
        SenseVoice language hint (default auto).
  --model <ggml.bin> --vad-model <ggml-silero.bin> --language <zh|auto|...> --backend <host|cli>
        whisper.cpp fallback settings; --backend host (default) keeps the model resident, cli starts one process per window.
  --runtime <directory> --threads <count> --hidden
        runtime files, recognition threads and start-hidden behavior.
  --glossary <glossary.txt>
        local UTF-8 term list, one term per line, re-read at every capture start
        (default: <runtime>\\glossary.txt). A malformed file is reported and the
        uncorrected transcript is still delivered.

Control running application: --control <start|stop|toggle|retry|cleanup|show|panel|copy|refine|quit>
Retry recognizes the same audio retained after a failed local recognition; cleanup deletes that audio.
show opens the expanded result panel, panel toggles it and copy copies the complete current result to the clipboard.
refine asks the DeepSeek API to correct the complete current result. Only that transcript text is sent, never the audio.
The refined text replaces the appended transcript only while the original target still holds exactly what Listen wrote; if the target was edited, the refined text stays in the panel and the edit is preserved. Without DEEPSEEK_API_KEY in the local environment the refine entry is unavailable, and local recognition is unaffected.
Clicking the status capsule toggles the expanded result panel; the panel shows the complete current result and the status detail, and results are never sent automatically.";

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
        backend: Backend::ModelHost,
        glossary: PathBuf::new(),
        engine_kind: Engine::SenseVoice,
        sherpa_lib: root
            .join(".tools/sherpa-onnx/sherpa-onnx-v2.0.1-win-x64-shared-MT-Release/lib"),
        sensevoice_model: root.join(".tools/models/sense-voice/model.int8.onnx"),
        sensevoice_tokens: root.join(".tools/models/sense-voice/tokens.txt"),
        sensevoice_vad: root.join(".tools/models/sense-voice/silero_vad.onnx"),
        sensevoice_language: "auto".into(),
    };
    let mut glossary = None;
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
            "--engine" => match value.as_str() {
                "sensevoice" | "sense-voice" | "sense_voice" => {
                    config.engine_kind = Engine::SenseVoice;
                }
                "whisper" => config.engine_kind = Engine::Whisper,
                _ => {
                    config.engine = PathBuf::from(value);
                    config.engine_kind = Engine::Whisper;
                }
            },
            "--sherpa-lib" => config.sherpa_lib = PathBuf::from(value),
            "--sensevoice-model" => config.sensevoice_model = PathBuf::from(value),
            "--sensevoice-tokens" => config.sensevoice_tokens = PathBuf::from(value),
            "--sensevoice-vad" => config.sensevoice_vad = PathBuf::from(value),
            "--sensevoice-language" => config.sensevoice_language = value,
            "--model" => config.model = PathBuf::from(value),
            "--vad-model" => config.vad_model = PathBuf::from(value),
            "--runtime" => config.runtime = PathBuf::from(value),
            "--glossary" => glossary = Some(PathBuf::from(value)),
            "--language" => config.language = value,
            "--backend" => {
                config.backend = match value.as_str() {
                    "host" => Backend::ModelHost,
                    "cli" => Backend::Cli,
                    _ => return Err("--backend must be host or cli".into()),
                };
            }
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
    // A custom runtime also moves the default term list; an explicit --glossary
    // always wins.
    config.glossary = glossary.unwrap_or_else(|| config.runtime.join("glossary.txt"));
    // Resolve once so changing the foreground application cannot change file interpretation.
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    for path in [
        &mut config.engine,
        &mut config.model,
        &mut config.vad_model,
        &mut config.runtime,
        &mut config.glossary,
        &mut config.sherpa_lib,
        &mut config.sensevoice_model,
        &mut config.sensevoice_tokens,
        &mut config.sensevoice_vad,
    ] {
        if !path.is_absolute() {
            *path = cwd.join(&*path);
        }
    }
    Ok(LaunchAction::Run(config))
}

fn main() {
    // A control invocation is part of a script or another automation. It must
    // never block on a modal dialog: report the failure on stderr and use the exit
    // code. Interactive launch errors keep the existing visible message.
    let control_mode = env::args().any(|argument| argument == "--control");
    let result = match parse() {
        Ok(LaunchAction::Run(config)) => {
            // The window owns Application construction, so hand it the resolved term
            // list before it starts. The file itself is re-read at every capture.
            listen::glossary::set_default_path(config.glossary.clone());
            gui::run(config)
        }
        Ok(LaunchAction::Control(control)) => gui::control(&control),
        Ok(LaunchAction::Help) => {
            gui::show_help(HELP);
            Ok(())
        }
        Err(error) => {
            if control_mode {
                control_error(&error);
                std::process::exit(2);
            }
            gui::show_error(&error);
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        if control_mode {
            control_error(&error);
            std::process::exit(1);
        }
        gui::show_error(&error);
        std::process::exit(1);
    }
}

/// Writes a control failure without assuming a console or a valid stderr handle.
fn control_error(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "Listen control failed: {message}");
}

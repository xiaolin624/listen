//! Process launch configuration: which recognition path this process runs and
//! where its files live. The daily default is the accepted cloud path (#40);
//! local recognition stays available as an explicit fallback (`--engine`), and
//! the process never switches between the two inside one task.
use crate::{
    AppResult, Application, LocalTranscriber, PlaybackAudio,
    audio::{WasapiPcmPlayback, WasapiPlayback},
    cloud::CloudSessions,
    recognition::{LocalSessions, RecognitionSessions},
    sensevoice::SenseVoice,
    whisper::{Backend, Whisper},
};
use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    SenseVoice,
    Whisper,
}

/// The recognition path this process was configured to run. It is the single
/// decision the application constructor consumes, so a status or test can name the
/// selected path without guessing from a backend string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recognition {
    /// The accepted Alibaba cloud trial, fixed model and region.
    CloudTrial,
    /// Local recognition, only ever selected explicitly.
    Local(Engine),
}

#[derive(Debug)]
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
    /// `--no-tray`: start without a notification-area icon. The default launch
    /// keeps the icon, so this is false unless the flag is passed.
    no_tray: bool,
    backend: Backend,
    glossary: Option<PathBuf>,
    engine_kind: Engine,
    sherpa_lib: PathBuf,
    sensevoice_model: PathBuf,
    sensevoice_tokens: PathBuf,
    sensevoice_vad: PathBuf,
    sensevoice_language: String,
    cloud_trial: bool,
    /// The pending-audio budget, its warning line and the free-disk floor (#36).
    storage: crate::storage::StoragePolicy,
}

#[derive(Debug)]
pub enum LaunchAction {
    Run(Config),
    Control(String),
    Help,
}

impl Config {
    /// The built-in paths for a checkout or unpacked package rooted at `root`.
    pub fn defaults(root: &Path) -> Self {
        Self {
            engine: root.join(".tools/whisper-v1.8.2-cpu/bin/Release/whisper-cli.exe"),
            model: root.join(".tools/models/ggml-small.bin"),
            vad_model: root.join(".tools/models/ggml-silero-v5.1.2.bin"),
            runtime: root.join(".artifacts/listen"),
            language: "zh".into(),
            threads: 6,
            hidden: false,
            no_tray: false,
            backend: Backend::ModelHost,
            glossary: None,
            engine_kind: Engine::SenseVoice,
            sherpa_lib: root
                .join(".tools/sherpa-onnx/sherpa-onnx-v2.0.1-win-x64-shared-MT-Release/lib"),
            sensevoice_model: root.join(".tools/models/sense-voice/model.int8.onnx"),
            sensevoice_tokens: root.join(".tools/models/sense-voice/tokens.txt"),
            sensevoice_vad: root.join(".tools/models/sense-voice/silero_vad.onnx"),
            sensevoice_language: "auto".into(),
            // #40: the accepted cloud path is the daily default. Local recognition
            // is an explicit fallback and is never reached by falling through.
            cloud_trial: true,
            storage: crate::storage::StoragePolicy::default(),
        }
    }

    /// Parse the process arguments after the executable name. `root` is the checkout
    /// or unpacked package the built-in default paths are resolved against.
    ///
    /// Explicit `--engine` selects local recognition; explicit `--cloud-trial ali`
    /// selects the cloud path. Asking for both is refused instead of silently
    /// choosing one, in either order.
    pub fn parse(
        root: &Path,
        args: impl IntoIterator<Item = String>,
    ) -> Result<LaunchAction, String> {
        let mut config = Self::defaults(root);
        let mut glossary = None;
        let mut explicit_engine = false;
        let mut explicit_cloud_trial = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--hidden" {
                config.hidden = true;
                continue;
            }
            if arg == "--no-tray" {
                config.no_tray = true;
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
                "--cloud-trial" => {
                    if value != "ali" {
                        return Err("--cloud-trial must be ali".into());
                    }
                    explicit_cloud_trial = true;
                    config.cloud_trial = true;
                }
                "--engine" => {
                    explicit_engine = true;
                    match value.as_str() {
                        "sensevoice" | "sense-voice" | "sense_voice" => {
                            config.engine_kind = Engine::SenseVoice;
                        }
                        "whisper" => config.engine_kind = Engine::Whisper,
                        _ => {
                            config.engine = PathBuf::from(value);
                            config.engine_kind = Engine::Whisper;
                        }
                    }
                }
                "--sherpa-lib" => config.sherpa_lib = PathBuf::from(value),
                "--sensevoice-model" => config.sensevoice_model = PathBuf::from(value),
                "--sensevoice-tokens" => config.sensevoice_tokens = PathBuf::from(value),
                "--sensevoice-vad" => config.sensevoice_vad = PathBuf::from(value),
                "--sensevoice-language" => config.sensevoice_language = value,
                "--model" => config.model = PathBuf::from(value),
                "--vad-model" => config.vad_model = PathBuf::from(value),
                "--runtime" => config.runtime = PathBuf::from(value),
                "--pending-audio-budget-mib" => {
                    config.storage.pending_audio_budget_bytes =
                        parse_mib("--pending-audio-budget-mib", &value)?;
                }
                "--pending-audio-warn-mib" => {
                    config.storage.warning_at_bytes =
                        Some(parse_mib("--pending-audio-warn-mib", &value)?);
                }
                "--free-disk-floor-mib" => {
                    config.storage.free_disk_floor_bytes =
                        parse_mib("--free-disk-floor-mib", &value)?;
                }
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
        if explicit_cloud_trial && explicit_engine {
            return Err(
                "--cloud-trial cannot be combined with --engine; cloud trial has no local fallback"
                    .into(),
            );
        }
        // An explicit engine is the local fallback; the cloud default is not one.
        if explicit_engine {
            config.cloud_trial = false;
        }
        // Only an explicit --glossary is a process-wide override. Without one the
        // application resolves the runtime copy and the copy next to listen.exe.
        config.glossary = glossary;
        // Resolve once so changing the foreground application cannot change file interpretation.
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        for path in [
            &mut config.engine,
            &mut config.model,
            &mut config.vad_model,
            &mut config.runtime,
            &mut config.sherpa_lib,
            &mut config.sensevoice_model,
            &mut config.sensevoice_tokens,
            &mut config.sensevoice_vad,
        ] {
            if !path.is_absolute() {
                *path = cwd.join(&*path);
            }
        }
        if let Some(glossary) = config.glossary.as_mut() {
            if !glossary.is_absolute() {
                *glossary = cwd.join(&*glossary);
            }
        }
        Ok(LaunchAction::Run(config))
    }

    /// The one recognition decision this configuration makes.
    pub fn recognition(&self) -> Recognition {
        if self.cloud_trial {
            Recognition::CloudTrial
        } else {
            Recognition::Local(self.engine_kind)
        }
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Whether this process installs a notification-area icon. Only `--no-tray`
    /// turns it off; without the flag the tray icon is created exactly as before.
    pub fn tray(&self) -> bool {
        !self.no_tray
    }

    /// Whether transient phase notifications may be shown. There is nowhere to
    /// show them without the icon, and `--hidden` suppresses them as before.
    pub fn notify(&self) -> bool {
        self.tray() && !self.hidden
    }

    pub fn glossary(&self) -> Option<&Path> {
        self.glossary.as_deref()
    }

    /// Build the application this configuration describes. The cloud default needs
    /// the machine-local credential, workspace and fixed model/region; if they are
    /// unusable, construction reports the reason and no local adapter is attached.
    pub fn application(&self, registration: AppResult<()>) -> AppResult<Application> {
        match self.recognition() {
            Recognition::CloudTrial => {
                let sessions = CloudSessions::from_local(Arc::new(WasapiPcmPlayback))?;
                Ok(self.application_with_sessions(Box::new(sessions), registration))
            }
            Recognition::Local(_) => {
                self.local_application(Arc::new(WasapiPlayback), self.transcriber(), registration)
            }
        }
    }

    /// The application constructor seam. Production passes the configured adapter;
    /// tests pass a controlled one. The runtime, storage policy and persistence are
    /// identical for both recognition paths.
    pub fn application_with_sessions(
        &self,
        sessions: Box<dyn RecognitionSessions>,
        registration: AppResult<()>,
    ) -> Application {
        Application::with_recognition_sessions(sessions, self.runtime.clone(), registration)
            .with_storage_policy(self.storage)
    }

    /// Build a local application from an injected device and transcriber. This
    /// refuses to run when the configuration selected the cloud default, so no
    /// caller can silently substitute local recognition for the cloud path.
    pub fn local_application(
        &self,
        audio: Arc<dyn PlaybackAudio>,
        transcriber: Arc<dyn LocalTranscriber>,
        registration: AppResult<()>,
    ) -> AppResult<Application> {
        match self.recognition() {
            Recognition::CloudTrial => Err(
                "The configured default is the Alibaba cloud trial; local recognition is an explicit fallback, not a silent substitute. Start with --engine to select the local path."
                    .into(),
            ),
            Recognition::Local(_) => Ok(self.application_with_sessions(
                Box::new(LocalSessions::new(audio, transcriber)),
                registration,
            )),
        }
    }

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

/// Resolve the checkout or unpacked package root the same way the GUI resolves its
/// default runtime: the nearest ancestor that owns `.tools`, else the working
/// directory.
pub fn root() -> Result<PathBuf, String> {
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    Ok(executable
        .ancestors()
        .find(|path| path.join(".tools").is_dir())
        .map(PathBuf::from)
        .unwrap_or(env::current_dir().map_err(|error| error.to_string())?))
}

/// Parse one explicit MiB option for the storage bounds (#36). The byte value is
/// passed through exactly; only the CLI spelling uses MiB.
fn parse_mib(option: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map(crate::storage::StoragePolicy::mib)
        .map_err(|_| format!("{option} must be a whole number of MiB"))
}

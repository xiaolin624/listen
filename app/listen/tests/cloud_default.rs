//! The accepted cloud path is the daily default (#40); local recognition is an
//! explicit fallback. These tests drive the production launch configuration and the
//! same `Application::dispatch` / `poll` / `snapshot` seam as the rest of the suite.
//! They never read a real credential, open a playback device or connect.

use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Preparation, Recording, Target, TargetSelection,
    cloud::{
        ALI_MODEL, ALI_REGION, AliSettings, CloudError, CloudSessions, CredentialSource,
        MessageSocket, MessageTransport, Secret, TrialLedger,
    },
    launch::{Config, Engine, LaunchAction, Recognition},
    recognition::SessionState,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        // The wall clock alone is not unique across parallel tests.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-cloud-default-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Parse the shipped launch configuration. `--runtime` pins every write to this
/// test's directory; it must not change which recognition path is selected.
fn launch_config(args: &[&str], runtime: &Runtime) -> Config {
    let mut args: Vec<String> = args
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    args.push("--runtime".into());
    args.push(runtime.0.display().to_string());
    match Config::parse(&runtime.0, args) {
        Ok(LaunchAction::Run(config)) => config,
        other => panic!("expected one run configuration: {other:?}"),
    }
}

/// Records whether the capture device was opened. The cloud default must report
/// its configuration problem before this boundary is reached.
struct NoCapture(Arc<AtomicBool>);
impl PlaybackAudio for NoCapture {
    fn start(&self, _directory: &Path) -> AppResult<Box<dyn Recording>> {
        self.0.store(true, Ordering::SeqCst);
        Err("capture must not start for an unusable cloud default".into())
    }
}

/// The network boundary. A cloud configuration is validated before connecting.
struct NeverConnect;
impl MessageTransport for NeverConnect {
    fn requires_live_cost_check(&self) -> bool {
        // The native Message transport verifies the live free-only policy after
        // capture starts. Construction and preparation must not connect.
        true
    }
    fn connect(
        &self,
        _settings: &AliSettings,
        _key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        panic!("the cloud default must be prepared before any connection")
    }
}

struct FixedKey(&'static str);
impl CredentialSource for FixedKey {
    fn load(&self) -> Result<Secret, CloudError> {
        Secret::new(self.0.to_owned())
    }
}

struct MissingKey;
impl CredentialSource for MissingKey {
    fn load(&self) -> Result<Secret, CloudError> {
        Err(CloudError::MissingKey)
    }
}

/// The target editor the refused cloud default must never write to.
#[derive(Default)]
struct Editor(Mutex<String>);
struct Selection(Arc<Editor>);
struct BoundEditor(Arc<Editor>);
impl TargetSelection for Selection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(BoundEditor(self.0.clone()))))
    }
}
impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.0.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut draft = self.0.0.lock().unwrap();
        if *draft != expected {
            return Err("draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

fn cloud_application(
    config: &Config,
    runtime: &Runtime,
    credentials: Arc<dyn CredentialSource>,
    settings: AliSettings,
    capture_opened: Arc<AtomicBool>,
) -> Application {
    let sessions = CloudSessions::new(
        Arc::new(NoCapture(capture_opened)),
        Arc::new(NeverConnect),
        credentials,
        settings,
        TrialLedger::new(runtime.0.join("ledger")),
    );
    config.application_with_sessions(Box::new(sessions), Ok(()))
}

const LOCAL_TEXT: &str = "本地识别的一段完整结果。\nLocal fallback, review before sending.";

/// A playback device that records a short valid WAV, like the existing local
/// end-to-end tests. It is only reached by the explicit local path.
struct Playback;
struct Playing(PathBuf);
impl PlaybackAudio for Playback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let path = directory.join("audio.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for _ in 0..160 {
            writer.write_sample(1234_i16).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
        Ok(Box::new(Playing(path)))
    }
}
impl Recording for Playing {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        Ok(CapturedAudio {
            path: self.0,
            description: "controlled local render device".into(),
        })
    }
}

/// The local recognition engine reports its own backend and transcribes the
/// recorded audio; the cloud attempt in the same test file never reaches it.
struct LocalModel;
impl LocalTranscriber for LocalModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn prepare(&self) -> Option<std::sync::mpsc::Receiver<AppResult<String>>> {
        None
    }
    fn backend_note(&self) -> Option<String> {
        Some("Local test engine (explicit --engine selection; no cloud service used)".into())
    }
    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        if wav.duration() == 0 {
            return Err("no playback audio reached the local engine".into());
        }
        Ok(LOCAL_TEXT.into())
    }
}

fn await_phase(app: &mut Application, phase: Phase) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if state.phase == phase {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "expected {phase:?}, observed {state:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_default_configuration_selects_the_accepted_cloud_trial_and_reports_its_source() {
    let runtime = Runtime::new();
    let config = launch_config(&[], &runtime);
    assert_eq!(
        config.recognition(),
        Recognition::CloudTrial,
        "the shipped configuration must select the accepted cloud path"
    );

    let capture_opened = Arc::new(AtomicBool::new(false));
    let app = cloud_application(
        &config,
        &runtime,
        Arc::new(FixedKey("CLOUD-DEFAULT-KEY-7C21")),
        AliSettings::beijing("cloud-default-workspace".into()),
        capture_opened.clone(),
    );
    let state = app.snapshot();
    assert_eq!(state.phase, Phase::Ready, "{state:?}");
    assert_eq!(state.preparation, Preparation::Ready, "{state:?}");
    assert!(state.cloud_trial(), "{state:?}");
    let backend = state
        .backend
        .as_deref()
        .expect("the cloud default must report its actual source");
    for expected in ["Alibaba Model Studio", ALI_MODEL, ALI_REGION] {
        assert!(backend.contains(expected), "{backend}");
    }
    assert!(
        !backend.to_ascii_lowercase().contains("two-provider"),
        "{backend}"
    );
    assert!(
        state.usage.is_some(),
        "a cloud default must report its own usage ledger"
    );
    assert!(
        !capture_opened.load(Ordering::SeqCst),
        "preparing the default must not open a capture device"
    );
}

#[test]
fn an_unusable_cloud_default_refuses_before_capture_and_never_delivers_local_text() {
    const PREVIOUS: &str = "previous complete result";
    let key = "CLOUD-DEFAULT-KEY-7C21";
    let scenarios: Vec<(Arc<dyn CredentialSource>, AliSettings, &str)> = vec![
        (
            Arc::new(MissingKey),
            AliSettings::beijing("cloud-default-workspace".into()),
            "No Alibaba cloud API key",
        ),
        (
            Arc::new(FixedKey("bad key")),
            AliSettings::beijing("cloud-default-workspace".into()),
            "invalid format",
        ),
        (
            Arc::new(FixedKey(key)),
            AliSettings::beijing(String::new()),
            "valid Alibaba workspace",
        ),
    ];
    for (credentials, settings, expected) in scenarios {
        let runtime = Runtime::new();
        fs::write(runtime.0.join("current-result.txt"), PREVIOUS).unwrap();
        let config = launch_config(&[], &runtime);
        assert_eq!(config.recognition(), Recognition::CloudTrial);

        let capture_opened = Arc::new(AtomicBool::new(false));
        let editor = Arc::new(Editor(Mutex::new("protected draft".into())));
        let mut app = cloud_application(
            &config,
            &runtime,
            credentials,
            settings,
            capture_opened.clone(),
        )
        .with_targets(Arc::new(Selection(editor.clone())));

        let before = app.snapshot();
        assert_eq!(before.phase, Phase::Failure, "{before:?}");
        assert!(
            before.detail.contains(expected),
            "expected {expected:?} in {:?}",
            before.detail
        );
        assert!(
            before
                .backend
                .as_deref()
                .is_some_and(|backend| backend.starts_with("Cloud trial:")),
            "the refused path must still report the cloud source: {before:?}"
        );
        assert_eq!(before.text, PREVIOUS);

        app.dispatch(Control::Start);
        app.poll();
        let after = app.snapshot();
        assert_eq!(after.phase, Phase::Failure, "{after:?}");
        assert_eq!(after.preparation, Preparation::Failed, "{after:?}");
        assert_eq!(after.recognition, Some(SessionState::Failed), "{after:?}");
        assert!(
            after.detail.contains(expected),
            "the refusal must be actionable, got {:?}",
            after.detail
        );
        assert_eq!(after.text, PREVIOUS, "no local text may replace the result");
        assert!(
            !capture_opened.load(Ordering::SeqCst),
            "a refused cloud default must not open a capture device"
        );
        assert_eq!(
            *editor.0.lock().unwrap(),
            "protected draft",
            "a refused cloud default must not write to any target"
        );
        assert_eq!(
            fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
            PREVIOUS,
            "a refused cloud default must not save local recognition output"
        );
        assert!(
            !fs::read_dir(&runtime.0).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("session-")),
            "a refused cloud default must not create a capture segment"
        );
        assert!(
            after
                .backend
                .as_deref()
                .is_some_and(|backend| backend.starts_with("Cloud trial:")),
            "no local backend may be substituted: {after:?}"
        );
        assert!(
            after.usage.is_some(),
            "the cloud adapter must stay the recognition path: {after:?}"
        );

        // A stop after the refusal is a no-op, not an implicit local run.
        app.dispatch(Control::Stop);
        app.poll();
        assert_eq!(app.snapshot().phase, Phase::Failure);
        assert_eq!(*editor.0.lock().unwrap(), "protected draft");
    }
}

#[test]
fn explicit_local_selection_runs_the_local_path_end_to_end_and_reports_it() {
    let runtime = Runtime::new();
    let config = launch_config(&["--engine", "sensevoice"], &runtime);
    assert_eq!(
        config.recognition(),
        Recognition::Local(Engine::SenseVoice),
        "an explicit --engine must select local recognition"
    );
    // The default may not be turned into a local application by falling through:
    // selecting local is an explicit act.
    let cloud = launch_config(&[], &runtime);
    assert!(
        cloud
            .local_application(Arc::new(Playback), Arc::new(LocalModel), Ok(()))
            .is_err(),
        "a cloud-default configuration must not build a silent local application"
    );

    let editor = Arc::new(Editor(Mutex::new("draft".into())));
    let mut app = config
        .local_application(Arc::new(Playback), Arc::new(LocalModel), Ok(()))
        .expect("an explicit local configuration must build the local application")
        .with_targets(Arc::new(Selection(editor.clone())));
    let ready = app.snapshot();
    assert!(!ready.cloud_trial(), "{ready:?}");
    assert!(
        ready
            .backend
            .as_deref()
            .is_some_and(|backend| backend.contains("Local test engine")),
        "local selection must report the local backend: {ready:?}"
    );
    assert!(
        ready.usage.is_none(),
        "a local selection claims no cloud usage ledger: {ready:?}"
    );

    app.dispatch(Control::Start);
    await_phase(&mut app, Phase::Capturing);
    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.text, LOCAL_TEXT);
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert!(!result.cloud_trial(), "{result:?}");
    assert_eq!(*editor.0.lock().unwrap(), format!("draft\n{LOCAL_TEXT}"));
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        LOCAL_TEXT
    );
}

#[test]
fn cloud_trial_and_an_explicit_engine_are_refused_in_both_directions() {
    let runtime = Runtime::new();
    assert_eq!(
        launch_config(&["--cloud-trial", "ali"], &runtime).recognition(),
        Recognition::CloudTrial,
        "an explicit cloud selection is still the cloud path"
    );
    for args in [
        ["--cloud-trial", "ali", "--engine", "whisper"],
        ["--engine", "whisper", "--cloud-trial", "ali"],
        ["--cloud-trial", "ali", "--engine", "sensevoice"],
        ["--engine", "sensevoice", "--cloud-trial", "ali"],
    ] {
        let mut parsed: Vec<String> = args
            .iter()
            .map(|argument| (*argument).to_string())
            .collect();
        parsed.push("--runtime".into());
        parsed.push(runtime.0.display().to_string());
        match Config::parse(&runtime.0, parsed) {
            Err(message) => {
                assert!(
                    message.contains("--cloud-trial cannot be combined with --engine"),
                    "{message}"
                );
                assert!(
                    message.contains("no local fallback"),
                    "the refusal must explain that there is no automatic fallback: {message}"
                );
            }
            Ok(other) => panic!("cloud trial plus --engine must be refused, got {other:?}"),
        }
    }
}

use listen::{
    AppResult, Application, CapturedAudio, Control, LocalTranscriber, Phase, PlaybackAudio,
    Recording,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "listen-flow-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
            description: "test render device".into(),
        })
    }
}
struct LocalModel;
impl LocalTranscriber for LocalModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        if wav.duration() == 0 {
            return Err("no playback audio reached the local model".into());
        }
        Ok("第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀".into())
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
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn hotkey_start_stop_delivers_complete_copyable_current_result_and_removes_temporary_audio() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    await_phase(&mut app, Phase::Capturing);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert!(
        result.retained_audio.is_empty(),
        "successful audio must be cleaned"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        result.text
    );
    assert!(
        !fs::read_dir(&runtime.0)
            .unwrap()
            .any(|entry| entry.unwrap().path().is_dir()),
        "success must leave no session audio directory"
    );
}

struct FailedModel;
impl LocalTranscriber for FailedModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Err("whisper.cpp process could not start: missing runtime DLL".into())
    }
}

#[test]
fn local_backend_failure_keeps_recoverable_audio_and_explains_failure() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(FailedModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    await_phase(&mut app, Phase::Capturing);
    app.dispatch(Control::Toggle);
    let failure = await_phase(&mut app, Phase::Failure);
    assert!(
        failure.detail.contains("missing runtime DLL"),
        "{failure:?}"
    );
    assert_eq!(
        failure.retained_audio.len(),
        1,
        "failure must expose the recoverable audio"
    );
    assert!(
        hound::WavReader::open(&failure.retained_audio[0])
            .unwrap()
            .duration()
            > 0
    );
    assert!(failure.text.is_empty());
    app.dispatch(Control::Stop);
    assert_eq!(
        app.snapshot().retained_audio,
        failure.retained_audio,
        "repeated stop must not lose recoverable audio"
    );
}

#[test]
fn failed_hotkey_registration_is_visible_and_cannot_claim_to_capture() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Err("Ctrl+Alt+R is already registered by another application (1409)".into()),
    );
    let initial = app.snapshot();
    assert_eq!(initial.phase, Phase::Failure);
    assert!(initial.detail.contains("1409"));
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Failure);
    assert_eq!(fs::read_dir(&runtime.0).unwrap().count(), 0);
}

struct SlowModel;
impl LocalTranscriber for SlowModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String> {
        thread::sleep(Duration::from_millis(100));
        LocalModel.transcribe(audio, directory)
    }
}

#[test]
fn busy_operations_explain_the_current_work_and_do_not_replace_or_duplicate_the_result() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(SlowModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    await_phase(&mut app, Phase::Capturing);
    app.dispatch(Control::Start);
    assert!(app.snapshot().detail.contains("Already capturing"));
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Processing);
    app.dispatch(Control::Toggle);
    assert!(app.snapshot().detail.contains("Still processing"));
    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Result);
    assert_eq!(app.snapshot().text, result.text);
}

struct MissingModel;
impl LocalTranscriber for MissingModel {
    fn check_ready(&self) -> AppResult<()> {
        Err("Local model is missing: ggml-base.bin".into())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        panic!("an unavailable model must never be used")
    }
}

#[test]
fn unavailable_local_backend_is_visible_before_start_and_never_claims_capture_started() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(MissingModel),
        runtime.0.clone(),
        Ok(()),
    );
    assert_eq!(app.snapshot().phase, Phase::Failure);
    assert!(app.snapshot().detail.contains("model is missing"));
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Failure);
    assert_eq!(fs::read_dir(&runtime.0).unwrap().count(), 0);
}

struct DisconnectedPlayback;
struct DisconnectedRecording(Box<dyn Recording>);
impl PlaybackAudio for DisconnectedPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        Ok(Box::new(DisconnectedRecording(Playback.start(directory)?)))
    }
}
impl Recording for DisconnectedRecording {
    fn failure(&self) -> Option<String> {
        Some("Playback device disconnected".into())
    }
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        self.0.finish()?;
        Err("Playback device disconnected; this recording may be incomplete".into())
    }
}

#[test]
fn interrupted_capture_automatically_reports_failure_and_retains_the_partial_audio() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(DisconnectedPlayback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    let failure = await_phase(&mut app, Phase::Failure);
    assert!(failure.detail.contains("disconnected"));
    assert_eq!(failure.retained_audio.len(), 1);
    assert!(failure.retained_audio[0].is_file());
    assert!(
        failure.text.is_empty(),
        "interrupted playback must not be presented as a complete transcript"
    );
}

struct MissingPlayback;
impl PlaybackAudio for MissingPlayback {
    fn start(&self, _directory: &Path) -> AppResult<Box<dyn Recording>> {
        Err("Cannot open playback endpoint: AUDCLNT_E_DEVICE_INVALIDATED".into())
    }
}

#[test]
fn playback_open_error_never_claims_to_capture_and_preserves_the_device_error() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(MissingPlayback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    let failure = app.snapshot();
    assert_eq!(failure.phase, Phase::Failure);
    assert!(failure.detail.contains("AUDCLNT_E_DEVICE_INVALIDATED"));
    assert!(failure.text.is_empty());
    assert!(
        failure.retained_audio.is_empty(),
        "no audio was opened, so do not invent recovery files"
    );
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Failure);
}

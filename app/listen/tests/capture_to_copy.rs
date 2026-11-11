use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Recording, Target, TargetSelection,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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

struct Editors {
    focused: Option<usize>,
    drafts: [String; 2],
    available: [bool; 2],
    unconfirmed: bool,
}

struct Desktop(Arc<Mutex<Editors>>);
struct BoundEditor {
    editors: Arc<Mutex<Editors>>,
    selected: usize,
}

impl TargetSelection for Desktop {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(self.0.lock().unwrap().focused.map(|selected| {
            Box::new(BoundEditor {
                editors: self.0.clone(),
                selected,
            }) as Box<dyn Target>
        }))
    }
}

impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        let desktop = self.editors.lock().unwrap();
        if !desktop.available[self.selected] {
            return Err("The original target was destroyed".into());
        }
        Ok(desktop.drafts[self.selected].clone())
    }

    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut desktop = self.editors.lock().unwrap();
        if !desktop.available[self.selected] {
            return Err("The original target was destroyed".into());
        }
        if desktop.drafts[self.selected] != expected {
            return Err("The draft changed before output".into());
        }
        desktop.drafts[self.selected] = replacement.into();
        if desktop.unconfirmed {
            return Err("The complete target output could not be confirmed".into());
        }
        Ok(())
    }
}

#[test]
fn hotkey_binds_original_editor_and_appends_complete_result_to_its_latest_draft() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors {
        focused: Some(0),
        drafts: ["原有草稿".into(), "另一个输入框".into()],
        available: [true; 2],
        unconfirmed: false,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    {
        let mut desktop = editors.lock().unwrap();
        desktop.drafts[0] = "原有草稿\n采集中新增 Rust 123".into();
        desktop.focused = Some(1);
    }
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    let desktop = editors.lock().unwrap();
    assert_eq!(
        desktop.drafts[0],
        "原有草稿\n采集中新增 Rust 123\n第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert_eq!(desktop.drafts[1], "另一个输入框");
    assert!(result.detail.contains("appended"), "{result:?}");
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
}

#[test]
fn no_target_at_start_keeps_the_complete_fallback_and_does_not_retarget_later_focus() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors {
        focused: None,
        drafts: ["原有草稿".into(), "后来获得焦点".into()],
        available: [true; 2],
        unconfirmed: false,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    editors.lock().unwrap().focused = Some(1);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Fallback));
    assert!(result.detail.contains("No supported editable target"));
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        result.text
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(desktop.drafts, ["原有草稿", "后来获得焦点"]);
    assert!(result.retained_audio.is_empty());
}

#[test]
fn destroyed_original_target_keeps_the_full_result_and_never_uses_a_replacement() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors {
        focused: Some(0),
        drafts: ["原有草稿".into(), "旁边输入框".into()],
        available: [true; 2],
        unconfirmed: false,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    {
        let mut desktop = editors.lock().unwrap();
        desktop.available[0] = false;
        desktop.drafts[0] = "重建的相似输入框".into();
        desktop.focused = Some(1);
    }
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Fallback));
    assert!(result.detail.contains("original target was destroyed"));
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(desktop.drafts, ["重建的相似输入框", "旁边输入框"]);
}

#[test]
fn unconfirmed_output_keeps_the_full_fallback_and_repeated_controls_never_retry_the_append() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors {
        focused: Some(0),
        drafts: ["原有草稿".into(), "旁边输入框".into()],
        available: [true; 2],
        unconfirmed: true,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(SlowModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    editors.lock().unwrap().focused = Some(1);
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    app.dispatch(Control::Stop);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    app.dispatch(Control::Stop);
    app.poll();
    assert_eq!(result.delivery, Some(Delivery::Fallback));
    assert!(result.detail.contains("could not be confirmed"));
    assert!(result.detail.contains("Check the original target"));
    assert!(result.detail.contains("not retried automatically"));
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        result.text
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(
        desktop.drafts[0],
        "原有草稿\n第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert_eq!(desktop.drafts[1], "旁边输入框");
}

#[test]
fn result_storage_failure_keeps_the_complete_text_and_audio_without_writing_the_target() {
    let runtime = Runtime::new();
    fs::create_dir(runtime.0.join("current-result.txt")).unwrap();
    let editors = Arc::new(Mutex::new(Editors {
        focused: Some(0),
        drafts: ["原有草稿".into(), "旁边输入框".into()],
        available: [true; 2],
        unconfirmed: false,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Failure);
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    assert!(result.detail.contains("Could not save the current result"));
    assert_eq!(result.retained_audio.len(), 1);
    assert!(result.retained_audio[0].is_file());
    assert_eq!(editors.lock().unwrap().drafts, ["原有草稿", "旁边输入框"]);
}

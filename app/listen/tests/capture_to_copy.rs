use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Recording, Target, TargetSelection,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TRANSCRIPT: &str = "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        // The wall clock alone is not unique: parallel tests can read the same
        // tick, and one Runtime::drop would then delete another test's directory.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-flow-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

struct NoSpeechModel;
impl LocalTranscriber for NoSpeechModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok(" \r\n\t".into())
    }
}

#[test]
fn no_valid_speech_reports_no_speech_and_keeps_the_previous_copyable_result() {
    let runtime = Runtime::new();
    let previous = "原来的完整转写。\nReview the API response before sending.";
    fs::write(runtime.0.join("current-result.txt"), previous).unwrap();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(NoSpeechModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.snapshot().phase == Phase::Processing {
        assert!(Instant::now() < deadline);
        app.poll();
        thread::sleep(Duration::from_millis(5));
    }
    let result = app.snapshot();
    assert_eq!(result.phase, Phase::NoSpeech);
    assert!(result.detail.contains("No valid speech"), "{result:?}");
    assert_eq!(result.text, previous);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        previous
    );
    assert!(result.retained_audio.is_empty());
    assert!(
        !fs::read_dir(&runtime.0)
            .unwrap()
            .any(|entry| entry.unwrap().path().is_dir()),
        "successful no-speech recognition must clean its audio"
    );
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().text, previous);
}

#[test]
fn no_speech_without_a_previous_result_leaves_the_fallback_empty_and_allows_another_capture() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(NoSpeechModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::NoSpeech);
    assert!(result.text.is_empty());
    assert!(!runtime.0.join("current-result.txt").exists());
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    app.dispatch(Control::Toggle);
    assert!(await_phase(&mut app, Phase::NoSpeech).text.is_empty());
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

/// A test editor is either writable, or it accepts reads but refuses writes the way
/// a real UIA editor reports its own state. A capacity limit stores only a prefix of
/// the requested value, and a concurrent edit is applied by the operator just before
/// the tool's replace call, after the draft was already read.
#[derive(Clone, Copy)]
enum Blocked {
    ReadOnly,
    Disabled,
}

impl Blocked {
    fn error(self) -> &'static str {
        match self {
            Blocked::ReadOnly => "The original target is read-only",
            Blocked::Disabled => "The original target is disabled",
        }
    }
}

struct Editors {
    focused: Option<usize>,
    drafts: [String; 2],
    available: [bool; 2],
    blocked: [Option<Blocked>; 2],
    limit: [Option<usize>; 2],
    concurrent_edit: Option<String>,
    unconfirmed: bool,
    replace_calls: usize,
}

impl Editors {
    fn with_drafts(first: &str, second: &str) -> Self {
        Self {
            focused: Some(0),
            drafts: [first.into(), second.into()],
            available: [true; 2],
            blocked: [None; 2],
            limit: [None; 2],
            concurrent_edit: None,
            unconfirmed: false,
            replace_calls: 0,
        }
    }
}

struct Desktop(Arc<Mutex<Editors>>);
struct BoundEditor {
    editors: Arc<Mutex<Editors>>,
    selected: usize,
}

impl TargetSelection for Desktop {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let desktop = self.0.lock().unwrap();
        let Some(selected) = desktop.focused else {
            return Ok(None);
        };
        if !desktop.available[selected] {
            return Err("The original target was destroyed before binding".into());
        }
        if let Some(blocked) = desktop.blocked[selected] {
            return Err(blocked.error().into());
        }
        Ok(Some(Box::new(BoundEditor {
            editors: self.0.clone(),
            selected,
        })))
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
        desktop.replace_calls += 1;
        if !desktop.available[self.selected] {
            return Err("The original target was destroyed".into());
        }
        if let Some(blocked) = desktop.blocked[self.selected] {
            return Err(blocked.error().into());
        }
        if let Some(edit) = desktop.concurrent_edit.take() {
            desktop.drafts[self.selected].push_str(&edit);
        }
        if desktop.drafts[self.selected] != expected {
            return Err(
                "The target draft changed during output; no replacement was attempted".into(),
            );
        }
        desktop.drafts[self.selected] = match desktop.limit[self.selected] {
            Some(limit) => replacement.chars().take(limit).collect(),
            None => replacement.into(),
        };
        if desktop.limit[self.selected].is_some() || desktop.unconfirmed {
            return Err("The complete target output could not be confirmed".into());
        }
        Ok(())
    }
}

#[test]
fn hotkey_binds_original_editor_and_appends_complete_result_to_its_latest_draft() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "另一个输入框")));
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
fn no_speech_never_appends_empty_lines_or_generated_text_to_a_bound_target() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "另一个输入框")));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(NoSpeechModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));
    app.dispatch(Control::Start);
    editors.lock().unwrap().focused = Some(1);
    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::NoSpeech);
    assert_eq!(editors.lock().unwrap().drafts, ["原有草稿", "另一个输入框"]);
    assert_eq!(result.delivery, None);
    assert!(result.text.is_empty());
}

#[test]
fn no_target_at_start_keeps_the_complete_fallback_and_does_not_retarget_later_focus() {
    let runtime = Runtime::new();
    let mut editors = Editors::with_drafts("原有草稿", "后来获得焦点");
    editors.focused = None;
    let editors = Arc::new(Mutex::new(editors));
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
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
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
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
    editors.lock().unwrap().unconfirmed = true;
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
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
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

struct NumberedModel(AtomicUsize);
impl LocalTranscriber for NumberedModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        if wav.duration() == 0 {
            return Err("no playback audio reached the local model".into());
        }
        let index = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(format!(
            "第 {index} 段完整转写：Rust 和 WASAPI。\nSecond line {index}. 🦀"
        ))
    }
}

#[test]
fn read_only_target_during_capture_keeps_the_complete_fallback_without_writing_it() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
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
        desktop.blocked[0] = Some(Blocked::ReadOnly);
        desktop.focused = Some(1);
    }
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    assert!(result.detail.contains("read-only"), "{result:?}");
    assert!(result.detail.contains("nothing was written"), "{result:?}");
    assert_eq!(result.text, TRANSCRIPT);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    {
        let desktop = editors.lock().unwrap();
        assert_eq!(desktop.drafts, ["原有草稿", "旁边输入框"]);
        assert_eq!(desktop.replace_calls, 1, "a refused write is never retried");
    }

    app.dispatch(Control::Stop);
    app.poll();
    assert_eq!(
        app.snapshot().text,
        TRANSCRIPT,
        "the fallback stays copyable"
    );
    assert_eq!(editors.lock().unwrap().drafts, ["原有草稿", "旁边输入框"]);
}

#[test]
fn disabled_target_at_start_is_reported_without_writing_any_control() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
    editors.lock().unwrap().blocked[0] = Some(Blocked::Disabled);
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    editors.lock().unwrap().focused = Some(1);
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    assert!(result.detail.contains("disabled"), "{result:?}");
    assert!(
        result.detail.contains("No text was written to any control"),
        "{result:?}"
    );
    assert_eq!(result.text, TRANSCRIPT);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(desktop.drafts, ["原有草稿", "旁边输入框"]);
    assert_eq!(
        desktop.replace_calls, 0,
        "a disabled target is never written"
    );
}

#[test]
fn capacity_limited_target_is_reported_as_truncated_and_keeps_the_complete_fallback() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
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
        desktop.limit[0] = Some(30);
        desktop.focused = Some(1);
    }
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    let intended = format!("原有草稿\n{TRANSCRIPT}");
    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    assert!(
        !result.detail.contains("The complete result was appended"),
        "a truncated write must never be reported as complete success: {result:?}"
    );
    assert!(result.detail.contains("truncated"), "{result:?}");
    assert!(
        result.detail.contains("not a complete delivery"),
        "{result:?}"
    );
    assert!(
        result.detail.contains(&format!(
            "only 30 of {} characters",
            intended.chars().count()
        )),
        "{result:?}"
    );
    assert!(
        result.detail.contains("Check the original target"),
        "{result:?}"
    );
    assert!(
        result.detail.contains("not retried automatically"),
        "{result:?}"
    );
    assert_eq!(result.text, TRANSCRIPT);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(
        desktop.drafts[0],
        intended.chars().take(30).collect::<String>(),
        "the target keeps only the truncated prefix it accepted"
    );
    assert_eq!(desktop.drafts[1], "旁边输入框");
    assert_eq!(desktop.replace_calls, 1);
}

#[test]
fn concurrent_target_edit_during_output_is_reported_unconfirmed_and_never_reverted() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "旁边输入框")));
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
        desktop.concurrent_edit = Some("用户同时输入".into());
        desktop.focused = Some(1);
    }
    app.dispatch(Control::Toggle);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    assert!(
        result.detail.contains("could not be confirmed"),
        "{result:?}"
    );
    assert!(
        result.detail.contains("may be partially written"),
        "{result:?}"
    );
    assert!(
        result.detail.contains("Check the original target"),
        "{result:?}"
    );
    assert!(
        result.detail.contains("not retried automatically"),
        "{result:?}"
    );
    assert_eq!(result.text, TRANSCRIPT);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    let desktop = editors.lock().unwrap();
    assert_eq!(
        desktop.drafts[0], "原有草稿用户同时输入",
        "the operator's concurrent edit is kept and nothing is rolled back"
    );
    assert_eq!(desktop.drafts[1], "旁边输入框");
    assert_eq!(desktop.replace_calls, 1);
}

#[test]
fn repeated_stop_held_hotkey_and_processing_controls_deliver_each_result_once() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::with_drafts("原有草稿", "另一个输入框")));
    let model = Arc::new(NumberedModel(AtomicUsize::new(0)));
    let mut app = Application::new(Arc::new(Playback), model, runtime.0.clone(), Ok(()))
        .with_targets(Arc::new(Desktop(editors.clone())));

    // A held or auto-repeating hotkey turns into several events: only the first can
    // stop the capture; the rest arrive during local processing.
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    for _ in 0..4 {
        app.dispatch(Control::Toggle);
    }
    assert_eq!(app.snapshot().phase, Phase::Processing);
    assert!(app.snapshot().detail.contains("Still processing"));
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Processing);

    let first = await_phase(&mut app, Phase::Result);
    let once = editors.lock().unwrap().drafts[0].clone();
    assert_eq!(once, format!("原有草稿\n{}", first.text));
    assert_eq!(once.matches("Rust 和 WASAPI").count(), 1);
    assert_eq!(editors.lock().unwrap().replace_calls, 1);
    assert_eq!(first.delivery, Some(Delivery::Appended));

    // Lost key-up / repeated stop after the result must not append or re-enter.
    for _ in 0..3 {
        app.dispatch(Control::Stop);
    }
    app.poll();
    assert!(app.snapshot().detail.contains("Not capturing"));
    assert_eq!(app.snapshot().phase, Phase::Result);
    assert_eq!(app.snapshot().text, first.text);
    assert_eq!(editors.lock().unwrap().drafts[0], once);
    assert_eq!(editors.lock().unwrap().replace_calls, 1);

    // A later capture binds the focus at its own start; it never reuses or mixes
    // the previous target, result, or append.
    editors.lock().unwrap().focused = Some(1);
    app.dispatch(Control::Toggle);
    app.dispatch(Control::Toggle);
    let second = await_phase(&mut app, Phase::Result);
    assert_ne!(second.text, first.text);
    let desktop = editors.lock().unwrap();
    assert_eq!(desktop.drafts[0], once);
    assert_eq!(desktop.drafts[1], format!("另一个输入框\n{}", second.text));
    assert_eq!(desktop.replace_calls, 2);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        second.text
    );
}

/// An operator owns and focuses the fixture; the product receives no coordinates,
/// HWND, DOM selector, application name, or alternate output adapter.
#[cfg(windows)]
#[test]
#[ignore = "requires an operator-owned focused editor and LISTEN_EDITOR_HANDSHAKE"]
fn selected_generic_editor_receives_complete_output_through_the_real_target_adapter() {
    let handshake = PathBuf::from(
        std::env::var_os("LISTEN_EDITOR_HANDSHAKE")
            .expect("set LISTEN_EDITOR_HANDSHAKE to a fresh operator-owned directory"),
    );
    fs::create_dir_all(&handshake).unwrap();
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(listen::target::NativeTargets));
    fs::write(
        handshake.join("ready"),
        "Focus the owned editor, then create focused.",
    )
    .unwrap();
    wait_for_operator(&handshake.join("focused"));
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    fs::write(
        handshake.join("capturing"),
        "Edit the draft, switch focus, then create stop.",
    )
    .unwrap();
    wait_for_operator(&handshake.join("stop"));
    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::Result);
    fs::write(handshake.join("detail.txt"), &result.detail).unwrap();
    fs::write(handshake.join("transcript.txt"), &result.text).unwrap();
    fs::write(
        handshake.join("delivery.txt"),
        format!("{:?}", result.delivery),
    )
    .unwrap();
    assert_eq!(
        result.text,
        "第一行：Rust 和 WASAPI。\nSecond line, review before sending. 🦀"
    );
    let expected = match std::env::var("LISTEN_EDITOR_EXPECT_FALLBACK").as_deref() {
        Ok("1") => Delivery::Fallback,
        _ => Delivery::Appended,
    };
    assert_eq!(result.delivery, Some(expected), "{}", result.detail);
    assert!(result.retained_audio.is_empty());
    app.dispatch(Control::Stop);
    app.poll();
    assert_eq!(app.snapshot().text, result.text);
    fs::write(
        handshake.join("finished"),
        "Check the owned editor and send counters.",
    )
    .unwrap();
}

#[cfg(windows)]
fn wait_for_operator(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "operator marker missing: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

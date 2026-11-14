//! Application-boundary tests for 术语表 correction: the recognized text is
//! corrected before it is saved as the current result and delivered.

use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Recording, Target, TargetSelection, TimedText,
    streaming::{SegmentConfig, SegmentStream, SegmentedAudioWriter},
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

const RAW: &str = "We use the DeepSek harnessness API.";
const CORRECTED: &str = "We use the DeepSeek Harness API.";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        // The wall clock alone is not unique: parallel tests can read the same
        // tick, and one Runtime::drop would then delete another test's directory.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-glossary-{}-{}-{}",
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

    fn with_glossary(content: &str) -> Self {
        let runtime = Self::new();
        fs::write(runtime.0.join("glossary.txt"), content).unwrap();
        runtime
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
        write_test_wav(&path, 160)?;
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

fn write_test_wav(path: &Path, samples: usize) -> AppResult<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for _ in 0..samples {
        writer.write_sample(1234_i16).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// Fails the first `failures` recognition calls, then returns the measured
/// mis-recognized engine output.
struct Model {
    failures: usize,
    calls: AtomicUsize,
}
impl Model {
    fn new(failures: usize) -> Self {
        Self {
            failures,
            calls: AtomicUsize::new(0),
        }
    }
}
impl LocalTranscriber for Model {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.failures {
            Err("local recognition failed".into())
        } else {
            Ok(RAW.into())
        }
    }
}

/// A streaming transcriber: the streamed path must never fall back to the
/// whole-audio call.
struct StreamingModel;
impl LocalTranscriber for StreamingModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        panic!("a streamed capture must be recognized per window")
    }
    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        _directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        Ok(vec![TimedText {
            start_ms: 0,
            end_ms: wav.duration() as u64 * 1000 / wav.spec().sample_rate as u64,
            text: RAW.into(),
        }])
    }
}

struct StreamingPlayback;
struct StreamingRecording {
    writer: Option<SegmentedAudioWriter>,
    stream: Option<SegmentStream>,
}
impl PlaybackAudio for StreamingPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let config = SegmentConfig {
            window_samples: 4_000,
            overlap_samples: 0,
            max_pending_segments: 2,
            spool_limit_bytes: 1_000_000,
        };
        let (mut writer, stream) = SegmentedAudioWriter::new(directory, config)?;
        for _ in 0..4_000 {
            writer.push(1234)?;
        }
        Ok(Box::new(StreamingRecording {
            writer: Some(writer),
            stream: Some(stream),
        }))
    }
}
impl Recording for StreamingRecording {
    fn take_segments(&mut self) -> Option<SegmentStream> {
        self.stream.take()
    }
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.writer
            .take()
            .unwrap()
            .finish("streamed test render device".into())
    }
}

struct Editor {
    text: String,
    writes: usize,
}
struct Desktop(Arc<Mutex<Editor>>);
struct BoundEditor(Arc<Mutex<Editor>>);
impl TargetSelection for Desktop {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(BoundEditor(self.0.clone()))))
    }
}
impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.lock().unwrap().text.clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut editor = self.0.lock().unwrap();
        if editor.text != expected {
            return Err("The target draft changed during output".into());
        }
        editor.text = replacement.into();
        editor.writes += 1;
        Ok(())
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

fn capture(app: &mut Application) -> listen::Snapshot {
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    await_phase(app, Phase::Result)
}

#[test]
fn corrected_text_is_delivered_and_saved_as_the_current_result() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let editor = Arc::new(Mutex::new(Editor {
        text: "原有草稿".into(),
        writes: 0,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editor.clone())));

    let result = capture(&mut app);
    assert_eq!(result.text, CORRECTED);
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        CORRECTED
    );
    assert_eq!(
        editor.lock().unwrap().text,
        format!("原有草稿\n{CORRECTED}"),
        "the user must receive the corrected text, not the engine output"
    );
    assert!(
        !result.detail.contains("Glossary correction is unavailable"),
        "{result:?}"
    );
}

#[test]
fn editing_the_glossary_applies_to_the_next_capture_without_a_restart() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    )
    .with_glossary(runtime.0.join("glossary.txt"));

    assert_eq!(capture(&mut app).text, CORRECTED);
    fs::write(runtime.0.join("glossary.txt"), "DeepSeek\n").unwrap();
    assert_eq!(
        capture(&mut app).text,
        "We use the DeepSeek harnessness API.",
        "the new file content must be used by the next capture, without a restart"
    );
}

#[test]
fn editing_the_glossary_during_capture_keeps_this_sessions_terms_fixed() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    )
    .with_glossary(runtime.0.join("glossary.txt"));

    app.dispatch(Control::Start);
    fs::write(runtime.0.join("glossary.txt"), "DeepSeek\n").unwrap();
    app.dispatch(Control::Stop);
    assert_eq!(await_phase(&mut app, Phase::Result).text, CORRECTED);
    assert_eq!(
        capture(&mut app).text,
        "We use the DeepSeek harnessness API."
    );
}

#[test]
fn a_retry_of_retained_audio_is_corrected_too() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let model = Arc::new(Model::new(1));
    let editor = Arc::new(Mutex::new(Editor {
        text: "原有草稿".into(),
        writes: 0,
    }));
    let mut app = Application::new(Arc::new(Playback), model, runtime.0.clone(), Ok(()))
        .with_targets(Arc::new(Desktop(editor.clone())));

    app.dispatch(Control::Toggle);
    app.dispatch(Control::Toggle);
    let failure = await_phase(&mut app, Phase::Failure);
    assert!(failure.retry_available, "{failure:?}");

    app.dispatch(Control::Retry);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.text, CORRECTED, "{result:?}");
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        CORRECTED
    );
    assert_eq!(
        editor.lock().unwrap().text,
        format!("原有草稿\n{CORRECTED}")
    );
    assert_eq!(editor.lock().unwrap().writes, 1);
}

#[test]
fn streamed_recognition_is_corrected_on_the_final_merged_text() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let mut app = Application::new(
        Arc::new(StreamingPlayback),
        Arc::new(StreamingModel),
        runtime.0.clone(),
        Ok(()),
    );

    let result = capture(&mut app);
    assert_eq!(result.text, CORRECTED, "{result:?}");
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        CORRECTED
    );
}

#[test]
fn a_malformed_glossary_is_reported_without_losing_the_capture() {
    let runtime = Runtime::with_glossary("| no canonical term\n");
    let editor = Arc::new(Mutex::new(Editor {
        text: String::new(),
        writes: 0,
    }));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editor.clone())));

    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing);
    assert!(
        capturing
            .detail
            .contains("Glossary correction is unavailable"),
        "{capturing:?}"
    );
    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::Result);

    assert_eq!(result.phase, Phase::Result);
    assert_eq!(
        result.text, RAW,
        "a malformed glossary must not lose the capture or guess a correction"
    );
    assert!(
        result.detail.contains("Glossary correction is unavailable"),
        "{result:?}"
    );
    assert!(result.detail.contains("line 1"), "{result:?}");
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        RAW
    );
    assert_eq!(editor.lock().unwrap().text, RAW);
}

#[test]
fn a_missing_glossary_file_delivers_the_uncorrected_text_without_a_warning() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    );

    let result = capture(&mut app);
    assert_eq!(result.text, RAW);
    assert!(
        !result.detail.contains("Glossary correction is unavailable"),
        "{result:?}"
    );
}

#[test]
fn the_active_glossary_file_is_reported_for_status_txt() {
    let runtime = Runtime::with_glossary("DeepSeek Harness\n");
    let app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    );
    assert_eq!(
        app.glossary_status(),
        runtime.0.join("glossary.txt").display().to_string(),
        "status.txt must name the glossary that is actually used"
    );
}

#[test]
fn no_glossary_file_is_reported_as_none_and_still_delivers() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model::new(0)),
        runtime.0.clone(),
        Ok(()),
    );
    assert_eq!(app.glossary_status(), "none");

    let result = capture(&mut app);
    assert_eq!(result.text, RAW);
    assert!(
        !result.detail.contains("Glossary correction is unavailable"),
        "{result:?}"
    );
}

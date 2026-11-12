//! Capture may start while the local model is still being prepared. These tests
//! drive the same `Application` boundary the hotkey uses with fake playback and
//! fake local models: they assert only what the user can see (phase, preparation
//! state, detail text, delivered text, retained audio).
#![cfg(windows)]

use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Preparation, Recording, Target, TargetSelection, TimedText,
    streaming::{SegmentConfig, SegmentStream, SegmentedAudioWriter},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CAPTURE_SAMPLES: usize = 800;

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        // The wall clock alone is not unique: parallel tests can read the same
        // tick, and one Runtime::drop would then delete another test's directory.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-preparation-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
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

/// A playback device that starts writing immediately, like the real loopback.
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
        for _ in 0..CAPTURE_SAMPLES {
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
            description: "preparation test playback device".into(),
        })
    }
}

/// A local model whose first-use preparation is completed by the test. Until the
/// test reports the model ready, the model reports that recognition is not yet
/// possible, but `check_ready` (the static model/backend check) already passes so
/// capture is allowed to start, exactly like a model that is still loading.
struct PreparingModel {
    preparation: Mutex<Option<mpsc::Receiver<AppResult<String>>>>,
    ready: AtomicBool,
    recognition_delay_ms: u64,
}
impl PreparingModel {
    fn new(receiver: mpsc::Receiver<AppResult<String>>) -> Self {
        Self {
            preparation: Mutex::new(Some(receiver)),
            ready: AtomicBool::new(false),
            recognition_delay_ms: 0,
        }
    }
    fn with_recognition_delay(receiver: mpsc::Receiver<AppResult<String>>, delay_ms: u64) -> Self {
        Self {
            recognition_delay_ms: delay_ms,
            ..Self::new(receiver)
        }
    }
    fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }
}
impl LocalTranscriber for PreparingModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        self.preparation.lock().unwrap().take()
    }
    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err(
                "The local model is still being prepared, so it cannot recognize audio yet".into(),
            );
        }
        thread::sleep(Duration::from_millis(self.recognition_delay_ms));
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        let samples = wav.duration();
        if samples == 0 {
            return Err("no playback audio reached the local model".into());
        }
        Ok(format!("complete capture of {samples} samples"))
    }
}

fn wait(
    app: &mut Application,
    what: &str,
    condition: impl Fn(&listen::Snapshot) -> bool,
) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if condition(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "flow did not reach {what}: {state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn session_directories(runtime: &Runtime) -> Vec<PathBuf> {
    fs::read_dir(&runtime.0)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// A single focused editor that accepts one complete append. Recording the number
/// of replacements makes a duplicate delivery visible; there is no send or enter
/// action at this boundary.
struct Editor {
    draft: Mutex<String>,
    replace_calls: AtomicUsize,
}
impl Editor {
    fn new(draft: &str) -> Arc<Self> {
        Arc::new(Self {
            draft: Mutex::new(draft.into()),
            replace_calls: AtomicUsize::new(0),
        })
    }
    fn draft(&self) -> String {
        self.draft.lock().unwrap().clone()
    }
    fn replace_calls(&self) -> usize {
        self.replace_calls.load(Ordering::SeqCst)
    }
}
struct Selection(Arc<Editor>);
struct BoundEditor(Arc<Editor>);
impl TargetSelection for Selection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(BoundEditor(self.0.clone()))))
    }
}
impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.draft())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("The target draft changed during output".into());
        }
        *draft = replacement.into();
        self.0.replace_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A local model that would answer even before it reports readiness. Only the
/// application's own readiness gate may decide when it is called, so a transcript
/// from it after a failed preparation would be a fabricated result.
struct EagerModel {
    preparation: Mutex<Option<mpsc::Receiver<AppResult<String>>>>,
}
impl LocalTranscriber for EagerModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        self.preparation.lock().unwrap().take()
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok("fabricated result: the model was never prepared".into())
    }
}

#[test]
fn preparation_failure_during_capture_ends_in_failure_and_retains_the_whole_audio() {
    let runtime = Runtime::new();
    let previous = "上一段完整转写，保留不动。\nPrevious complete result.";
    fs::write(runtime.0.join("current-result.txt"), previous).unwrap();
    let (sender, receiver) = mpsc::channel();
    let model = Arc::new(PreparingModel::new(receiver));
    let mut app = Application::new(Arc::new(Playback), model.clone(), runtime.0.clone(), Ok(()));
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);

    sender
        .send(Err("the resident model could not be loaded".into()))
        .unwrap();
    let failure = wait(&mut app, "preparation failure", |state| {
        state.phase == Phase::Failure
    });
    assert_eq!(failure.preparation, Preparation::Failed, "{failure:?}");
    assert!(
        failure.detail.contains("could not be loaded"),
        "{failure:?}"
    );
    assert!(
        failure.detail.contains("stopped"),
        "the capture must be reported as stopped, not silently discarded: {failure:?}"
    );
    assert_eq!(
        failure.text, previous,
        "nothing from a capture with a failed preparation may be delivered"
    );
    assert_eq!(failure.delivery, None, "{failure:?}");
    assert_eq!(
        failure.retained_audio.len(),
        1,
        "the complete capture audio must be kept: {failure:?}"
    );
    let retained = failure.retained_audio[0].clone();
    let wav = hound::WavReader::open(&retained).unwrap();
    assert_eq!(
        wav.duration() as usize,
        CAPTURE_SAMPLES,
        "the retained audio must contain the whole capture, including its beginning"
    );
    assert!(failure.retry_available, "{failure:?}");
    assert!(
        failure.detail.contains("retained"),
        "the failure must explain that the audio was retained: {failure:?}"
    );

    // Retrying cannot succeed while the model is still unavailable, and it must
    // keep the same audio rather than losing it.
    app.dispatch(Control::Retry);
    let retried = wait(&mut app, "retry failure", |state| {
        state.phase == Phase::Failure && state.detail.contains("Local retry failed")
    });
    assert!(
        retried.detail.contains("failed to prepare"),
        "a retry must wait for the failed preparation instead of asking the engine: {retried:?}"
    );
    assert_eq!(retried.retained_audio, [retained.clone()]);
    assert!(retained.is_file());

    app.dispatch(Control::CleanupAudio);
    let cleaned = app.snapshot();
    assert_eq!(cleaned.phase, Phase::Failure);
    assert!(!cleaned.retry_available, "{cleaned:?}");
    assert!(cleaned.retained_audio.is_empty(), "{cleaned:?}");
    assert!(!retained.exists(), "cleanup must delete the retained audio");
    assert!(
        cleaned.detail.contains("deleted at your request"),
        "{cleaned:?}"
    );
}

#[test]
fn hotkey_start_while_the_model_is_preparing_captures_and_delivers_after_readiness() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let model = Arc::new(PreparingModel::new(receiver));
    let mut app = Application::new(Arc::new(Playback), model.clone(), runtime.0.clone(), Ok(()));
    assert_eq!(app.snapshot().phase, Phase::Preparing);

    // The hotkey arrives while the model is still loading: capture must start now,
    // and the snapshot must not pretend the model is ready.
    app.dispatch(Control::Toggle);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing, "{capturing:?}");
    assert_eq!(capturing.preparation, Preparation::Pending, "{capturing:?}");
    assert!(
        capturing.detail.contains("still preparing"),
        "a capture started during preparation must say the model is still loading: {capturing:?}"
    );
    assert!(
        capturing.timing.preparation_ms.is_none(),
        "preparation has not finished yet: {capturing:?}"
    );

    model.mark_ready();
    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready = wait(&mut app, "readiness while capturing", |state| {
        state.preparation == Preparation::Ready
    });
    assert_eq!(
        ready.phase,
        Phase::Capturing,
        "preparation completing during a capture must not replace the capture phase: {ready:?}"
    );
    assert!(
        ready.timing.preparation_ms.is_some(),
        "preparation duration must be recorded: {ready:?}"
    );

    app.dispatch(Control::Toggle);
    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(result.preparation, Preparation::Ready, "{result:?}");
    assert_eq!(
        result.text,
        format!("complete capture of {CAPTURE_SAMPLES} samples"),
        "the whole capture started during preparation must be recognized"
    );
    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    assert!(result.retained_audio.is_empty());
    assert!(
        result.timing.preparation_ms.is_some(),
        "the result must keep the measured preparation duration: {result:?}"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        result.text
    );
    assert!(
        session_directories(&runtime).is_empty(),
        "success must clean its session audio"
    );
}

#[test]
fn preparation_finishing_while_local_processing_keeps_processing_and_delivers_the_capture() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    // Long enough that preparation can finish while the stopped capture is still
    // being recognized locally.
    let model = Arc::new(PreparingModel::with_recognition_delay(receiver, 200));
    let mut app = Application::new(Arc::new(Playback), model.clone(), runtime.0.clone(), Ok(()));
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    model.mark_ready();
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Processing);

    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready = wait(&mut app, "readiness while processing", |state| {
        state.preparation == Preparation::Ready
    });
    assert_eq!(
        ready.phase,
        Phase::Processing,
        "readiness must not replace the local-processing phase: {ready:?}"
    );

    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(result.preparation, Preparation::Ready, "{result:?}");
    assert_eq!(
        result.text,
        format!("complete capture of {CAPTURE_SAMPLES} samples")
    );
    assert!(result.retained_audio.is_empty());
}

#[test]
fn stop_before_preparation_finishes_waits_for_readiness_and_appends_the_result_once() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let model = Arc::new(PreparingModel::new(receiver));
    let editor = Editor::new("原有草稿");
    let mut app = Application::new(Arc::new(Playback), model.clone(), runtime.0.clone(), Ok(()))
        .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    app.dispatch(Control::Stop);
    let processing = app.snapshot();
    assert_eq!(processing.phase, Phase::Processing, "{processing:?}");
    assert_eq!(
        processing.preparation,
        Preparation::Pending,
        "{processing:?}"
    );
    assert!(
        processing.detail.contains("still being prepared"),
        "the processing state must not claim the model is ready: {processing:?}"
    );
    assert!(
        processing.detail.contains("buffered audio"),
        "the processing state must say the audio is buffered: {processing:?}"
    );
    assert!(
        processing.detail.contains("no messages are sent"),
        "{processing:?}"
    );
    assert_eq!(
        editor.draft(),
        "原有草稿",
        "nothing may be delivered while waiting"
    );
    assert_eq!(editor.replace_calls(), 0);

    // The stopped capture is recognized only after the model reports readiness.
    model.mark_ready();
    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(result.preparation, Preparation::Ready, "{result:?}");
    assert_eq!(
        result.text,
        format!("complete capture of {CAPTURE_SAMPLES} samples")
    );
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    let draft = editor.draft();
    assert_eq!(draft, format!("原有草稿\n{}", result.text));
    assert_eq!(
        draft.matches(&result.text).count(),
        1,
        "the complete transcript must be appended exactly once: {draft:?}"
    );
    assert_eq!(
        editor.replace_calls(),
        1,
        "one append and no automatic send/enter action"
    );
    assert!(result.retained_audio.is_empty());
}

#[test]
fn preparation_failure_after_stop_reports_failure_retains_audio_and_delivers_nothing() {
    let runtime = Runtime::new();
    let previous = "上一段完整转写。";
    fs::write(runtime.0.join("current-result.txt"), previous).unwrap();
    let (sender, receiver) = mpsc::channel();
    let model = Arc::new(EagerModel {
        preparation: Mutex::new(Some(receiver)),
    });
    let editor = Editor::new("原有草稿");
    let mut app = Application::new(Arc::new(Playback), model, runtime.0.clone(), Ok(()))
        .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    app.dispatch(Control::Stop);
    let processing = app.snapshot();
    assert_eq!(processing.phase, Phase::Processing, "{processing:?}");
    assert!(
        processing.detail.contains("still being prepared"),
        "{processing:?}"
    );

    sender
        .send(Err("the resident model could not be loaded".into()))
        .unwrap();
    let failure = wait(&mut app, "preparation failure", |state| {
        state.phase == Phase::Failure
    });
    assert_eq!(failure.preparation, Preparation::Failed, "{failure:?}");
    assert!(
        failure.detail.contains("failed to prepare")
            && failure.detail.contains("could not be loaded"),
        "{failure:?}"
    );
    assert_eq!(
        failure.text, previous,
        "a capture whose model never prepared must not be delivered: {failure:?}"
    );
    assert_eq!(failure.delivery, None, "{failure:?}");
    assert_eq!(
        failure.retained_audio.len(),
        1,
        "the stopped capture's audio must be retained: {failure:?}"
    );
    assert_eq!(
        hound::WavReader::open(&failure.retained_audio[0])
            .unwrap()
            .duration() as usize,
        CAPTURE_SAMPLES
    );
    assert!(failure.retry_available, "{failure:?}");
    assert_eq!(editor.draft(), "原有草稿", "nothing may be delivered");
    assert_eq!(editor.replace_calls(), 0);

    // A retry must wait for the same failed preparation and keep the audio.
    app.dispatch(Control::Retry);
    let retried = wait(&mut app, "retry failure", |state| {
        state.phase == Phase::Failure && state.detail.contains("Local retry failed")
    });
    assert!(retried.detail.contains("failed to prepare"), "{retried:?}");
    assert_eq!(retried.retained_audio, failure.retained_audio);
    assert!(retried.retry_available);
    assert_eq!(editor.draft(), "原有草稿");
    assert_eq!(editor.replace_calls(), 0);
}

/// The real capture path writes bounded windows and recognizes them before stop.
/// With `wait_for_readiness` the fake queues a window until preparation reports
/// ready; without it the fake rejects undecoded audio exactly like the real
/// engine does before preparation, so only the application's gate can prevent it.
struct WaitingStreamingModel {
    preparation: Mutex<Option<mpsc::Receiver<AppResult<String>>>>,
    ready: Arc<AtomicBool>,
    wait_for_readiness: bool,
}
impl LocalTranscriber for WaitingStreamingModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        self.preparation.lock().unwrap().take()
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err(
                "The local model is still being prepared, so it cannot recognize audio yet".into(),
            );
        }
        Ok("retried complete capture".into())
    }
    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        _directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        if !self.ready.load(Ordering::SeqCst) {
            if !self.wait_for_readiness {
                return Err(
                    "The local SenseVoice engine is not prepared; audio was not decoded".into(),
                );
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.ready.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    return Err("the local model never reported readiness".into());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        let mut reader = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < samples.len() {
            let value = samples[offset];
            let end = (offset + 1..samples.len())
                .find(|&index| samples[index] != value)
                .unwrap_or(samples.len());
            let text = match value {
                1000 => "开始",
                2000 => "跨段",
                3000 => " Rust",
                4000 => " 收尾。",
                _ => "",
            };
            // The long spoken word straddles the first window; the full overlap recovers it.
            if value != 2000 || end - offset >= 320 {
                result.push(TimedText {
                    start_ms: offset as u64 / 16,
                    end_ms: end as u64 / 16,
                    text: text.into(),
                });
            }
            offset = end;
        }
        Ok(result)
    }
}

struct StreamingPlayback {
    input: Mutex<Option<mpsc::Receiver<Vec<i16>>>>,
}
struct StreamingRecording {
    stop: Arc<AtomicBool>,
    stream: Option<SegmentStream>,
    progress: Arc<listen::streaming::Progress>,
    worker: Option<thread::JoinHandle<AppResult<CapturedAudio>>>,
}
impl PlaybackAudio for StreamingPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let config = SegmentConfig {
            window_samples: 1600,
            overlap_samples: 320,
            max_pending_segments: 4,
            ..SegmentConfig::default()
        };
        let (mut writer, stream) = SegmentedAudioWriter::new(directory, config)?;
        let progress = stream.progress.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let input = self
            .input
            .lock()
            .unwrap()
            .take()
            .ok_or("only one capture is expected")?;
        let worker = thread::spawn(move || {
            while !signal.load(Ordering::Acquire) {
                match input.recv_timeout(Duration::from_millis(1)) {
                    Ok(samples) => {
                        for sample in samples {
                            writer.push(sample)?;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            writer.finish("preparation streaming test playback device".into())
        });
        Ok(Box::new(StreamingRecording {
            stop,
            stream: Some(stream),
            progress,
            worker: Some(worker),
        }))
    }
}
impl Recording for StreamingRecording {
    fn take_segments(&mut self) -> Option<SegmentStream> {
        self.stream.take()
    }
    fn progress(&self) -> Option<listen::streaming::CaptureProgress> {
        Some(self.progress.snapshot())
    }
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap()
    }
}

#[test]
fn streaming_capture_started_during_preparation_buffers_windows_then_recognizes_them() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let ready = Arc::new(AtomicBool::new(false));
    let model = Arc::new(WaitingStreamingModel {
        preparation: Mutex::new(Some(receiver)),
        ready: ready.clone(),
        wait_for_readiness: true,
    });
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(StreamingPlayback {
            input: Mutex::new(Some(rx)),
        }),
        model,
        runtime.0.clone(),
        Ok(()),
    );
    assert_eq!(app.snapshot().phase, Phase::Preparing);

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    let mut samples = vec![1000; 1440];
    samples.extend(vec![2000; 320]);
    samples.extend(vec![3000; 960]);
    samples.extend(vec![4000; 480]);
    tx.send(samples).unwrap();

    // The windows are on disk while the model is still preparing; they must not be
    // dropped or decoded into a fake result.
    let buffering = wait(&mut app, "buffered playback", |state| {
        state.progress.received_samples >= 3200
    });
    assert_eq!(buffering.phase, Phase::Capturing, "{buffering:?}");
    assert_eq!(buffering.preparation, Preparation::Pending, "{buffering:?}");
    assert!(
        buffering.detail.contains("still preparing"),
        "the capture must report the model is still loading: {buffering:?}"
    );

    ready.store(true, Ordering::SeqCst);
    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready_while_capturing = wait(&mut app, "readiness while capturing", |state| {
        state.preparation == Preparation::Ready && state.progress.recognized_segments >= 1
    });
    assert_eq!(ready_while_capturing.phase, Phase::Capturing);
    assert!(
        !ready_while_capturing.detail.contains("still preparing"),
        "readiness must stop claiming the model is still loading: {ready_while_capturing:?}"
    );

    app.dispatch(Control::Stop);
    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(result.text, "开始跨段 Rust 收尾。");
    assert_eq!(result.preparation, Preparation::Ready);
    assert!(result.retained_audio.is_empty());
    assert!(
        session_directories(&runtime).is_empty(),
        "success must clean every session audio directory"
    );
}

#[test]
fn streaming_capture_stopped_before_preparation_finishes_records_the_windows_after_readiness() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let ready = Arc::new(AtomicBool::new(false));
    // This fake would reject a window decoded before readiness, exactly like the
    // real engine; only the application's gate can keep it from being called.
    let model = Arc::new(WaitingStreamingModel {
        preparation: Mutex::new(Some(receiver)),
        ready: ready.clone(),
        wait_for_readiness: false,
    });
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(StreamingPlayback {
            input: Mutex::new(Some(rx)),
        }),
        model,
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    let mut samples = vec![1000; 1440];
    samples.extend(vec![2000; 320]);
    samples.extend(vec![3000; 960]);
    samples.extend(vec![4000; 480]);
    tx.send(samples).unwrap();
    let buffering = wait(&mut app, "buffered playback", |state| {
        state.progress.received_samples >= 3200
    });
    assert_eq!(
        buffering.phase,
        Phase::Capturing,
        "a window that closed before readiness must not be decoded or treated as a failure: {buffering:?}"
    );
    assert_eq!(buffering.preparation, Preparation::Pending, "{buffering:?}");

    app.dispatch(Control::Stop);
    let processing = app.snapshot();
    assert_eq!(processing.phase, Phase::Processing, "{processing:?}");
    assert!(
        processing.detail.contains("still being prepared")
            && processing.detail.contains("buffered audio"),
        "{processing:?}"
    );

    ready.store(true, Ordering::SeqCst);
    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(result.text, "开始跨段 Rust 收尾。");
    assert_eq!(result.preparation, Preparation::Ready);
    assert!(result.retained_audio.is_empty());
    assert!(
        session_directories(&runtime).is_empty(),
        "success must clean every session audio directory"
    );
}

#[test]
fn streaming_preparation_failure_retains_the_buffered_windows_for_recovery() {
    let runtime = Runtime::new();
    let previous = "上一段完整转写。";
    fs::write(runtime.0.join("current-result.txt"), previous).unwrap();
    let (sender, receiver) = mpsc::channel();
    let ready = Arc::new(AtomicBool::new(false));
    let model = Arc::new(WaitingStreamingModel {
        preparation: Mutex::new(Some(receiver)),
        ready: ready.clone(),
        wait_for_readiness: true,
    });
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(StreamingPlayback {
            input: Mutex::new(Some(rx)),
        }),
        model,
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Toggle);
    tx.send(vec![1000; 1440]).unwrap();
    let buffering = wait(&mut app, "buffered playback", |state| {
        state.progress.received_samples >= 1440
    });
    assert_eq!(buffering.phase, Phase::Capturing, "{buffering:?}");
    assert_eq!(buffering.preparation, Preparation::Pending);

    sender
        .send(Err("the resident model could not be loaded".into()))
        .unwrap();
    let failure = wait(&mut app, "preparation failure", |state| {
        state.phase == Phase::Failure
    });
    assert_eq!(failure.preparation, Preparation::Failed, "{failure:?}");
    assert!(failure.retry_available, "{failure:?}");
    assert_eq!(
        failure.text, previous,
        "a capture with a failed preparation must not be delivered as a transcript: {failure:?}"
    );
    assert!(
        !failure.retained_audio.is_empty(),
        "the buffered windows must be retained: {failure:?}"
    );
    assert!(
        failure
            .retained_audio
            .iter()
            .any(|path| hound::WavReader::open(path)
                .map(|wav| wav.duration() as usize == 1440)
                .unwrap_or(false)),
        "the retained audio must contain the whole buffered capture: {failure:?}"
    );
    let retained = failure.retained_audio.clone();

    app.dispatch(Control::Retry);
    let retried = wait(&mut app, "retry failure", |state| {
        state.phase == Phase::Failure && state.detail.contains("Local retry failed")
    });
    assert!(
        retried.detail.contains("failed to prepare"),
        "a retry must wait for the failed preparation instead of asking the engine: {retried:?}"
    );
    assert_eq!(retried.retained_audio, retained);

    app.dispatch(Control::CleanupAudio);
    assert!(app.snapshot().retained_audio.is_empty());
    assert!(
        retained.iter().all(|path| !path.exists()),
        "cleanup must delete the retained windows"
    );
}

#[test]
fn retry_and_cleanup_refuse_accurately_while_preparing_or_capturing_without_touching_audio() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let model = Arc::new(PreparingModel::new(receiver));
    let mut app = Application::new(Arc::new(Playback), model.clone(), runtime.0.clone(), Ok(()));

    // Before capture: both controls refuse while the model is preparing.
    app.dispatch(Control::Retry);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Preparing);
    assert!(
        refused.detail.contains("still being prepared")
            && refused.detail.contains("Nothing is being captured"),
        "{refused:?}"
    );
    app.dispatch(Control::CleanupAudio);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Preparing);
    assert!(
        refused.detail.contains("still being prepared")
            && refused.detail.contains("Nothing is being captured"),
        "{refused:?}"
    );

    // A capture is now running while preparation continues: the same controls keep
    // refusing, but must no longer claim that nothing is being captured, and must
    // not touch the session audio.
    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    let sessions = session_directories(&runtime);
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let audio = sessions[0].join("audio.wav");
    assert!(audio.is_file());
    app.dispatch(Control::Retry);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Capturing, "{refused:?}");
    assert!(
        refused.detail.contains("still being prepared"),
        "{refused:?}"
    );
    assert!(
        refused.detail.contains("being captured"),
        "the refusal must be accurate about the running capture: {refused:?}"
    );
    assert!(
        !refused.detail.contains("Nothing is being captured"),
        "{refused:?}"
    );
    app.dispatch(Control::CleanupAudio);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Capturing, "{refused:?}");
    assert!(
        refused.detail.contains("still being prepared")
            && refused.detail.contains("cleaning failed audio"),
        "{refused:?}"
    );
    assert!(
        audio.is_file(),
        "a refused cleanup must not delete capture audio"
    );

    model.mark_ready();
    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready = wait(&mut app, "readiness while capturing", |state| {
        state.preparation == Preparation::Ready
    });
    assert_eq!(ready.phase, Phase::Capturing);

    // With a ready model, the capture itself is still in flight.
    app.dispatch(Control::Retry);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Capturing);
    assert!(refused.detail.contains("Still capturing"), "{refused:?}");
    app.dispatch(Control::CleanupAudio);
    let refused = app.snapshot();
    assert_eq!(refused.phase, Phase::Capturing);
    assert!(refused.detail.contains("Still capturing"), "{refused:?}");
    assert!(
        audio.is_file(),
        "a refused cleanup must not delete capture audio"
    );

    // Local processing is covered by the same guards.
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Processing);
    app.dispatch(Control::Retry);
    assert!(
        app.snapshot().detail.contains("Still processing"),
        "{:?}",
        app.snapshot()
    );
    app.dispatch(Control::CleanupAudio);
    assert!(
        app.snapshot().detail.contains("Still processing"),
        "{:?}",
        app.snapshot()
    );
    let result = wait(&mut app, "result", |state| state.phase == Phase::Result);
    assert_eq!(
        result.text,
        format!("complete capture of {CAPTURE_SAMPLES} samples")
    );
}

#[test]
fn snapshot_reports_pending_ready_and_failed_preparation_without_claiming_ready_early() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(PreparingModel::new(receiver)),
        runtime.0.clone(),
        Ok(()),
    );
    let preparing = app.snapshot();
    assert_eq!(preparing.phase, Phase::Preparing, "{preparing:?}");
    assert_eq!(preparing.preparation, Preparation::Pending, "{preparing:?}");

    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready = wait(&mut app, "ready", |state| {
        state.preparation == Preparation::Ready
    });
    assert_eq!(ready.phase, Phase::Ready, "{ready:?}");
    assert!(
        ready.timing.preparation_ms.is_some(),
        "preparation duration must be recorded: {ready:?}"
    );
    assert!(
        ready.detail.contains("Resident local model is ready."),
        "{ready:?}"
    );

    let failed_runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let mut failed = Application::new(
        Arc::new(Playback),
        Arc::new(PreparingModel::new(receiver)),
        failed_runtime.0.clone(),
        Ok(()),
    );
    assert_eq!(failed.snapshot().preparation, Preparation::Pending);
    sender
        .send(Err("the resident model could not be loaded".into()))
        .unwrap();
    let failure = wait(&mut failed, "preparation failure", |state| {
        state.phase == Phase::Failure
    });
    assert_eq!(failure.preparation, Preparation::Failed, "{failure:?}");
    assert!(
        failure.detail.contains("could not be loaded"),
        "{failure:?}"
    );
    assert!(
        !failure.detail.contains("Ready."),
        "a failed preparation must not claim readiness: {failure:?}"
    );
}

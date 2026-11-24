//! First-use preparation, explicit backend fallback and stop-to-result timing at
//! the same `Application::dispatch` boundary the hotkey uses. No audio device,
//! desktop control or real model is opened here.
#![cfg(windows)]

use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Preparation, Recording, Target, TargetSelection,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-latency-{}-{}-{}",
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

struct Playback {
    starts: Arc<AtomicUsize>,
}
struct Playing(PathBuf);
impl PlaybackAudio for Playback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
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
            description: "timing test playback device".into(),
        })
    }
}

fn wait(app: &mut Application, phase: Phase) -> listen::Snapshot {
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

struct PreparingModel {
    preparation: Mutex<Option<mpsc::Receiver<AppResult<String>>>>,
    note: Mutex<Option<String>>,
}
impl LocalTranscriber for PreparingModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        self.preparation.lock().unwrap().take()
    }
    fn backend_note(&self) -> Option<String> {
        self.note.lock().unwrap().clone()
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok("prepared model result".into())
    }
}

fn wait_for_preparation(app: &mut Application) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if state.preparation == Preparation::Ready {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "preparation did not finish: {state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn preparation_is_visible_and_capture_starts_before_the_model_is_ready() {
    let runtime = Runtime::new();
    let starts = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    let model = PreparingModel {
        preparation: Mutex::new(Some(receiver)),
        note: Mutex::new(None),
    };
    let mut app = Application::new(
        Arc::new(Playback {
            starts: starts.clone(),
        }),
        Arc::new(model),
        runtime.0.clone(),
        Ok(()),
    );
    let preparing = app.snapshot();
    assert_eq!(preparing.phase, Phase::Preparing, "{preparing:?}");
    assert_eq!(preparing.preparation, Preparation::Pending, "{preparing:?}");
    assert!(
        preparing
            .detail
            .contains("Preparing the local recognition model"),
        "{preparing:?}"
    );
    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(
        capturing.phase,
        Phase::Capturing,
        "a capture started during preparation must not be refused: {capturing:?}"
    );
    assert_eq!(
        capturing.preparation,
        Preparation::Pending,
        "a capture started during preparation must not be reported as ready: {capturing:?}"
    );
    assert!(
        capturing.detail.contains("still preparing"),
        "the capture must say the model is still loading: {capturing:?}"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "a preparing model must still start the recording device"
    );
    // #15's target: "按下快捷键 <1 s 可开始采集". This is the application's own
    // dispatch-to-Capturing measurement; the real-device number is recorded from
    // status.txt `timing_capture_start_ms` in docs/issue-review-fixes.md.
    assert!(
        capturing
            .timing
            .capture_start_ms
            .is_some_and(|milliseconds| milliseconds < 1_000),
        "dispatch to Capturing must be measured and inside the 1 s target: {capturing:?}"
    );

    sender
        .send(Ok("Resident local model is ready.".into()))
        .unwrap();
    let ready = wait_for_preparation(&mut app);
    assert_eq!(
        ready.phase,
        Phase::Capturing,
        "preparation finishing during capture must not replace the capture phase: {ready:?}"
    );
    assert!(
        ready.timing.preparation_ms.is_some(),
        "preparation duration must be recorded: {ready:?}"
    );

    app.dispatch(Control::Stop);
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.text, "prepared model result");

    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_preparation_is_reported_and_never_claims_ready() {
    let runtime = Runtime::new();
    let (sender, receiver) = mpsc::channel();
    let model = PreparingModel {
        preparation: Mutex::new(Some(receiver)),
        note: Mutex::new(None),
    };
    let mut app = Application::new(
        Arc::new(Playback {
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(model),
        runtime.0.clone(),
        Ok(()),
    );
    sender
        .send(Err("the resident model could not be loaded".into()))
        .unwrap();
    let failure = wait(&mut app, Phase::Failure);
    assert!(
        failure.detail.contains("could not be loaded"),
        "{failure:?}"
    );
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Failure);
}

struct FallbackModel;
impl LocalTranscriber for FallbackModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn backend_note(&self) -> Option<String> {
        Some("CLI fallback: the resident model is unavailable; each window reloads it.".into())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok("fallback result".into())
    }
}

#[test]
fn an_explicit_backend_fallback_is_visible_in_ready_and_in_the_snapshot() {
    let runtime = Runtime::new();
    let app = Application::new(
        Arc::new(Playback {
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(FallbackModel),
        runtime.0.clone(),
        Ok(()),
    );
    let ready = app.snapshot();
    assert_eq!(ready.phase, Phase::Ready);
    assert!(
        ready.detail.contains("CLI fallback"),
        "the ready detail must name the fallback: {ready:?}"
    );
    assert_eq!(
        ready.backend.as_deref(),
        Some("CLI fallback: the resident model is unavailable; each window reloads it.")
    );
}

struct SlowModel;
impl LocalTranscriber for SlowModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn backend_note(&self) -> Option<String> {
        Some("Resident local model; fresh state per window.".into())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        thread::sleep(Duration::from_millis(60));
        Ok("timed result".into())
    }
}

#[test]
fn backfill_to_the_bound_target_stays_inside_the_three_hundred_millisecond_budget() {
    struct InstantModel;
    impl LocalTranscriber for InstantModel {
        fn check_ready(&self) -> AppResult<()> {
            Ok(())
        }
        fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
            Ok("第一行：Rust 和 WASAPI。".into())
        }
    }

    struct Editor {
        draft: Arc<Mutex<String>>,
    }
    impl Target for Editor {
        fn content(&mut self) -> AppResult<String> {
            Ok(self.draft.lock().unwrap().clone())
        }
        fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
            let mut draft = self.draft.lock().unwrap();
            if *draft != expected {
                return Err("The target draft changed during output; no replacement".into());
            }
            *draft = replacement.into();
            Ok(())
        }
    }
    struct Desktop(Arc<Mutex<String>>);
    impl TargetSelection for Desktop {
        fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
            Ok(Some(Box::new(Editor {
                draft: self.0.clone(),
            })))
        }
    }

    let runtime = Runtime::new();
    let draft = Arc::new(Mutex::new("原有草稿".to_string()));
    let mut app = Application::new(
        Arc::new(Playback {
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(InstantModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(draft.clone())));

    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing, "{capturing:?}");
    // #15: "按下快捷键 <1 s 可开始采集" and "回填 <300 ms". Both are measured in
    // the application, recorded in `Timing`, and asserted here with the fake device
    // and the fake target; the real-device numbers live in the verification doc.
    assert!(
        capturing
            .timing
            .capture_start_ms
            .is_some_and(|milliseconds| milliseconds < 1_000),
        "dispatch to Capturing must be measured and inside the 1 s target: {capturing:?}"
    );

    app.dispatch(Control::Stop);
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert_eq!(
        *draft.lock().unwrap(),
        "原有草稿\n第一行：Rust 和 WASAPI。",
        "the complete result is appended once to the bound target"
    );
    let delivery_ms = result.timing.delivery_ms.expect("backfill timing");
    assert!(
        delivery_ms <= 300,
        "backfill took {delivery_ms} ms, above the 300 ms target: {result:?}"
    );
}

#[test]
fn stop_to_result_reports_finalize_inference_save_and_delivery_separately() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback {
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(SlowModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.text, "timed result");
    let timing = &result.timing;
    assert!(
        timing.stop_to_result_ms.is_some_and(|value| value >= 50),
        "stop-to-result must include the 60 ms recognition: {timing:?}"
    );
    assert!(timing.finalize_ms.is_some(), "{timing:?}");
    assert!(
        timing.recognition_ms.is_some_and(|value| value >= 50),
        "{timing:?}"
    );
    assert!(
        timing
            .last_window_inference_ms
            .is_some_and(|value| value >= 50),
        "{timing:?}"
    );
    assert_eq!(timing.recognition_windows, 1);
    assert!(timing.save_ms.is_some(), "{timing:?}");
    assert!(timing.delivery_ms.is_some(), "{timing:?}");

    let record = fs::read_to_string(runtime.0.join("latency.jsonl")).unwrap();
    let line = record.lines().next().unwrap();
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(value["phase"], "Result");
    assert_eq!(
        value["backend"],
        "Resident local model; fresh state per window."
    );
    assert!(
        value["stop_to_result_ms"].as_u64().is_some_and(|v| v >= 50),
        "{value}"
    );
    assert!(
        value["last_window_inference_ms"].as_u64().is_some(),
        "{value}"
    );
}

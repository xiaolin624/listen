use listen::{
    AppResult, Application, CapturedAudio, Control, LocalTranscriber, Phase, PlaybackAudio,
    Recording, TimedText,
    streaming::{CaptureProgress, SegmentConfig, SegmentStream, SegmentedAudioWriter},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        // The wall clock alone is not unique: parallel tests can read the same
        // tick, and one Runtime::drop would then delete another test's directory.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-long-{}-{}-{}",
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

struct Playback {
    input: Mutex<Option<mpsc::Receiver<Vec<i16>>>>,
    config: SegmentConfig,
}
struct Playing {
    stop: Arc<AtomicBool>,
    stream: Option<SegmentStream>,
    progress: Arc<listen::streaming::Progress>,
    worker: Option<JoinHandle<AppResult<CapturedAudio>>>,
}
impl PlaybackAudio for Playback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let (mut writer, stream) = SegmentedAudioWriter::new(directory, self.config)?;
        let progress = stream.progress.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let input = self.input.lock().unwrap().take().unwrap();
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
            writer.finish("deterministic playback endpoint".into())
        });
        Ok(Box::new(Playing {
            stop,
            stream: Some(stream),
            progress,
            worker: Some(worker),
        }))
    }
}
impl Recording for Playing {
    fn take_segments(&mut self) -> Option<SegmentStream> {
        self.stream.take()
    }
    fn progress(&self) -> Option<CaptureProgress> {
        Some(self.progress.snapshot())
    }
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap()
    }
}

struct Words;
impl LocalTranscriber for Words {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _: &CapturedAudio, _: &Path) -> AppResult<String> {
        panic!("streaming playback must be recognized before stop, by bounded windows")
    }
    fn transcribe_segment(&self, audio: &CapturedAudio, _: &Path) -> AppResult<Vec<TimedText>> {
        let mut reader = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < samples.len() {
            let value = samples[offset];
            let end = (offset + 1..samples.len())
                .find(|&i| samples[i] != value)
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

fn wait(app: &mut Application, condition: impl Fn(&listen::Snapshot) -> bool) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if condition(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "flow did not complete: {state:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn config() -> SegmentConfig {
    SegmentConfig {
        window_samples: 1600,
        overlap_samples: 320,
        max_pending_segments: 4,
        ..SegmentConfig::default()
    }
}

#[test]
fn playback_is_recognized_while_capturing_and_stop_merges_cross_boundary_words_and_tail_once() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(Words),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let mut samples = vec![1000; 1440];
    samples.extend(vec![2000; 320]);
    samples.extend(vec![3000; 960]);
    samples.extend(vec![4000; 480]);
    tx.send(samples).unwrap();
    let during = wait(&mut app, |s| s.progress.recognized_segments >= 2);
    assert_eq!(during.phase, Phase::Capturing);
    assert!(
        during.text.is_empty(),
        "capturing must not publish subtitles"
    );
    app.dispatch(Control::Stop);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, |s| s.phase == Phase::Result);
    assert_eq!(result.text, "开始跨段 Rust 收尾。");
    assert!(result.retained_audio.is_empty());
    assert!(
        fs::read_dir(&runtime.0)
            .unwrap()
            .all(|entry| !entry.unwrap().path().is_dir())
    );
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().text, result.text);
}

#[test]
fn stopping_before_one_window_returns_the_whole_short_utterance() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(Words),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    tx.send(vec![4000; 160]).unwrap();
    wait(&mut app, |s| s.progress.received_samples == 160);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, |s| s.phase == Phase::Result);
    assert_eq!(result.text, "收尾。");
    assert_eq!(
        result.progress.recognized_segments, 1,
        "final measurements must include the tail"
    );
    assert_eq!(result.progress.spool_bytes, 0);
}

#[test]
fn exhausted_audio_storage_interrupts_capture_visibly_and_preserves_all_received_samples() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: SegmentConfig {
                spool_limit_bytes: 364,
                ..config()
            },
        }),
        Arc::new(Words),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    tx.send(vec![4000; 500]).unwrap();
    let failure = wait(&mut app, |s| s.phase == Phase::Failure);
    assert!(failure.detail.contains("storage budget"), "{failure:?}");
    assert_eq!(failure.progress.received_samples, 160);
    let retained: usize = failure
        .retained_audio
        .iter()
        .map(|path| hound::WavReader::open(path).unwrap().duration() as usize)
        .sum();
    assert_eq!(
        retained, 160,
        "every accepted sample must remain recoverable"
    );
    assert!(
        failure.text.is_empty(),
        "an interrupted capture must not claim a complete transcript"
    );
}

struct GatedWords(Arc<AtomicBool>);
impl LocalTranscriber for GatedWords {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _: &CapturedAudio, _: &Path) -> AppResult<String> {
        unreachable!()
    }
    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        while !self.0.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        Words.transcribe_segment(audio, directory)
    }
}

#[test]
fn slow_recognition_reports_bounded_backlog_then_retains_audio_and_checkpoint_after_interruption() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let release = Arc::new(AtomicBool::new(false));
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(GatedWords(release.clone())),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    tx.send(vec![1000; 20_000]).unwrap();
    let pressure = wait(&mut app, |s| s.phase == Phase::Processing);
    release.store(true, Ordering::Release);
    assert!(pressure.detail.contains("backlog"), "{pressure:?}");
    assert!(
        pressure.progress.peak_pending_segments <= 6,
        "queue plus running/retained windows is bounded"
    );
    assert!(pressure.progress.buffered_samples <= 320);
    let failure = wait(&mut app, |s| s.phase == Phase::Failure);
    assert!(failure.detail.contains("backlog"), "{failure:?}");
    assert!(
        !failure.text.is_empty(),
        "completed recognition must remain recoverable"
    );
    assert!(failure.detail.contains("partial-result.txt"));
    assert_eq!(
        failure.retained_audio.len(),
        1,
        "unqueued audio must remain recoverable after queued work drains"
    );
    assert_eq!(
        hound::WavReader::open(&failure.retained_audio[0])
            .unwrap()
            .duration(),
        1600
    );
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().text, failure.text);
}

#[cfg(windows)]
#[test]
fn local_cli_timestamp_output_preserves_split_chinese_bytes_and_merges_only_the_audio_overlap() {
    let runtime = Runtime::new();
    let engine = runtime.0.join("whisper-cli.exe");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_backend.rs");
    let linker = std::env::var("CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER").unwrap();
    let compiled = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg(source)
        .arg("-o")
        .arg(&engine)
        .args([
            "-C",
            "link-self-contained=yes",
            "-C",
            &format!("linker={linker}"),
        ])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::copy(&engine, runtime.0.join("vad-speech-segments.exe")).unwrap();
    let model = runtime.0.join("ggml.bin");
    fs::write(&model, 0x67676d6cu32.to_le_bytes()).unwrap();
    let transcriber = listen::whisper::Whisper {
        executable: engine,
        model: model.clone(),
        vad_model: model,
        language: "zh".into(),
        threads: 1,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(transcriber),
        runtime.0.join("flow"),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let mut samples = vec![1000; 1280];
    samples.extend(vec![2000; 480]);
    samples.extend(vec![3000; 960]);
    samples.extend(vec![4000; 480]);
    tx.send(samples).unwrap();
    wait(&mut app, |s| s.progress.recognized_segments >= 2);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, |s| s.phase == Phase::Result);
    assert_eq!(result.text, "开始跨段 Rust 收尾。");
}

struct CrashedModel;
impl LocalTranscriber for CrashedModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _: &CapturedAudio, _: &Path) -> AppResult<String> {
        panic!("backend process boundary crashed")
    }
}

#[test]
fn a_crashed_background_recognizer_is_reported_without_waiting_for_more_audio() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(CrashedModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    tx.send(vec![1000; 1600]).unwrap();
    let failure = wait(&mut app, |s| s.phase == Phase::Failure);
    assert!(failure.detail.contains("panicked"), "{failure:?}");
    assert_eq!(failure.retained_audio.len(), 1);
}

struct SpeechThenSilence;
impl LocalTranscriber for SpeechThenSilence {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _: &CapturedAudio, _: &Path) -> AppResult<String> {
        unreachable!()
    }
    fn transcribe_segment(&self, audio: &CapturedAudio, _: &Path) -> AppResult<Vec<TimedText>> {
        let mut wav = hound::WavReader::open(&audio.path).unwrap();
        if wav.samples::<i16>().all(|sample| sample.unwrap() == 1000) {
            Ok(vec![
                TimedText {
                    start_ms: 0,
                    end_ms: 80,
                    text: "开始".into(),
                },
                TimedText {
                    start_ms: 90,
                    end_ms: 100,
                    text: "收尾。".into(),
                },
            ])
        } else {
            Ok(Vec::new())
        }
    }
}

#[test]
fn silence_in_a_later_window_does_not_erase_the_already_recognized_trailing_word() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(SpeechThenSilence),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let mut samples = vec![1000; 1600];
    samples.extend(vec![0; 1280]);
    tx.send(samples).unwrap();
    wait(&mut app, |s| s.progress.recognized_segments == 2);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, |s| s.phase == Phase::Result);
    assert_eq!(result.text, "开始收尾。");
}

#[test]
fn equal_words_spoken_on_both_sides_of_a_boundary_remain_two_distinct_utterances() {
    let runtime = Runtime::new();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut app = Application::new(
        Arc::new(Playback {
            input: Mutex::new(Some(rx)),
            config: config(),
        }),
        Arc::new(Words),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let mut samples = vec![1000; 1280];
    samples.extend(vec![4000; 320]);
    samples.extend(vec![0; 960]);
    samples.extend(vec![4000; 320]);
    samples.extend(vec![0; 320]);
    tx.send(samples).unwrap();
    wait(&mut app, |s| s.progress.received_samples == 3200);
    app.dispatch(Control::Stop);
    let result = wait(&mut app, |s| s.phase == Phase::Result);
    assert_eq!(result.text, "开始 收尾。 收尾。");
}

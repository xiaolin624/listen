use listen::{
    AppResult, Application, CaptureEndpoint, CapturedAudio, Control, LocalTranscriber, Phase,
    PlaybackAudio, Recording,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const OLD_ENDPOINT: &str = "耳机 (WH-1000XM4)";
const OLD_ID: &str = "{0.0.0.00000000}.{BCDDDDA0-5EAB-4FDD-9ECD-EFEB87A8FA34}";
const NEW_ENDPOINT: &str = "扬声器 (Realtek(R) Audio)";
const NEW_ID: &str = "{0.0.0.00000000}.{F611C5B7-1080-4BBF-97E2-5C4C4B04EDD0}";
const PARTIAL: &str = "已收到的部分转写";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-routing-{}-{}-{}",
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

struct LocalModel;
impl LocalTranscriber for LocalModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok("不应被调用".into())
    }
}

/// A controlled playback boundary that reports the real capture endpoint identity
/// and can raise the same interruption the OS device boundary raises. It writes the
/// already received audio and recognition checkpoint to the session directory so the
/// application's real retention path is exercised.
struct ControlledPlayback {
    endpoint: Option<CaptureEndpoint>,
    failure: Option<String>,
    failure_after: Duration,
    checkpoint: Option<&'static str>,
}

struct ControlledRecording {
    endpoint: Option<CaptureEndpoint>,
    failure: Option<String>,
    failure_after: Duration,
    started: Instant,
    wav: PathBuf,
}

impl PlaybackAudio for ControlledPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let wav = directory.join("segment-00000000").join("playback.wav");
        fs::create_dir_all(wav.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).map_err(|e| e.to_string())?;
        for _ in 0..160 {
            writer.write_sample(1234_i16).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
        if let Some(text) = self.checkpoint {
            fs::write(directory.join("partial-result.txt"), text).unwrap();
        }
        Ok(Box::new(ControlledRecording {
            endpoint: self.endpoint.clone(),
            failure: self.failure.clone(),
            failure_after: self.failure_after,
            started: Instant::now(),
            wav,
        }))
    }
}

impl Recording for ControlledRecording {
    fn endpoint(&self) -> Option<CaptureEndpoint> {
        self.endpoint.clone()
    }
    fn failure(&self) -> Option<String> {
        self.failure
            .clone()
            .filter(|_| self.started.elapsed() >= self.failure_after)
    }
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        match self.failure {
            Some(reason) => Err(reason),
            None => Ok(CapturedAudio {
                path: self.wav,
                description: "controlled playback endpoint".into(),
            }),
        }
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

fn endpoint(name: &str, id: &str) -> CaptureEndpoint {
    CaptureEndpoint {
        role: "default multimedia render endpoint".into(),
        name: name.into(),
        id: id.into(),
    }
}

#[test]
fn capture_status_names_the_default_multimedia_endpoint_and_its_limited_scope() {
    let runtime = Runtime::new();
    let expected = endpoint(OLD_ENDPOINT, OLD_ID);
    let mut app = Application::new(
        std::sync::Arc::new(ControlledPlayback {
            endpoint: Some(expected.clone()),
            failure: None,
            failure_after: Duration::from_secs(60),
            checkpoint: None,
        }),
        std::sync::Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing);
    assert_eq!(capturing.endpoint, Some(expected));
    for expected_text in [
        "default multimedia render endpoint",
        OLD_ENDPOINT,
        OLD_ID,
        "default communication endpoint",
        "outside this capture",
        "microphone",
    ] {
        assert!(
            capturing.detail.contains(expected_text),
            "capture status must state {expected_text:?}: {capturing:?}"
        );
    }
}

#[test]
fn capture_without_a_reported_endpoint_never_claims_a_specific_route() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        std::sync::Arc::new(ControlledPlayback {
            endpoint: None,
            failure: None,
            failure_after: Duration::from_secs(60),
            checkpoint: None,
        }),
        std::sync::Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing);
    assert_eq!(capturing.endpoint, None);
    assert!(
        capturing.detail.contains("could not be identified"),
        "{capturing:?}"
    );
    assert!(
        !capturing
            .detail
            .contains("default multimedia render endpoint"),
        "an unknown route must not be presented as a confirmed endpoint: {capturing:?}"
    );
}

#[test]
fn switched_default_endpoint_interrupts_capture_clearly_and_keeps_partial_audio_and_text() {
    let runtime = Runtime::new();
    let previous = "上一次完整结果，不应被中断的采集覆盖。";
    fs::write(runtime.0.join("current-result.txt"), previous).unwrap();
    let reason = format!(
        "The default multimedia render endpoint changed during capture from '{OLD_ENDPOINT}' ({OLD_ID}) to '{NEW_ENDPOINT}' ({NEW_ID}). Capture stopped before audio from the new endpoint was mixed in; received audio is retained and this capture is incomplete."
    );
    let mut app = Application::new(
        std::sync::Arc::new(ControlledPlayback {
            endpoint: Some(endpoint(OLD_ENDPOINT, OLD_ID)),
            failure: Some(reason.clone()),
            failure_after: Duration::from_millis(60),
            checkpoint: Some(PARTIAL),
        }),
        std::sync::Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let failure = await_phase(&mut app, Phase::Failure);
    assert!(failure.detail.contains(&reason), "{failure:?}");
    assert!(failure.detail.contains("incomplete"), "{failure:?}");
    assert_eq!(failure.text, PARTIAL);
    assert!(
        failure.detail.contains("partial-result.txt"),
        "the recoverable checkpoint must be named: {failure:?}"
    );
    assert!(
        failure.detail.contains("text is incomplete"),
        "the recoverable text must be labelled incomplete: {failure:?}"
    );
    assert!(
        failure.detail.contains("not a completed transcription"),
        "{failure:?}"
    );
    assert_eq!(failure.retained_audio.len(), 1);
    assert!(failure.retained_audio[0].is_file());
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        previous,
        "an interrupted capture must not overwrite the previous complete current result"
    );
    assert_eq!(failure.endpoint, Some(endpoint(OLD_ENDPOINT, OLD_ID)));
}

#[test]
fn removed_endpoint_is_reported_with_its_identity_and_retains_received_audio() {
    let runtime = Runtime::new();
    let reason = format!(
        "The playback endpoint '{OLD_ENDPOINT}' ({OLD_ID}) was removed or became invalid during capture (AUDCLNT_E_DEVICE_INVALIDATED). Capture stopped; received audio is retained and this capture is incomplete."
    );
    let mut app = Application::new(
        std::sync::Arc::new(ControlledPlayback {
            endpoint: Some(endpoint(OLD_ENDPOINT, OLD_ID)),
            failure: Some(reason.clone()),
            failure_after: Duration::from_millis(60),
            checkpoint: None,
        }),
        std::sync::Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    let failure = await_phase(&mut app, Phase::Failure);
    assert!(failure.detail.contains(&reason), "{failure:?}");
    assert!(failure.detail.contains("AUDCLNT_E_DEVICE_INVALIDATED"));
    assert!(failure.detail.contains("incomplete"), "{failure:?}");
    assert!(
        failure.text.is_empty(),
        "no checkpoint exists, so no text may be claimed: {failure:?}"
    );
    assert_eq!(failure.retained_audio.len(), 1);
    assert!(failure.retained_audio[0].is_file());
}

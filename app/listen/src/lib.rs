use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
pub mod audio;
pub mod streaming;
#[cfg(windows)]
pub mod whisper;
use streaming::{CaptureProgress, RecognitionWorker, SegmentStream};

pub type AppResult<T> = Result<T, String>;

/// The OS playback device boundary. Implementations never open a microphone.
pub trait PlaybackAudio: Send + Sync {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>>;
}

pub trait Recording: Send {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio>;
    fn take_segments(&mut self) -> Option<SegmentStream> {
        None
    }
    fn progress(&self) -> Option<CaptureProgress> {
        None
    }
    fn failure(&self) -> Option<String> {
        None
    }
}

pub struct CapturedAudio {
    pub path: PathBuf,
    pub description: String,
}

pub struct TimedText {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// A local executable/model boundary; there is no remote fallback.
pub trait LocalTranscriber: Send + Sync {
    fn check_ready(&self) -> AppResult<()>;
    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String>;
    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        Ok(vec![TimedText {
            start_ms: 0,
            end_ms: wav.duration() as u64 * 1000 / wav.spec().sample_rate as u64,
            text: self.transcribe(audio, directory)?,
        }])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Ready,
    Capturing,
    Processing,
    Result,
    Failure,
}

#[derive(Clone, Copy)]
pub enum Control {
    Start,
    Stop,
    Toggle,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub detail: String,
    pub text: String,
    pub retained_audio: Vec<PathBuf>,
    pub progress: CaptureProgress,
}

/// The application boundary used by both global hotkey and complete-flow tests.
pub struct Application {
    snapshot: Snapshot,
    audio: Arc<dyn PlaybackAudio>,
    transcriber: Arc<dyn LocalTranscriber>,
    runtime: PathBuf,
    recording: Option<Box<dyn Recording>>,
    recognition: Option<RecognitionWorker>,
    progress: Option<Arc<streaming::Progress>>,
    session: Option<PathBuf>,
    completion: Option<mpsc::Receiver<AppResult<(String, String)>>>,
    control_error: Option<String>,
}

impl Application {
    pub fn new(
        audio: Arc<dyn PlaybackAudio>,
        transcriber: Arc<dyn LocalTranscriber>,
        runtime: PathBuf,
        hotkey_registration: AppResult<()>,
    ) -> Self {
        let control_error = hotkey_registration.err();
        let initial_error = control_error
            .clone()
            .map(|e| format!("Global hotkey unavailable: {e}"))
            .or_else(|| transcriber.check_ready().err());
        Self {
            snapshot: Snapshot {
                phase: if initial_error.is_some() { Phase::Failure } else { Phase::Ready },
                detail: initial_error.unwrap_or_else(|| "Ready. Ctrl+Alt+R starts playback capture. Closing the window keeps Listen in the tray.".into()),
                text: fs::read_to_string(runtime.join("current-result.txt")).unwrap_or_default(),
                retained_audio: Vec::new(),
                progress: CaptureProgress::default(),
            },
            audio,
            transcriber,
            runtime,
            recording: None,
            recognition: None,
            progress: None,
            session: None,
            completion: None,
            control_error,
        }
    }

    pub fn dispatch(&mut self, control: Control) {
        let control = match control {
            Control::Toggle if self.snapshot.phase == Phase::Capturing => Control::Stop,
            Control::Toggle => Control::Start,
            other => other,
        };
        match control {
            Control::Start if self.recording.is_none() && self.completion.is_none() => {
                let result = (|| {
                    if let Some(error) = &self.control_error {
                        return Err(format!("Global hotkey unavailable: {error}"));
                    }
                    self.transcriber.check_ready()?;
                    fs::create_dir_all(&self.runtime).map_err(|e| e.to_string())?;
                    let unique = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_nanos();
                    let directory = self
                        .runtime
                        .join(format!("session-{}-{unique}", std::process::id()));
                    fs::create_dir(&directory).map_err(|e| e.to_string())?;
                    self.session = Some(directory.clone());
                    self.recording = Some(self.audio.start(&directory)?);
                    if let Some(stream) = self.recording.as_mut().unwrap().take_segments() {
                        self.progress = Some(stream.progress.clone());
                        self.recognition = Some(RecognitionWorker::start(
                            stream,
                            self.transcriber.clone(),
                            directory,
                        ));
                    }
                    self.snapshot.progress = CaptureProgress::default();
                    Ok::<_, String>(())
                })();
                match result {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Capturing;
                        self.snapshot.detail =
                            "Capturing playback audio; press the hotkey again to stop.".into();
                    }
                    Err(error) => self.fail(error),
                }
            }
            Control::Stop if self.recording.is_some() => {
                let recording = self.recording.take().unwrap();
                let directory = self.session.clone().unwrap();
                let transcriber = self.transcriber.clone();
                let recognition = self.recognition.take();
                let (tx, rx) = mpsc::channel();
                self.completion = Some(rx);
                self.snapshot.phase = Phase::Processing;
                self.snapshot.detail = "Processing locally; no messages are sent.".into();
                thread::spawn(move || {
                    let captured = recording.finish();
                    let recognized = recognition.map(|worker| worker.finish());
                    let result = match (captured, recognized) {
                        (Ok(audio), Some(result)) => result.map(|text| (text, audio.description)),
                        (Ok(audio), None) => transcriber
                            .transcribe(&audio, &directory)
                            .map(|text| (text, audio.description)),
                        (Err(capture), Some(Err(recognition))) => {
                            Err(format!("{recognition}; {capture}"))
                        }
                        (Err(error), _) => Err(error),
                    };
                    let _ = tx.send(result);
                });
            }
            _ if self.completion.is_some() => {
                self.snapshot.detail = "Still processing the current playback locally; wait for the result before starting again.".into();
            }
            Control::Start => {
                self.snapshot.detail =
                    "Already capturing playback audio. Stop to transcribe the current recording."
                        .into();
            }
            _ => {
                if !self.snapshot.detail.starts_with("Not capturing;") {
                    self.snapshot.detail = format!(
                        "Not capturing; the current result is unchanged. {}",
                        self.snapshot.detail
                    );
                }
            }
        }
    }

    pub fn poll(&mut self) {
        if let Some(progress) = self.progress.as_ref().map(|p| p.snapshot()).or_else(|| {
            self.recording
                .as_ref()
                .and_then(|recording| recording.progress())
        }) {
            self.snapshot.progress = progress;
        }
        if self.snapshot.phase == Phase::Capturing && self.recognition.is_some() {
            let p = &self.snapshot.progress;
            self.snapshot.detail = format!(
                "Capturing playback and recognizing locally; no subtitles. {:.1} s received; {} recognition windows pending, {} completed; {} bytes audio on disk; {} samples audio in memory.{}",
                p.received_samples as f64 / streaming::SAMPLE_RATE as f64,
                p.pending_segments,
                p.recognized_segments,
                p.spool_bytes,
                p.buffered_samples,
                if p.pending_segments > 1 {
                    " Recognition backlog is growing; bounded spool pressure will interrupt capture with recoverable audio."
                } else {
                    ""
                }
            );
        }
        if let Some(error) = self
            .recording
            .as_ref()
            .and_then(|recording| recording.failure())
            .or_else(|| {
                self.recognition
                    .as_ref()
                    .and_then(|worker| worker.progress.failure())
            })
        {
            self.dispatch(Control::Stop);
            self.snapshot.detail = format!("{error}; finalizing partial audio for recovery.");
        }
        let Some(rx) = &self.completion else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Local processing worker stopped unexpectedly".into())
            }
        };
        self.completion = None;
        match result {
            Ok((text, description)) => {
                let result = fs::write(self.runtime.join("current-result.txt"), &text)
                    .and_then(|()| fs::remove_dir_all(self.session.as_ref().unwrap()));
                self.snapshot.text = text;
                match result {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Result;
                        self.snapshot.detail = format!(
                            "Current result is ready in the fallback text box. Review and copy it.\nPlayback: {description}"
                        );
                    }
                    Err(error) => self.fail(format!(
                        "Could not save the current result or clean temporary audio: {error}"
                    )),
                }
            }
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, error: String) {
        if let Some(directory) = &self.session {
            let mut pending = vec![directory.clone()];
            while let Some(directory) = pending.pop() {
                if let Ok(entries) = fs::read_dir(directory) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            pending.push(path);
                        } else if path.extension().is_some_and(|extension| extension == "wav")
                            && !self.snapshot.retained_audio.contains(&path)
                        {
                            self.snapshot.retained_audio.push(path);
                        }
                    }
                }
            }
        }
        self.snapshot.phase = Phase::Failure;
        self.snapshot.detail = if self.snapshot.retained_audio.is_empty() {
            error
        } else {
            format!(
                "{error}. Audio retained for local recovery: {}",
                self.snapshot
                    .retained_audio
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        if let Some(path) = self
            .session
            .as_ref()
            .map(|directory| directory.join("partial-result.txt"))
        {
            if let Ok(text) = fs::read_to_string(&path) {
                if !text.is_empty() {
                    self.snapshot.text = text;
                    self.snapshot.detail.push_str(&format!(". Incomplete recoverable text is in the fallback box and {}. This is not a completed transcription.", path.display()));
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}

//! One capture's recognition lifecycle. The application consumes session events;
//! local file decoding and its window stitching stay inside the local adapter.
use crate::{
    AppResult, CaptureEndpoint, CapturedAudio, LocalTranscriber, PlaybackAudio, Preparation,
    Recording, Timing, capture_scope, elapsed_ms,
    glossary::Glossary,
    preparation_note,
    streaming::{self, CaptureProgress, RecognitionWorker},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

/// An identity for one start/stop operation, also attached to every result event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Preparing,
    Capturing,
    Stable,
    AwaitingFinal,
    Complete,
    Failed,
}

impl SessionState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Capturing => "capturing",
            Self::Stable => "stable",
            Self::AwaitingFinal => "awaiting_final",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

pub struct RecognitionReadiness {
    pub state: Preparation,
    pub backend: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub error: Option<String>,
}

pub struct SessionRequest {
    pub id: SessionId,
    /// Start creates this directory after validating availability; retry reuses it.
    pub directory: PathBuf,
    /// Fixed at capture start; edits to the file only affect later captures.
    pub glossary: Glossary,
}

pub struct SessionStatus {
    pub state: SessionState,
    pub progress: CaptureProgress,
    pub endpoint: Option<CaptureEndpoint>,
    pub detail: String,
}

pub struct SessionResult {
    pub text: String,
    pub description: String,
    pub timing: Timing,
    pub stopped_at: Option<Instant>,
}

pub struct SessionFailure {
    pub error: String,
    pub retry_audio: Option<CapturedAudio>,
    pub retry_scope: Option<String>,
    pub timing: Timing,
}

pub enum SessionUpdate {
    /// A stable sentence is recognition progress, never a deliverable result.
    Stable(String),
    /// The whole stopped capture has been confirmed, including an empty result.
    Finished(SessionResult),
    Failed(SessionFailure),
}

pub struct SessionEvent {
    pub id: SessionId,
    pub update: SessionUpdate,
}

/// The replaceable recognition seam. A session owns capture, continuous audio
/// consumption and stop/final confirmation. A future streaming adapter can feed
/// PCM directly to its engine here; it need not implement LocalTranscriber or use
/// the local adapter's overlapping file windows.
pub trait RecognitionSessions: Send {
    /// Local recognition can warn and continue without correction. A cloud trial
    /// must not silently upload after failing to load its declared term snapshot.
    fn requires_valid_glossary(&self) -> bool {
        false
    }
    fn preparation(&mut self) -> RecognitionReadiness;
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>>;
    fn retry(
        &mut self,
        request: SessionRequest,
        audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>>;
}

pub trait RecognitionSession: Send {
    /// Idempotent: repeating stop must not start another finalization.
    fn stop(&mut self);
    fn status(&self) -> SessionStatus;
    fn poll(&mut self) -> Option<SessionEvent>;
}

const READINESS_TIMEOUT: Duration = Duration::from_secs(120);

enum ReadinessState {
    Pending,
    Ready,
    Failed(String),
}

/// Shared only by the local adapter and its decoder. UI polling resolves model
/// preparation without making the capture wait to start storing audio.
pub(crate) struct ReadinessSignal {
    state: Mutex<ReadinessState>,
    changed: Condvar,
}

impl ReadinessSignal {
    fn new(state: ReadinessState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
            changed: Condvar::new(),
        })
    }
    fn resolve(&self, state: ReadinessState) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = state;
        self.changed.notify_all();
    }
    fn preparation(&self) -> Preparation {
        match &*self.state.lock().unwrap_or_else(|error| error.into_inner()) {
            ReadinessState::Pending => Preparation::Pending,
            ReadinessState::Ready => Preparation::Ready,
            ReadinessState::Failed(_) => Preparation::Failed,
        }
    }
    fn failure(&self) -> Option<String> {
        match &*self.state.lock().unwrap_or_else(|error| error.into_inner()) {
            ReadinessState::Failed(error) => Some(error.clone()),
            _ => None,
        }
    }
    pub(crate) fn wait(&self) -> AppResult<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let deadline = Instant::now()
            .checked_add(READINESS_TIMEOUT)
            .unwrap_or_else(Instant::now);
        loop {
            match &*state {
                ReadinessState::Ready => return Ok(()),
                ReadinessState::Failed(error) => {
                    return Err(format!(
                        "The local recognition model failed to prepare: {error}. The buffered audio was not decoded and is retained."
                    ));
                }
                ReadinessState::Pending => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "The local recognition model was still preparing after {} s, so the buffered audio was not decoded and is retained.",
                    READINESS_TIMEOUT.as_secs()
                ));
            }
            let (guard, _) = self
                .changed
                .wait_timeout(state, deadline.duration_since(now))
                .unwrap_or_else(|error| error.into_inner());
            state = guard;
        }
    }
}

pub(crate) struct LocalSessions {
    audio: Arc<dyn PlaybackAudio>,
    transcriber: Arc<dyn LocalTranscriber>,
    preparation: Option<mpsc::Receiver<AppResult<String>>>,
    preparation_started: Option<Instant>,
    preparation_ms: Option<u64>,
    readiness: Arc<ReadinessSignal>,
    backend: Option<String>,
}

impl LocalSessions {
    pub fn new(audio: Arc<dyn PlaybackAudio>, transcriber: Arc<dyn LocalTranscriber>) -> Self {
        let error = transcriber.check_ready().err();
        let preparation = if error.is_none() {
            transcriber.prepare()
        } else {
            None
        };
        let preparation_started = preparation.as_ref().map(|_| Instant::now());
        let state = match error {
            Some(error) => ReadinessState::Failed(error),
            None if preparation.is_some() => ReadinessState::Pending,
            None => ReadinessState::Ready,
        };
        Self {
            audio,
            backend: transcriber.backend_note(),
            transcriber,
            preparation,
            preparation_started,
            preparation_ms: None,
            readiness: ReadinessSignal::new(state),
        }
    }
}

impl RecognitionSessions for LocalSessions {
    fn preparation(&mut self) -> RecognitionReadiness {
        if let Some(receiver) = &self.preparation {
            let received = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "Local model preparation stopped unexpectedly, so the local model is not ready.".into()
                )),
            };
            if let Some(result) = received {
                match result {
                    Ok(note) => {
                        self.readiness.resolve(ReadinessState::Ready);
                        self.preparation_ms = self.preparation_started.map(elapsed_ms);
                        if self.backend.is_none() {
                            self.backend = Some(note);
                        }
                    }
                    Err(error) => self.readiness.resolve(ReadinessState::Failed(error)),
                }
                self.preparation = None;
                self.preparation_started = None;
            }
        }
        if let Some(note) = self.transcriber.backend_note() {
            self.backend = Some(note);
        }
        RecognitionReadiness {
            state: self.readiness.preparation(),
            backend: self.backend.clone(),
            elapsed_ms: self.preparation_ms,
            error: self.readiness.failure(),
        }
    }

    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        self.transcriber.check_ready()?;
        fs::create_dir(&request.directory).map_err(|error| error.to_string())?;
        let mut recording = self.audio.start(&request.directory)?;
        let endpoint = recording.endpoint();
        // Stable progress is advisory. Coalesce it if the UI has not polled;
        // never accumulate a transcript copy for every decoded local window.
        let (stable_tx, stable) = mpsc::sync_channel(1);
        let recognition = recording.take_segments().map(|stream| {
            RecognitionWorker::start(
                stream,
                self.transcriber.clone(),
                request.directory.clone(),
                self.readiness.clone(),
                stable_tx,
            )
        });
        let progress = recognition.as_ref().map(|worker| worker.progress.clone());
        Ok(Box::new(LocalSession {
            request,
            recording: Some(recording),
            recognition,
            progress,
            endpoint,
            transcriber: self.transcriber.clone(),
            readiness: self.readiness.clone(),
            state: SessionState::Capturing,
            stable,
            completion: None,
            final_progress: CaptureProgress::default(),
        }))
    }

    fn retry(
        &mut self,
        request: SessionRequest,
        audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        let (tx, rx) = mpsc::channel();
        let transcriber = self.transcriber.clone();
        let readiness = self.readiness.clone();
        let id = request.id;
        let directory = request.directory.clone();
        let glossary = request.glossary.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let result = readiness
                .wait()
                .and_then(|()| transcriber.check_ready())
                .and_then(|()| transcriber.transcribe(&audio, &directory))
                .map(|text| glossary.correct(&text));
            let elapsed = elapsed_ms(started);
            let timing = Timing {
                recognition_ms: Some(elapsed),
                last_window_inference_ms: Some(elapsed),
                recognition_windows: 1,
                ..Timing::default()
            };
            let update = match result {
                Ok(text) => SessionUpdate::Finished(SessionResult {
                    text,
                    description: audio.description,
                    timing,
                    stopped_at: None,
                }),
                Err(error) => SessionUpdate::Failed(SessionFailure {
                    error: format!("Local retry failed: {error}"),
                    retry_audio: Some(audio),
                    retry_scope: None,
                    timing,
                }),
            };
            let _ = tx.send(SessionEvent { id, update });
        });
        let (_, stable) = mpsc::channel();
        Ok(Box::new(LocalSession {
            request,
            recording: None,
            recognition: None,
            progress: None,
            endpoint: None,
            transcriber: self.transcriber.clone(),
            readiness: self.readiness.clone(),
            state: SessionState::AwaitingFinal,
            stable,
            completion: Some(rx),
            final_progress: CaptureProgress::default(),
        }))
    }
}

struct LocalSession {
    request: SessionRequest,
    recording: Option<Box<dyn Recording>>,
    recognition: Option<RecognitionWorker>,
    progress: Option<Arc<streaming::Progress>>,
    endpoint: Option<CaptureEndpoint>,
    transcriber: Arc<dyn LocalTranscriber>,
    readiness: Arc<ReadinessSignal>,
    state: SessionState,
    stable: mpsc::Receiver<String>,
    completion: Option<mpsc::Receiver<SessionEvent>>,
    final_progress: CaptureProgress,
}

impl LocalSession {
    fn progress(&self) -> CaptureProgress {
        self.progress
            .as_ref()
            .map(|progress| progress.snapshot())
            .or_else(|| {
                self.recording
                    .as_ref()
                    .and_then(|recording| recording.progress())
            })
            .unwrap_or_else(|| self.final_progress.clone())
    }

    fn finish(&mut self, preparation_failure: Option<String>) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let stopped_at = Instant::now();
        self.final_progress = self.progress();
        if preparation_failure.is_some() {
            if let Some(progress) = &self.progress {
                progress.cancel();
            }
        }
        let recognition = self.recognition.take();
        let streaming = recognition.is_some();
        let progress = self.progress.clone();
        let transcriber = self.transcriber.clone();
        let readiness = self.readiness.clone();
        let directory = self.request.directory.clone();
        let glossary = self.request.glossary.clone();
        let id = self.request.id;
        let (tx, rx) = mpsc::channel();
        self.completion = Some(rx);
        self.state = SessionState::AwaitingFinal;
        thread::spawn(move || {
            let finalize_started = Instant::now();
            let captured = recording.finish();
            let finalize_ms = elapsed_ms(finalize_started);
            let recognition_started = Instant::now();
            let recognized = recognition.map(|worker| worker.finish());
            let mut timing = Timing {
                finalize_ms: Some(finalize_ms),
                recognition_ms: Some(elapsed_ms(recognition_started)),
                ..Timing::default()
            };
            let audio = match captured {
                Ok(audio) => audio,
                Err(capture_error) => {
                    let error = match recognized {
                        Some(Err(error)) => format!("{error}; {capture_error}"),
                        _ => capture_error,
                    };
                    timing.stop_to_result_ms = Some(elapsed_ms(stopped_at));
                    let _ = tx.send(SessionEvent {
                        id,
                        update: SessionUpdate::Failed(SessionFailure {
                            error,
                            retry_audio: None,
                            retry_scope: None,
                            timing,
                        }),
                    });
                    return;
                }
            };
            let result = match recognized {
                Some(Ok(outcome)) => {
                    timing.last_window_wait_ms = Some(outcome.timing.wait_ms);
                    timing.last_window_inference_ms = Some(outcome.timing.inference_ms);
                    timing.recognition_windows = outcome.timing.windows;
                    Ok(outcome.text)
                }
                Some(Err(error)) => Err(error),
                None => {
                    let started = Instant::now();
                    let result = readiness
                        .wait()
                        .and_then(|()| transcriber.transcribe(&audio, &directory));
                    let elapsed = elapsed_ms(started);
                    timing.recognition_ms = Some(elapsed);
                    timing.last_window_wait_ms = Some(0);
                    timing.last_window_inference_ms = Some(elapsed);
                    timing.recognition_windows = 1;
                    result
                }
            };
            let result = match preparation_failure {
                Some(error) => Err(format!(
                    "The local recognition model failed to prepare: {error}. The capture in progress was stopped; nothing was transcribed or delivered."
                )),
                None => result,
            };
            let update = match result {
                Ok(text) => SessionUpdate::Finished(SessionResult {
                    text: glossary.correct(&text),
                    description: audio.description,
                    timing,
                    stopped_at: Some(stopped_at),
                }),
                Err(error) => {
                    let recognized_segments = progress
                        .as_ref()
                        .map(|progress| progress.snapshot().recognized_segments)
                        .unwrap_or(0);
                    let retry_audio = if audio.path.is_file() {
                        Some(audio)
                    } else if streaming && recognized_segments == 0 {
                        streaming::retained_audio_path(&directory)
                            .ok()
                            .map(|path| CapturedAudio {
                                path,
                                description: audio.description,
                            })
                    } else {
                        None
                    };
                    let retry_scope = if retry_audio.is_none()
                        && streaming
                        && recognized_segments > 0
                    {
                        Some(format!(
                            "This capture was partially recognized before the failure ({recognized_segments} window(s) had been checkpointed). Those windows' audio was released after recognition, so automatic local retry is only offered when no recognition window has completed; it is not offered here. The recognized prefix above is incomplete, and the remaining audio paths are listed for manual recovery."
                        ))
                    } else {
                        None
                    };
                    timing.stop_to_result_ms = Some(elapsed_ms(stopped_at));
                    SessionUpdate::Failed(SessionFailure {
                        error,
                        retry_audio,
                        retry_scope,
                        timing,
                    })
                }
            };
            let _ = tx.send(SessionEvent { id, update });
        });
    }
}

impl RecognitionSession for LocalSession {
    fn stop(&mut self) {
        self.finish(None);
    }

    fn status(&self) -> SessionStatus {
        let progress = self.progress();
        let preparation = self.readiness.preparation();
        let state = if self.recording.is_some() && preparation == Preparation::Pending {
            SessionState::Preparing
        } else {
            self.state
        };
        let detail = if self.recording.is_some() {
            let mut detail = capture_scope(self.endpoint.as_ref());
            if self.recognition.is_some() {
                detail.push_str(&format!(
                    " {:.1} s received; {} recognition windows pending, {} completed; {} bytes audio on disk; {} samples audio in memory.{}",
                    progress.received_samples as f64 / streaming::SAMPLE_RATE as f64,
                    progress.pending_segments, progress.recognized_segments, progress.spool_bytes,
                    progress.buffered_samples,
                    if progress.pending_segments > 1 {
                        " Recognition backlog is growing; bounded spool pressure will interrupt capture with recoverable audio."
                    } else { "" }
                ));
            }
            detail.push_str(preparation_note(preparation));
            detail
        } else if preparation == Preparation::Pending {
            crate::PROCESSING_WHILE_PREPARING_DETAIL.into()
        } else {
            crate::PROCESSING_READY_DETAIL.into()
        };
        SessionStatus {
            state,
            progress,
            endpoint: self.endpoint.clone(),
            detail,
        }
    }

    fn poll(&mut self) -> Option<SessionEvent> {
        if self.recording.is_some() {
            if let Some(error) = self.readiness.failure() {
                self.finish(Some(error));
            } else if self
                .recording
                .as_ref()
                .and_then(|recording| recording.failure())
                .or_else(|| {
                    self.progress
                        .as_ref()
                        .and_then(|progress| progress.failure())
                })
                .is_some()
            {
                self.finish(None);
            }
        }
        if let Some(receiver) = &self.completion {
            let event = match receiver.try_recv() {
                Ok(event) => Some(event),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(SessionEvent {
                    id: self.request.id,
                    update: SessionUpdate::Failed(SessionFailure {
                        error: "Local processing worker stopped unexpectedly".into(),
                        retry_audio: None,
                        retry_scope: None,
                        timing: Timing::default(),
                    }),
                }),
            };
            if let Some(event) = event {
                self.completion = None;
                self.state = match event.update {
                    SessionUpdate::Failed(_) => SessionState::Failed,
                    _ => SessionState::Complete,
                };
                return Some(event);
            }
        }
        if self.recording.is_some() {
            if let Ok(text) = self.stable.try_recv() {
                self.state = SessionState::Stable;
                return Some(SessionEvent {
                    id: self.request.id,
                    update: SessionUpdate::Stable(self.request.glossary.correct(&text)),
                });
            }
        }
        None
    }
}

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod paste;
pub mod streaming;
#[cfg(windows)]
pub mod target;
#[cfg(windows)]
pub mod whisper;
#[cfg(windows)]
pub mod whisper_timing;
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
    /// The render endpoint this recording actually captured. Device-bound
    /// implementations report the role, identifier and friendly name read from the
    /// OS; a recording that cannot name its endpoint must return `None` rather than
    /// letting the application claim a route it has not verified.
    fn endpoint(&self) -> Option<CaptureEndpoint> {
        None
    }
}

/// The exact render route a capture is bound to. The role is kept as text because
/// the application never captures a communications endpoint or a microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureEndpoint {
    pub role: String,
    pub id: String,
    pub name: String,
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

/// The foreground editor boundary. Selection happens once, when capture starts.
pub trait TargetSelection: Send + Sync {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>>;
}

/// A bound external editor. Replacing a value never sends keyboard or submit actions.
pub trait Target: Send {
    fn content(&mut self) -> AppResult<String>;
    /// Replace only if the latest draft still equals `expected`, and return success
    /// only after reading back the complete replacement. An error may mean output
    /// happened partially; callers must retain the transcript and never retry it or
    /// restore the previous text automatically. Reading the target afterwards for a
    /// status report is allowed.
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Ready,
    Capturing,
    Processing,
    Result,
    NoSpeech,
    Failure,
}

#[derive(Clone, Copy)]
pub enum Control {
    Start,
    Stop,
    Toggle,
    Retry,
    CleanupAudio,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    Appended,
    Fallback,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub detail: String,
    pub text: String,
    pub retained_audio: Vec<PathBuf>,
    pub progress: CaptureProgress,
    pub delivery: Option<Delivery>,
    pub retry_available: bool,
    pub endpoint: Option<CaptureEndpoint>,
}

/// States which render route the capture is bound to and which routes are outside
/// its scope. A capture that cannot name its endpoint never claims to cover one.
fn capture_scope(endpoint: Option<&CaptureEndpoint>) -> String {
    match endpoint {
        Some(endpoint) => format!(
            "Capturing the {} '{}' ({}); no microphone is used. Only this endpoint is captured: audio routed only to the default communication endpoint or to another device is outside this capture. Press the hotkey again to stop.",
            endpoint.role, endpoint.name, endpoint.id
        ),
        None => "Capturing playback audio; the selected render endpoint could not be identified, so this result does not claim to cover a specific endpoint. No microphone is used. Press the hotkey again to stop.".into(),
    }
}

struct Completed {
    text: String,
    description: String,
    delivery: Option<Delivery>,
    detail: String,
    save_error: Option<String>,
    directory: PathBuf,
}

/// Audio from a failed recognition attempt, kept on disk so the user can retry
/// without replaying the speech. The target is the one bound at capture start;
/// retrying never selects the focus that happens to be active later.
struct RetainedCapture {
    audio: CapturedAudio,
    directory: PathBuf,
    target: AppResult<Option<Box<dyn Target>>>,
}

enum Completion {
    Finished(Completed),
    Failed {
        error: String,
        directory: PathBuf,
        retry: Option<RetainedCapture>,
    },
}

/// One delivery attempt's user-visible outcome. A complete append is only reported
/// after the target contract confirms that the whole replacement was read back.
fn deliver(target: AppResult<Option<Box<dyn Target>>>, text: &str) -> (Delivery, String) {
    let mut target = match target {
        Ok(Some(target)) => target,
        Ok(None) => {
            return not_delivered(
                "No supported editable target was selected at capture start".into(),
            );
        }
        Err(error) => return not_delivered(error),
    };
    let latest = match target.content() {
        Ok(latest) => latest,
        Err(error) => return not_delivered(error),
    };
    let replacement = if latest.is_empty() {
        text.to_string()
    } else {
        format!("{latest}\n{text}")
    };
    match target.replace(&latest, &replacement) {
        Ok(()) => (
            Delivery::Appended,
            "The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.".into(),
        ),
        Err(error) => {
            // Diagnose the failed write by reading the target again. Reading is not a
            // retry: the write is never repeated and the previous text is never restored.
            let observed = target.content();
            failed_write(&latest, &replacement, &error, observed)
        }
    }
}

/// Nothing was sent to the target because selection, reading, or saving failed first.
fn not_delivered(reason: String) -> (Delivery, String) {
    (
        Delivery::Fallback,
        format!(
            "{reason}. No text was written to any control. The complete result is in the fallback text box. Review and copy it to the intended input."
        ),
    )
}

/// Reports what the target actually holds after a failed write, without ever claiming
/// a complete delivery. The read-back distinguishes a refused write from a truncated
/// one, a late complete write, and a possibly partial one.
fn failed_write(
    previous: &str,
    intended: &str,
    error: &str,
    observed: AppResult<String>,
) -> (Delivery, String) {
    let advice = "Check the original target before copying; output is not retried automatically and the original text is not reverted.";
    let fallback = "The complete result is in the fallback text box.";
    let observed = match observed {
        Ok(observed) => observed,
        Err(read_error) => {
            return (
                Delivery::Fallback,
                format!(
                    "The write to the original target could not be confirmed ({error}); the target could not be read back ({read_error}). {advice} {fallback}"
                ),
            );
        }
    };
    // Line endings are not part of the delivered text's meaning.
    let observed = observed.replace("\r\n", "\n");
    let previous = previous.replace("\r\n", "\n");
    let intended = intended.replace("\r\n", "\n");
    if observed == previous {
        (
            Delivery::Fallback,
            format!(
                "The original target still holds its previous content, so nothing was written to it ({error}). {fallback} Review and copy it to the intended input."
            ),
        )
    } else if observed == intended {
        (
            Delivery::Fallback,
            format!(
                "The write to the original target could not be confirmed ({error}), but it now contains the complete appended text. {advice} A copy remains in Listen."
            ),
        )
    } else if !observed.is_empty() && intended.starts_with(&observed) {
        (
            Delivery::Fallback,
            format!(
                "The original target holds only {} of {} characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. {advice} {fallback}",
                observed.chars().count(),
                intended.chars().count()
            ),
        )
    } else {
        (
            Delivery::Fallback,
            format!(
                "The write to the original target could not be confirmed ({error}); the target content changed and may be partially written. {advice} {fallback}"
            ),
        )
    }
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
    completion: Option<mpsc::Receiver<Completion>>,
    retry: Option<RetainedCapture>,
    control_error: Option<String>,
    targets: Option<Arc<dyn TargetSelection>>,
    bound_target: AppResult<Option<Box<dyn Target>>>,
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
                detail: initial_error.unwrap_or_else(|| "Ready. Ctrl+Alt+R captures only the default multimedia playback endpoint; audio routed only to the default communication endpoint or another device is outside this capture. Closing the window keeps Listen in the tray.".into()),
                text: fs::read_to_string(runtime.join("current-result.txt")).unwrap_or_default(),
                retained_audio: Vec::new(),
                progress: CaptureProgress::default(),
                delivery: None,
                retry_available: false,
                endpoint: None,
            },
            audio,
            transcriber,
            runtime,
            recording: None,
            recognition: None,
            progress: None,
            session: None,
            completion: None,
            retry: None,
            control_error,
            targets: None,
            bound_target: Ok(None),
        }
    }

    pub fn with_targets(mut self, targets: Arc<dyn TargetSelection>) -> Self {
        self.targets = Some(targets);
        self
    }

    pub fn dispatch(&mut self, control: Control) {
        let control = match control {
            Control::Toggle if self.snapshot.phase == Phase::Capturing => Control::Stop,
            Control::Toggle => Control::Start,
            other => other,
        };
        match control {
            Control::Start if self.recording.is_none() && self.completion.is_none() => {
                if self.retry.is_some() {
                    self.snapshot.detail = "A failed capture's audio is retained for local retry. Retry local recognition or clean the failed audio before starting a new capture; the current result is unchanged.".into();
                    return;
                }
                // Capture the actual selection before readiness checks or any UI feedback.
                self.bound_target = self
                    .targets
                    .as_ref()
                    .map_or(Ok(None), |targets| targets.focused_target());
                self.snapshot.endpoint = None;
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
                    self.snapshot.endpoint = self
                        .recording
                        .as_ref()
                        .and_then(|recording| recording.endpoint());
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
                        self.snapshot.delivery = None;
                        self.snapshot.retained_audio.clear();
                        self.snapshot.retry_available = false;
                        self.snapshot.detail = capture_scope(self.snapshot.endpoint.as_ref());
                    }
                    Err(error) => {
                        self.bound_target = Ok(None);
                        let directory = self.session.take();
                        self.fail(directory.as_deref(), error);
                    }
                }
            }
            Control::Stop if self.recording.is_some() => {
                let recording = self.recording.take().unwrap();
                let directory = self.session.take().unwrap();
                let transcriber = self.transcriber.clone();
                let recognition = self.recognition.take();
                let streaming = recognition.is_some();
                let progress = self.progress.clone();
                let target = std::mem::replace(&mut self.bound_target, Ok(None));
                let current_result = self.runtime.join("current-result.txt");
                let (tx, rx) = mpsc::channel();
                self.completion = Some(rx);
                self.snapshot.phase = Phase::Processing;
                self.snapshot.detail = "Processing locally; no messages are sent.".into();
                thread::spawn(move || {
                    let captured = recording.finish();
                    let recognized = recognition.map(|worker| worker.finish());
                    let audio = match captured {
                        Ok(audio) => audio,
                        Err(capture_error) => {
                            let error = match &recognized {
                                Some(Err(recognition_error)) => {
                                    format!("{recognition_error}; {capture_error}")
                                }
                                _ => capture_error,
                            };
                            let _ = tx.send(Completion::Failed {
                                error,
                                directory,
                                retry: None,
                            });
                            return;
                        }
                    };
                    let recognized = match recognized {
                        Some(result) => result,
                        None => transcriber.transcribe(&audio, &directory),
                    };
                    let completion = match recognized {
                        // Only an unfragmented capture can be retried as a whole. A
                        // streamed session that has already consumed windows no longer
                        // has their audio on disk, so its checkpoint stays a partial
                        // result instead of pretending the remaining windows are whole.
                        Err(error) => {
                            let retained = if audio.path.is_file() {
                                Some(CapturedAudio {
                                    path: audio.path.clone(),
                                    description: audio.description.clone(),
                                })
                            } else if streaming
                                && progress.as_ref().is_some_and(|progress| {
                                    progress.snapshot().recognized_segments == 0
                                })
                            {
                                streaming::retained_audio_path(&directory).ok().map(|path| {
                                    CapturedAudio {
                                        path,
                                        description: audio.description.clone(),
                                    }
                                })
                            } else {
                                None
                            };
                            let retry = retained.map(|audio| RetainedCapture {
                                audio,
                                directory: directory.clone(),
                                target,
                            });
                            Completion::Failed {
                                error,
                                directory,
                                retry,
                            }
                        }
                        // Silence and non-speech leave the previous result and every
                        // target untouched; nothing is saved or appended for them.
                        Ok(text) if text.trim().is_empty() => Completion::Finished(Completed {
                            text,
                            description: audio.description,
                            delivery: None,
                            detail: String::new(),
                            save_error: None,
                            directory,
                        }),
                        Ok(text) => {
                            let save_error = fs::write(&current_result, &text)
                                .err()
                                .map(|e| e.to_string());
                            let (delivery, detail) = match &save_error {
                                // Saving first means a storage failure never changes a target.
                                Some(error) => not_delivered(format!(
                                    "Could not save the current result: {error}"
                                )),
                                None => deliver(target, &text),
                            };
                            Completion::Finished(Completed {
                                text,
                                description: audio.description,
                                delivery: Some(delivery),
                                detail,
                                save_error,
                                directory,
                            })
                        }
                    };
                    let _ = tx.send(completion);
                });
            }
            Control::Retry => {
                if self.recording.is_some() || self.completion.is_some() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before retrying.".into();
                    return;
                }
                self.retry_local_recognition();
            }
            Control::CleanupAudio => {
                if self.recording.is_some() || self.completion.is_some() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before cleaning failed audio.".into();
                    return;
                }
                self.cleanup_failed_audio();
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

    /// Re-run local recognition over the exact audio kept from a failed attempt.
    /// This never starts capture, never re-selects a target, and never uses a
    /// remote service: the only recognizer is the configured local transcriber.
    fn retry_local_recognition(&mut self) {
        let Some(retry) = self.retry.take() else {
            self.snapshot.retry_available = false;
            self.snapshot.detail = "No retained audio is available for a local retry, so nothing was recognized and no result was delivered. The current result is unchanged.".into();
            return;
        };
        if !retry.audio.path.is_file() {
            self.snapshot.retry_available = false;
            self.snapshot.retained_audio.clear();
            let _ = fs::remove_dir_all(&retry.directory);
            self.snapshot.detail = "The audio retained from the failed capture is no longer on disk, so it was not retried. The current result is unchanged.".into();
            return;
        }
        let RetainedCapture {
            audio,
            directory,
            target,
        } = retry;
        let transcriber = self.transcriber.clone();
        let current_result = self.runtime.join("current-result.txt");
        let (tx, rx) = mpsc::channel();
        self.completion = Some(rx);
        self.snapshot.phase = Phase::Processing;
        self.snapshot.delivery = None;
        self.snapshot.retry_available = false;
        self.snapshot.detail =
            "Retrying recognition locally on the retained audio; no replay is needed and no messages are sent.".into();
        thread::spawn(move || {
            let recognized = transcriber
                .check_ready()
                .and_then(|()| transcriber.transcribe(&audio, &directory));
            let completion = match recognized {
                // Still failing: the same audio stays retryable after another local
                // repair. Nothing has been written to any target.
                Err(error) => {
                    let retry = RetainedCapture {
                        audio,
                        directory: directory.clone(),
                        target,
                    };
                    Completion::Failed {
                        error: format!("Local retry failed: {error}"),
                        directory,
                        retry: Some(retry),
                    }
                }
                Ok(text) if text.trim().is_empty() => Completion::Finished(Completed {
                    text,
                    description: audio.description,
                    delivery: None,
                    detail: String::new(),
                    save_error: None,
                    directory,
                }),
                Ok(text) => match fs::write(&current_result, &text) {
                    Ok(()) => {
                        let (delivery, detail) = deliver(target, &text);
                        Completion::Finished(Completed {
                            text,
                            description: audio.description,
                            delivery: Some(delivery),
                            detail,
                            save_error: None,
                            directory,
                        })
                    }
                    Err(error) => {
                        let retry = RetainedCapture {
                            audio,
                            directory: directory.clone(),
                            target,
                        };
                        Completion::Failed {
                            error: format!("Could not save the current result: {error}"),
                            directory,
                            retry: Some(retry),
                        }
                    }
                },
            };
            let _ = tx.send(completion);
        });
    }

    /// Delete the audio kept from a failed recognition at the user's explicit
    /// request. A later retry must not act as if a capture were still available.
    fn cleanup_failed_audio(&mut self) {
        let Some(retry) = self.retry.take() else {
            self.snapshot.retry_available = false;
            self.snapshot.detail =
                "There is no failed capture audio to clean. The current result is unchanged."
                    .into();
            return;
        };
        match fs::remove_dir_all(&retry.directory) {
            Ok(()) => {
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                self.snapshot.detail = "The temporarily retained audio from the failed capture was deleted at your request. It will not be retried, and the current result is unchanged.".into();
            }
            Err(error) => {
                self.retry = Some(retry);
                self.snapshot.retry_available = true;
                self.snapshot.detail = format!(
                    "The temporarily retained audio could not be deleted ({error}), so it is still available for a local retry."
                );
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
                "{} {:.1} s received; {} recognition windows pending, {} completed; {} bytes audio on disk; {} samples audio in memory.{}",
                capture_scope(self.snapshot.endpoint.as_ref()),
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
        let completion = match rx.try_recv() {
            Ok(completion) => completion,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.completion = None;
                self.fail(None, "Local processing worker stopped unexpectedly".into());
                return;
            }
        };
        self.completion = None;
        match completion {
            Completion::Finished(completed) if completed.text.trim().is_empty() => {
                // A completed no-speech session is not a new transcript. In particular,
                // leave the previous copyable result and its on-disk copy untouched.
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                match fs::remove_dir_all(&completed.directory) {
                    Ok(()) => {
                        self.snapshot.phase = Phase::NoSpeech;
                        self.snapshot.delivery = None;
                        self.snapshot.detail = format!(
                            "No valid speech detected; nothing was delivered. The previous current result is unchanged.\nPlayback: {}", completed.description
                        );
                    }
                    Err(error) => self.fail(
                        Some(&completed.directory),
                        format!(
                            "No valid speech detected, but temporary audio could not be cleaned: {error}"
                        ),
                    ),
                }
            }
            Completion::Finished(completed) => {
                let cleanup = if let Some(error) = &completed.save_error {
                    // A storage failure keeps the audio recoverable, as before.
                    Err(error.clone())
                } else {
                    fs::remove_dir_all(&completed.directory).map_err(|e| e.to_string())
                };
                self.snapshot.text = completed.text;
                self.snapshot.delivery = completed.delivery;
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                match cleanup {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Result;
                        self.snapshot.detail =
                            format!("{}\nPlayback: {}", completed.detail, completed.description);
                    }
                    Err(error) => self.fail(
                        Some(&completed.directory),
                        format!(
                            "Could not save the current result or clean temporary audio: {error}"
                        ),
                    ),
                }
            }
            Completion::Failed {
                error,
                directory,
                retry,
            } => {
                let retryable = retry.is_some();
                self.retry = retry;
                self.snapshot.retry_available = retryable;
                self.fail(Some(&directory), error);
                if retryable {
                    self.snapshot.detail.push_str("\nThe audio from this capture is retained. Local retry recognizes the same audio again without replaying the speech; the target bound at capture start is kept, or the fallback text box is used if it is no longer valid.");
                }
            }
        }
    }

    fn fail(&mut self, directory: Option<&Path>, error: String) {
        if let Some(directory) = directory {
            let mut pending = vec![directory.to_path_buf()];
            while let Some(directory) = pending.pop() {
                if let Ok(entries) = fs::read_dir(&directory) {
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
        let error = error.trim_end_matches('.').to_string();
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
        if let Some(directory) = directory {
            let path = directory.join("partial-result.txt");
            if let Ok(text) = fs::read_to_string(&path) {
                if !text.is_empty() {
                    self.snapshot.text = text;
                    self.snapshot.detail.push_str(&format!(". The capture was interrupted, so this text is incomplete: it is shown in the fallback box and stored at {}. This is not a completed transcription.", path.display()));
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}

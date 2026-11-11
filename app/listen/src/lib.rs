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
pub mod target;
#[cfg(windows)]
pub mod whisper;

pub type AppResult<T> = Result<T, String>;

/// The OS playback device boundary. Implementations never open a microphone.
pub trait PlaybackAudio: Send + Sync {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>>;
}

pub trait Recording: Send {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio>;
    fn failure(&self) -> Option<String> {
        None
    }
}

pub struct CapturedAudio {
    pub path: PathBuf,
    pub description: String,
}

/// A local executable/model boundary; there is no remote fallback.
pub trait LocalTranscriber: Send + Sync {
    fn check_ready(&self) -> AppResult<()>;
    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String>;
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
    /// happened partially; callers must retain the transcript and never retry it.
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
    pub delivery: Option<Delivery>,
}

struct Completed {
    text: String,
    description: String,
    delivery: Option<Delivery>,
    detail: String,
    save_error: Option<String>,
}

/// The application boundary used by both global hotkey and complete-flow tests.
pub struct Application {
    snapshot: Snapshot,
    audio: Arc<dyn PlaybackAudio>,
    transcriber: Arc<dyn LocalTranscriber>,
    runtime: PathBuf,
    recording: Option<Box<dyn Recording>>,
    session: Option<PathBuf>,
    completion: Option<mpsc::Receiver<AppResult<Completed>>>,
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
                detail: initial_error.unwrap_or_else(|| "Ready. Ctrl+Alt+R starts playback capture. Closing the window keeps Listen in the tray.".into()),
                text: fs::read_to_string(runtime.join("current-result.txt")).unwrap_or_default(),
                retained_audio: Vec::new(),
                delivery: None,
            },
            audio,
            transcriber,
            runtime,
            recording: None,
            session: None,
            completion: None,
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
                // Capture the actual selection before readiness checks or any UI feedback.
                self.bound_target = self
                    .targets
                    .as_ref()
                    .map_or(Ok(None), |targets| targets.focused_target());
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
                    Ok::<_, String>(())
                })();
                match result {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Capturing;
                        self.snapshot.delivery = None;
                        self.snapshot.detail =
                            "Capturing playback audio; press the hotkey again to stop.".into();
                    }
                    Err(error) => {
                        self.bound_target = Ok(None);
                        self.fail(error);
                    }
                }
            }
            Control::Stop if self.recording.is_some() => {
                let recording = self.recording.take().unwrap();
                let directory = self.session.clone().unwrap();
                let transcriber = self.transcriber.clone();
                let target = std::mem::replace(&mut self.bound_target, Ok(None));
                let current_result = self.runtime.join("current-result.txt");
                let (tx, rx) = mpsc::channel();
                self.completion = Some(rx);
                self.snapshot.phase = Phase::Processing;
                self.snapshot.detail = "Processing locally; no messages are sent.".into();
                thread::spawn(move || {
                    let result = recording.finish().and_then(|audio| {
                        let text = transcriber.transcribe(&audio, &directory)?;
                        if text.trim().is_empty() {
                            return Ok(Completed {
                                text,
                                description: audio.description,
                                delivery: None,
                                detail: String::new(),
                                save_error: None,
                            });
                        }
                        let save_error = fs::write(current_result, &text).err().map(|e| e.to_string());
                        let mut output_attempted = false;
                        let output = save_error.as_ref().map_or(Ok(()), |error| {
                            Err(format!("Could not save the current result: {error}"))
                        }).and_then(|()| target).and_then(|target| {
                            let Some(mut target) = target else {
                                return Err("No supported editable target was selected at capture start".into());
                            };
                            let latest = target.content()?;
                            let replacement = if latest.is_empty() {
                                text.clone()
                            } else {
                                format!("{latest}\n{text}")
                            };
                            output_attempted = true;
                            target.replace(&latest, &replacement)
                        });
                        let (delivery, detail) = match output {
                            Ok(()) => (Delivery::Appended, "The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.".into()),
                            Err(error) => {
                                let advice = if output_attempted {
                                    "Check the original target before copying; output is not retried automatically."
                                } else {
                                    "Review and copy it to the intended input."
                                };
                                (Delivery::Fallback, format!("{error}. The complete result is in the fallback text box. {advice}"))
                            }
                        };
                        Ok(Completed { text, description: audio.description, delivery: Some(delivery), detail, save_error })
                    });
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
        if let Some(error) = self
            .recording
            .as_ref()
            .and_then(|recording| recording.failure())
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
            Ok(completed) if completed.text.trim().is_empty() => {
                // A completed no-speech session is not a new transcript. In particular,
                // leave the previous copyable result and its on-disk copy untouched.
                match fs::remove_dir_all(self.session.as_ref().unwrap()) {
                    Ok(()) => {
                        self.snapshot.phase = Phase::NoSpeech;
                        self.snapshot.delivery = None;
                        self.snapshot.detail = format!(
                            "No valid speech detected; nothing was delivered. The previous current result is unchanged.\nPlayback: {}", completed.description
                        );
                    }
                    Err(error) => self.fail(format!(
                        "No valid speech detected, but temporary audio could not be cleaned: {error}"
                    )),
                }
            }
            Ok(completed) => {
                let result = completed.save_error.map_or_else(
                    || {
                        fs::remove_dir_all(self.session.as_ref().unwrap())
                            .map_err(|e| e.to_string())
                    },
                    Err,
                );
                self.snapshot.text = completed.text;
                self.snapshot.delivery = completed.delivery;
                match result {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Result;
                        self.snapshot.detail =
                            format!("{}\nPlayback: {}", completed.detail, completed.description);
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
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|extension| extension == "wav")
                        && !self.snapshot.retained_audio.contains(&path)
                    {
                        self.snapshot.retained_audio.push(path);
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
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}

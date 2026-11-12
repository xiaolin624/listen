use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
pub mod audio;
pub mod glossary;
#[cfg(windows)]
pub mod paste;
pub mod streaming;
#[cfg(windows)]
pub mod target;
#[cfg(windows)]
pub mod whisper;
#[cfg(windows)]
pub mod whisper_host;
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
    /// Start any first-use model preparation (for example loading a resident
    /// model). `None` means the transcriber is ready immediately. The receiver
    /// yields a user-visible backend note on success; capture must not start
    /// until it is ready.
    fn prepare(&self) -> Option<mpsc::Receiver<AppResult<String>>> {
        None
    }
    /// The backend actually in use, including an explicit fallback reason.
    fn backend_note(&self) -> Option<String> {
        None
    }
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
    /// First-use local model preparation is in progress and no capture has been
    /// started. Capture may still start; a capture started during preparation is
    /// `Capturing` while `Snapshot::preparation` stays `Preparation::Pending`.
    Preparing,
    Ready,
    Capturing,
    Processing,
    Result,
    NoSpeech,
    Failure,
}

/// First-use local model preparation, reported separately from `Phase` so that a
/// consumer can always distinguish "the model is still loading" from "ready",
/// including while capturing. A pending or failed preparation is never reported
/// as ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preparation {
    /// First-use preparation is still running in the background.
    Pending,
    /// The local model reported readiness; recognition is available.
    Ready,
    /// Preparation failed (or was never possible), so recognition is not available.
    Failed,
}

/// Measured stop-to-result breakdown. Every duration is wall-clock milliseconds;
/// `stop_to_result_ms` is measured from the Stop control to the moment the result
/// was saved and delivered (or the failure was recorded), not from the next GUI
/// refresh. `preparation_ms` covers first-use model loading only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    pub preparation_ms: Option<u64>,
    pub stop_to_result_ms: Option<u64>,
    pub finalize_ms: Option<u64>,
    pub recognition_ms: Option<u64>,
    pub last_window_wait_ms: Option<u64>,
    pub last_window_inference_ms: Option<u64>,
    pub save_ms: Option<u64>,
    pub delivery_ms: Option<u64>,
    pub recognition_windows: usize,
}

#[derive(Clone, Copy)]
pub enum Control {
    Start,
    Stop,
    Toggle,
    Retry,
    CleanupAudio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Appended,
    Fallback,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    /// Whether first-use model preparation is pending, finished or failed. This
    /// remains observable in every phase, including `Capturing`.
    pub preparation: Preparation,
    pub detail: String,
    pub text: String,
    pub retained_audio: Vec<PathBuf>,
    pub progress: CaptureProgress,
    pub delivery: Option<Delivery>,
    pub retry_available: bool,
    pub endpoint: Option<CaptureEndpoint>,
    pub backend: Option<String>,
    pub timing: Timing,
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

const READY_DETAIL: &str = "Ready. Ctrl+Alt+R captures only the default multimedia playback endpoint; audio routed only to the default communication endpoint or another device is outside this capture. Closing the window keeps Listen in the tray.";

/// Capture details always say whether the local model is still loading, so a
/// capture that started during preparation never reads like a ready capture.
fn preparation_note(preparation: Preparation) -> &'static str {
    match preparation {
        Preparation::Pending => {
            " The local recognition model is still preparing; audio is being buffered and recognition starts when it is ready."
        }
        Preparation::Ready | Preparation::Failed => "",
    }
}

fn ready_detail(note: Option<&str>) -> String {
    match note {
        Some(note) if !note.is_empty() => format!("{READY_DETAIL}\nBackend: {note}"),
        _ => READY_DETAIL.into(),
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

struct Completed {
    text: String,
    description: String,
    delivery: Option<Delivery>,
    detail: String,
    save_error: Option<String>,
    directory: PathBuf,
    timing: Timing,
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
        timing: Timing,
        /// Why an automatic local retry cannot cover this failure even though some
        /// audio remains on disk. Shown to the user instead of silently omitting
        /// the retry action.
        retry_scope: Option<String>,
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
    preparation: Option<mpsc::Receiver<AppResult<String>>>,
    preparation_started: Option<Instant>,
    backend: Option<String>,
    glossary_path: PathBuf,
    glossary: glossary::Glossary,
    glossary_note: Option<String>,
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
        let backend = transcriber.backend_note();
        let mut preparation = None;
        let mut preparation_started = None;
        let (phase, preparation_state, detail) = match &initial_error {
            Some(error) => (Phase::Failure, Preparation::Failed, error.clone()),
            None => match transcriber.prepare() {
                Some(receiver) => {
                    preparation = Some(receiver);
                    preparation_started = Some(Instant::now());
                    (
                        Phase::Preparing,
                        Preparation::Pending,
                        "Preparing the local recognition model in the background; capture can start now and its audio is buffered until the model reports Ready.".into(),
                    )
                }
                None => (
                    Phase::Ready,
                    Preparation::Ready,
                    ready_detail(backend.as_deref()),
                ),
            },
        };
        let backend = transcriber.backend_note();
        let glossary_path = glossary::default_path(&runtime);
        Self {
            snapshot: Snapshot {
                phase,
                preparation: preparation_state,
                detail,
                text: fs::read_to_string(runtime.join("current-result.txt")).unwrap_or_default(),
                retained_audio: Vec::new(),
                progress: CaptureProgress::default(),
                delivery: None,
                retry_available: false,
                endpoint: None,
                backend: backend.clone(),
                timing: Timing::default(),
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
            preparation,
            preparation_started,
            backend,
            glossary_path,
            glossary: glossary::Glossary::default(),
            glossary_note: None,
        }
    }

    /// Use a specific 术语表 file for this application. The default is
    /// `<runtime>/glossary.txt`, or the process default set from `--glossary`.
    pub fn with_glossary(mut self, path: impl Into<PathBuf>) -> Self {
        self.glossary_path = path.into();
        self
    }

    /// Read the glossary again so an edit applies to this capture without a
    /// restart. A malformed file is reported and never loses the capture: the
    /// transcript is still delivered, only without correction.
    fn reload_glossary(&mut self) {
        match glossary::Glossary::load(&self.glossary_path) {
            Ok(loaded) => {
                self.glossary = loaded;
                self.glossary_note = None;
            }
            Err(error) => {
                self.glossary = glossary::Glossary::default();
                self.glossary_note = Some(format!(
                    "Glossary correction is unavailable: {error}. The uncorrected transcript is still delivered."
                ));
            }
        }
    }

    fn with_glossary_note(&self, detail: String) -> String {
        match &self.glossary_note {
            Some(note) => format!("{detail}\n{note}"),
            None => detail,
        }
    }

    pub fn with_targets(mut self, targets: Arc<dyn TargetSelection>) -> Self {
        self.targets = Some(targets);
        self
    }

    /// What the application is doing right now, phrased for refusal messages that
    /// must not claim "nothing is being captured" while a capture is running.
    fn activity(&self) -> &'static str {
        if self.recording.is_some() {
            "Playback is being captured and buffered"
        } else if self.completion.is_some() {
            "The previous capture is still being processed locally"
        } else {
            "Nothing is being captured"
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
                    // The glossary is edited by hand between captures; re-read it
                    // at every capture start rather than at process start.
                    self.reload_glossary();
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
                        self.snapshot.detail = self.with_glossary_note(format!(
                            "{}{}",
                            capture_scope(self.snapshot.endpoint.as_ref()),
                            preparation_note(self.snapshot.preparation)
                        ));
                        self.snapshot.timing = Timing {
                            preparation_ms: self.snapshot.timing.preparation_ms,
                            ..Timing::default()
                        };
                    }
                    Err(error) => {
                        self.bound_target = Ok(None);
                        let directory = self.session.take();
                        self.fail(directory.as_deref(), error);
                    }
                }
            }
            Control::Stop if self.recording.is_some() => {
                let stop_at = Instant::now();
                let preparation_ms = self.snapshot.timing.preparation_ms;
                let recording = self.recording.take().unwrap();
                let directory = self.session.take().unwrap();
                let transcriber = self.transcriber.clone();
                let recognition = self.recognition.take();
                let streaming = recognition.is_some();
                let progress = self.progress.clone();
                let target = std::mem::replace(&mut self.bound_target, Ok(None));
                let current_result = self.runtime.join("current-result.txt");
                let glossary = self.glossary.clone();
                let (tx, rx) = mpsc::channel();
                self.completion = Some(rx);
                self.snapshot.phase = Phase::Processing;
                self.snapshot.detail = "Processing locally; no messages are sent.".into();
                thread::spawn(move || {
                    let finalize_started = Instant::now();
                    let captured = recording.finish();
                    let finalize_ms = elapsed_ms(finalize_started);
                    let recognition_started = Instant::now();
                    let recognized = recognition.map(|worker| worker.finish());
                    let recognition_ms = elapsed_ms(recognition_started);
                    let mut timing = Timing {
                        preparation_ms,
                        finalize_ms: Some(finalize_ms),
                        recognition_ms: Some(recognition_ms),
                        ..Timing::default()
                    };
                    let audio = match captured {
                        Ok(audio) => audio,
                        Err(capture_error) => {
                            let error = match &recognized {
                                Some(Err(recognition_error)) => {
                                    format!("{recognition_error}; {capture_error}")
                                }
                                _ => capture_error,
                            };
                            timing.stop_to_result_ms = Some(elapsed_ms(stop_at));
                            let _ = tx.send(Completion::Failed {
                                error,
                                directory,
                                retry: None,
                                timing,
                                retry_scope: None,
                            });
                            return;
                        }
                    };
                    let (recognized, window_timing) = match recognized {
                        Some(Ok(outcome)) => (Ok(outcome.text), Some(outcome.timing)),
                        Some(Err(error)) => (Err(error), None),
                        None => {
                            let started = Instant::now();
                            let result = transcriber.transcribe(&audio, &directory);
                            let elapsed = elapsed_ms(started);
                            timing.recognition_ms = Some(elapsed);
                            (
                                result,
                                Some(streaming::RecognitionTiming {
                                    wait_ms: 0,
                                    inference_ms: elapsed,
                                    windows: 1,
                                }),
                            )
                        }
                    };
                    if let Some(window) = &window_timing {
                        timing.last_window_wait_ms = Some(window.wait_ms);
                        timing.last_window_inference_ms = Some(window.inference_ms);
                        timing.recognition_windows = window.windows;
                    }
                    // Correct only the final merged text, after recognition and
                    // segmentation are finished: timestamps and window boundaries are
                    // never touched by the glossary.
                    let recognized = recognized.map(|text| glossary.correct(&text));
                    let completion = match recognized {
                        // Only an unfragmented capture can be retried as a whole. A
                        // streamed session that has already consumed windows no longer
                        // has their audio on disk, so its checkpoint stays a partial
                        // result instead of pretending the remaining windows are whole.
                        Err(error) => {
                            let recognized_segments = progress
                                .as_ref()
                                .map(|progress| progress.snapshot().recognized_segments)
                                .unwrap_or(0);
                            let retained = if audio.path.is_file() {
                                Some(CapturedAudio {
                                    path: audio.path.clone(),
                                    description: audio.description.clone(),
                                })
                            } else if streaming && recognized_segments == 0 {
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
                            // A streamed session whose earlier windows were already
                            // recognized and released cannot be retried as a whole.
                            // Say so explicitly; the remaining audio paths are still
                            // listed by `fail` for manual recovery.
                            let retry_scope = if retry.is_none()
                                && streaming
                                && recognized_segments > 0
                            {
                                Some(format!(
                                    "This capture was partially recognized before the failure ({recognized_segments} window(s) had been checkpointed). Those windows' audio was released after recognition, so automatic local retry is only offered when no recognition window has completed; it is not offered here. The recognized prefix above is incomplete, and the remaining audio paths are listed for manual recovery."
                                ))
                            } else {
                                None
                            };
                            timing.stop_to_result_ms = Some(elapsed_ms(stop_at));
                            Completion::Failed {
                                error,
                                directory,
                                retry,
                                timing,
                                retry_scope,
                            }
                        }
                        // Silence and non-speech leave the previous result and every
                        // target untouched; nothing is saved or appended for them.
                        Ok(text) if text.trim().is_empty() => {
                            timing.stop_to_result_ms = Some(elapsed_ms(stop_at));
                            Completion::Finished(Completed {
                                text,
                                description: audio.description,
                                delivery: None,
                                detail: String::new(),
                                save_error: None,
                                directory,
                                timing,
                            })
                        }
                        Ok(text) => {
                            let save_started = Instant::now();
                            let save_error = fs::write(&current_result, &text)
                                .err()
                                .map(|e| e.to_string());
                            timing.save_ms = Some(elapsed_ms(save_started));
                            let delivery_started = Instant::now();
                            let (delivery, detail) = match &save_error {
                                // Saving first means a storage failure never changes a target.
                                Some(error) => not_delivered(format!(
                                    "Could not save the current result: {error}"
                                )),
                                None => deliver(target, &text),
                            };
                            timing.delivery_ms = Some(elapsed_ms(delivery_started));
                            timing.stop_to_result_ms = Some(elapsed_ms(stop_at));
                            Completion::Finished(Completed {
                                text,
                                description: audio.description,
                                delivery: Some(delivery),
                                detail,
                                save_error,
                                directory,
                                timing,
                            })
                        }
                    };
                    let _ = tx.send(completion);
                });
            }
            Control::Retry => {
                if self.preparation.is_some() {
                    let activity = self.activity();
                    self.snapshot.detail = format!(
                        "The local recognition model is still being prepared; wait until Listen reports Ready before retrying. {activity} and the current result is unchanged."
                    );
                    return;
                }
                if self.recording.is_some() {
                    self.snapshot.detail = "Still capturing the current playback locally; stop it and wait for the result before retrying.".into();
                    return;
                }
                if self.completion.is_some() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before retrying.".into();
                    return;
                }
                self.retry_local_recognition();
            }
            Control::CleanupAudio => {
                if self.preparation.is_some() {
                    let activity = self.activity();
                    self.snapshot.detail = format!(
                        "The local recognition model is still being prepared; wait until Listen reports Ready before cleaning failed audio. {activity} and the current result is unchanged."
                    );
                    return;
                }
                if self.recording.is_some() {
                    self.snapshot.detail = "Still capturing the current playback locally; stop it and wait for the result before cleaning failed audio.".into();
                    return;
                }
                if self.completion.is_some() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before cleaning failed audio.".into();
                    return;
                }
                self.cleanup_failed_audio();
            }
            _ if self.preparation.is_some() => {
                let activity = self.activity();
                self.snapshot.detail = format!(
                    "The local recognition model is still being prepared; {activity}. The current result is unchanged."
                );
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
        let glossary = self.glossary.clone();
        let (tx, rx) = mpsc::channel();
        self.completion = Some(rx);
        self.snapshot.phase = Phase::Processing;
        self.snapshot.delivery = None;
        self.snapshot.retry_available = false;
        self.snapshot.detail =
            "Retrying recognition locally on the retained audio; no replay is needed and no messages are sent.".into();
        thread::spawn(move || {
            let started = Instant::now();
            let recognized = transcriber
                .check_ready()
                .and_then(|()| transcriber.transcribe(&audio, &directory))
                .map(|text| {
                    // The retry must deliver the same corrected text as the original
                    // path; it uses the glossary loaded when this capture started.
                    glossary.correct(&text)
                });
            let elapsed = elapsed_ms(started);
            let timing = Timing {
                recognition_ms: Some(elapsed),
                last_window_inference_ms: Some(elapsed),
                recognition_windows: 1,
                ..Timing::default()
            };
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
                        timing,
                        retry_scope: None,
                    }
                }
                Ok(text) if text.trim().is_empty() => Completion::Finished(Completed {
                    text,
                    description: audio.description,
                    delivery: None,
                    detail: String::new(),
                    save_error: None,
                    directory,
                    timing,
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
                            timing,
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
                            timing,
                            retry_scope: None,
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

    /// Report a failed first-use preparation. A capture that was already running is
    /// stopped and its received audio is retained: a preparation failure is a real
    /// failure, never a ready state and never a silently discarded capture.
    fn fail_preparation(&mut self, error: String) {
        self.preparation = None;
        self.preparation_started = None;
        self.snapshot.preparation = Preparation::Failed;
        if self.recording.is_some() {
            self.abort_capture_for_failed_preparation(error);
            return;
        }
        if self.snapshot.phase == Phase::Preparing {
            self.snapshot.phase = Phase::Failure;
            self.snapshot.detail = error;
        }
    }

    /// Stop a capture because the model it is waiting for cannot become ready. The
    /// windows the worker has not decoded yet stay on disk, the recording is
    /// finalized, and the existing retained-audio/retry/cleanup controls are
    /// offered exactly as for a failed recognition.
    fn abort_capture_for_failed_preparation(&mut self, error: String) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let directory = self.session.take();
        let recognition = self.recognition.take();
        let streaming = recognition.is_some();
        let progress = self.progress.take();
        if let Some(progress) = &progress {
            progress.cancel();
        }
        let target = std::mem::replace(&mut self.bound_target, Ok(None));

        // Finalize the recording first so its retained WAV is complete, then stop
        // the worker; cancellation keeps the windows it has not decoded.
        let captured = recording.finish();
        let _ = recognition.map(|worker| worker.finish());
        let recognized_segments = progress
            .as_ref()
            .map(|progress| progress.snapshot().recognized_segments)
            .unwrap_or(0);

        let (capture_error, kept) = match captured {
            Ok(audio) => {
                let kept = if audio.path.is_file() {
                    Some(CapturedAudio {
                        path: audio.path.clone(),
                        description: audio.description.clone(),
                    })
                } else if streaming && recognized_segments == 0 {
                    directory
                        .as_deref()
                        .and_then(|directory| streaming::retained_audio_path(directory).ok())
                        .map(|path| CapturedAudio {
                            path,
                            description: audio.description.clone(),
                        })
                } else {
                    None
                };
                (None, kept)
            }
            Err(capture_error) => (Some(capture_error), None),
        };
        let retryable = kept.is_some();
        self.retry = match (kept, directory.clone()) {
            (Some(audio), Some(directory)) => Some(RetainedCapture {
                audio,
                directory,
                target,
            }),
            _ => None,
        };
        self.snapshot.retry_available = retryable;
        let mut message = format!(
            "The local recognition model failed to prepare: {error}. The capture in progress was stopped; nothing was transcribed or delivered."
        );
        if let Some(capture_error) = capture_error {
            message.push_str(&format!(
                " Finalizing the capture also failed: {capture_error}"
            ));
        }
        self.fail(directory.as_deref(), message);
        if retryable {
            self.snapshot.detail.push_str("\nThe audio from this capture is retained. Local retry recognizes the same audio again without replaying the speech; the target bound at capture start is kept, or the fallback text box is used if it is no longer valid.");
        } else if streaming && recognized_segments > 0 {
            self.snapshot.detail.push_str(&format!(
                "\nThis capture was partially recognized before the preparation failure ({recognized_segments} window(s) had been checkpointed). Those windows' audio was released after recognition, so automatic local retry is not offered here; the remaining audio paths are listed for manual recovery."
            ));
        }
    }

    pub fn poll(&mut self) {
        if self.preparation.is_some() {
            let received = self.preparation.as_ref().unwrap().try_recv();
            match received {
                Ok(Ok(note)) => {
                    self.snapshot.timing.preparation_ms =
                        self.preparation_started.map(|started| elapsed_ms(started));
                    self.preparation = None;
                    self.preparation_started = None;
                    self.snapshot.preparation = Preparation::Ready;
                    // A capture started during preparation keeps its own phase and
                    // detail; readiness is reported through `Snapshot::preparation`.
                    if self.snapshot.phase == Phase::Preparing {
                        self.snapshot.phase = Phase::Ready;
                        self.snapshot.detail = ready_detail(Some(&note));
                    } else if self.snapshot.phase == Phase::Capturing {
                        // Drop the "still preparing" note without waiting for the
                        // next progress update (non-streamed captures do not have one).
                        self.snapshot.detail = self.with_glossary_note(format!(
                            "{}{}",
                            capture_scope(self.snapshot.endpoint.as_ref()),
                            preparation_note(self.snapshot.preparation)
                        ));
                    }
                }
                Ok(Err(error)) => {
                    self.fail_preparation(error);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Preparation is still pending. A capture may already be
                    // running (and buffering), so keep polling progress, device
                    // failures and completions below instead of returning early.
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.fail_preparation(
                        "Local model preparation stopped unexpectedly, so the local model is not ready."
                            .into(),
                    );
                    return;
                }
            }
        }
        if let Some(note) = self.transcriber.backend_note() {
            if self.backend.as_deref() != Some(note.as_str()) {
                self.backend = Some(note.clone());
                self.snapshot.backend = Some(note);
            }
        }
        if let Some(progress) = self.progress.as_ref().map(|p| p.snapshot()).or_else(|| {
            self.recording
                .as_ref()
                .and_then(|recording| recording.progress())
        }) {
            self.snapshot.progress = progress;
        }
        if self.snapshot.phase == Phase::Capturing && self.recognition.is_some() {
            let p = &self.snapshot.progress;
            let detail = format!(
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
            let mut detail = detail;
            detail.push_str(preparation_note(self.snapshot.preparation));
            self.snapshot.detail = self.with_glossary_note(detail);
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
                self.snapshot.timing = completed.timing.clone();
                self.snapshot.delivery = None;
                self.record_latency("NoSpeech", &completed.timing, &completed.directory);
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                match fs::remove_dir_all(&completed.directory) {
                    Ok(()) => {
                        self.snapshot.phase = Phase::NoSpeech;
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
                self.snapshot.timing = completed.timing.clone();
                self.snapshot.text = completed.text;
                self.snapshot.delivery = completed.delivery;
                self.record_latency("Result", &completed.timing, &completed.directory);
                let cleanup = if let Some(error) = &completed.save_error {
                    // A storage failure keeps the audio recoverable, as before.
                    Err(error.clone())
                } else {
                    fs::remove_dir_all(&completed.directory).map_err(|e| e.to_string())
                };
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                match cleanup {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Result;
                        self.snapshot.detail = self.with_glossary_note(format!(
                            "{}\nPlayback: {}",
                            completed.detail, completed.description
                        ));
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
                timing,
                retry_scope,
            } => {
                let retryable = retry.is_some();
                self.retry = retry;
                self.snapshot.retry_available = retryable;
                self.snapshot.timing = timing.clone();
                self.record_latency("Failure", &timing, &directory);
                self.fail(Some(&directory), error);
                if retryable {
                    self.snapshot.detail.push_str("\nThe audio from this capture is retained. Local retry recognizes the same audio again without replaying the speech; the target bound at capture start is kept, or the fallback text box is used if it is no longer valid.");
                } else if let Some(scope) = retry_scope {
                    self.snapshot.detail.push_str("\n");
                    self.snapshot.detail.push_str(&scope);
                }
            }
        }
    }

    /// Append one machine-readable latency record. The session directory is
    /// removed after success, so this keeps the measured breakdown in the
    /// runtime directory for later review instead of relying on GUI timing.
    fn record_latency(&self, phase: &str, timing: &Timing, directory: &Path) {
        use std::io::Write;
        let entry = serde_json::json!({
            "utc": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or_default(),
            "session": directory.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            "phase": phase,
            "backend": self.snapshot.backend,
            "delivery": format!("{:?}", self.snapshot.delivery),
            "text_chars": self.snapshot.text.chars().count(),
            "preparation_ms": timing.preparation_ms,
            "stop_to_result_ms": timing.stop_to_result_ms,
            "finalize_ms": timing.finalize_ms,
            "recognition_ms": timing.recognition_ms,
            "last_window_wait_ms": timing.last_window_wait_ms,
            "last_window_inference_ms": timing.last_window_inference_ms,
            "save_ms": timing.save_ms,
            "delivery_ms": timing.delivery_ms,
            "recognition_windows": timing.recognition_windows,
        });
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.runtime.join("latency.jsonl"))
        {
            let _ = writeln!(file, "{entry}");
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

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
pub mod audio;
pub mod cloud;
#[cfg(windows)]
pub mod deepseek;
pub mod glossary;
#[cfg(windows)]
pub mod paste;
pub mod recognition;
#[cfg(windows)]
pub mod sensevoice;
pub mod streaming;
#[cfg(windows)]
pub mod target;
#[cfg(windows)]
pub mod whisper;
#[cfg(windows)]
pub mod whisper_host;
#[cfg(windows)]
pub mod whisper_timing;
use recognition::{
    LocalSessions, RecognitionSession, RecognitionSessions, SessionEvent, SessionId,
    SessionRequest, SessionResult, SessionState, SessionUpdate,
};
use streaming::{CaptureProgress, SegmentStream};

pub type AppResult<T> = Result<T, String>;

/// The OS playback device boundary. Implementations never open a microphone.
pub trait PlaybackAudio: Send + Sync {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>>;
}

pub trait Recording: Send {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio>;
    /// Continuous, non-overlapping 16 kHz mono signed little-endian PCM. A cloud
    /// adapter must refuse a device that only supplies local recognition windows.
    fn take_pcm_stream(&mut self) -> Option<cloud::PcmStream> {
        None
    }
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

#[derive(Clone)]
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
    /// yields a user-visible backend note on success. Capture may start before
    /// preparation finishes; its recognition waits for this outcome instead of
    /// decoding audio against an unprepared model.
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

/// The optional 精修 (refine) boundary: text in, corrected text out.
///
/// This optional text-only path is separate from explicit cloud recognition.
/// Its implementation is handed one transcript
/// string and nothing else. It never sees the captured audio, a file path, the
/// runtime directory or any other transcript. Implementations report availability
/// without doing network work, so a missing API key disables the entry before the
/// user can press it.
pub trait RefineText: Send + Sync {
    /// `Ok(())` when this implementation can run at all (for example a key exists).
    fn availability(&self) -> Result<(), String>;
    /// Correct one complete transcript. Only this text leaves the machine.
    fn refine(&self, text: &str) -> AppResult<String>;
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
    /// `Control::Start`/hotkey dispatch to `Phase::Capturing`, including binding the
    /// target and starting the device. #15's target is under 1 s.
    pub capture_start_ms: Option<u64>,
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
    /// 精修: ask the configured text-refine port to correct the complete current
    /// result and replace only the transcript that was appended from it.
    Refine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Appended,
    Fallback,
}

/// The always-observable 精修 state. It is reported next to, not instead of, `Phase`:
/// refining never changes what capture, processing or a result mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refine {
    /// Nothing can be refined right now; the reason is user-readable and never
    /// contains a credential.
    Unavailable(String),
    /// A complete result exists and the port is able to run.
    Available,
    /// A refine request is in flight. The current result is still the current result.
    Running,
    /// The refined text replaced the current result and, when a target existed, the
    /// appended transcript that the target still held.
    Applied,
    /// The refined text is valid but was deliberately not written to the target; it
    /// is exposed as `Snapshot::refined_text` and the user's target is untouched.
    NotApplied(String),
    /// The refine request itself failed. The complete result is unchanged.
    Failed(String),
}

impl Refine {
    /// The stable `status.txt` spelling. Reasons live in `detail` and, in the UI, in
    /// the panel's status block.
    pub fn name(&self) -> &'static str {
        match self {
            Refine::Unavailable(_) => "unavailable",
            Refine::Available => "available",
            Refine::Running => "running",
            Refine::Applied => "applied",
            Refine::NotApplied(_) => "not_applied",
            Refine::Failed(_) => "failed",
        }
    }

    /// Why the refine entry is not (or no longer) usable, if there is a reason.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Refine::Unavailable(reason) | Refine::NotApplied(reason) | Refine::Failed(reason) => {
                Some(reason)
            }
            Refine::Running => Some("a refine request is already running"),
            Refine::Applied => Some("the refined text was already applied to this result"),
            Refine::Available => None,
        }
    }
}

/// No refine implementation was connected at all; the entry stays unavailable and
/// local recognition is unaffected.
const NO_REFINE_PORT: &str =
    "No DeepSeek refine connector is configured, so the current result cannot be refined.";

/// A condition outside the capture state machine that the user must still see.
/// It is reported separately from [`Snapshot::detail`] so a live capture's 100 ms
/// progress rewrite cannot erase it, and from [`Phase`] because no phase changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The clipboard copy failed. The previous clipboard content was not replaced,
    /// so the user must not assume the result is pasteable.
    CopyFailed(String),
    /// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` failed, so the resident
    /// status capsule is visible to screen sharing and recordings.
    CaptureExcluded(String),
}

impl Notice {
    /// The one user-facing sentence. It always names the consequence, not just the
    /// platform error.
    pub fn text(&self) -> String {
        match self {
            Notice::CopyFailed(reason) => format!(
                "Could not copy the result: {reason}. The clipboard was not replaced; the complete result is still in the panel and can be selected and copied by hand."
            ),
            Notice::CaptureExcluded(reason) => format!(
                "The status capsule could not be hidden from screen capture ({reason}), so screen sharing and recordings will show it. Capture and recognition are unaffected."
            ),
        }
    }
}

/// Line endings are not part of a target's meaning: editors may report the same
/// content with CRLF or LF, so every refine comparison uses this form.
fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n")
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    /// Whether first-use model preparation is pending, finished or failed. This
    /// remains observable in every phase, including `Capturing`.
    pub preparation: Preparation,
    /// Recognition progress is separate from complete, deliverable text.
    pub recognition: Option<SessionState>,
    pub detail: String,
    pub text: String,
    pub retained_audio: Vec<PathBuf>,
    pub progress: CaptureProgress,
    pub delivery: Option<Delivery>,
    pub retry_available: bool,
    pub endpoint: Option<CaptureEndpoint>,
    pub backend: Option<String>,
    pub timing: Timing,
    /// The 精修 state. Observable in every phase, including before any result exists,
    /// so a missing key or an in-flight refine never has to be guessed from the UI.
    pub refine: Refine,
    /// The refined text when it must not be written to the target (the target was
    /// edited, or could not be confirmed unchanged). It is shown in the expanded
    /// panel while `text` keeps the complete result.
    pub refined_text: Option<String>,
    /// A user-visible problem outside the capture flow (a failed clipboard copy, a
    /// capsule that cannot be hidden from screen capture). `None` when there is
    /// nothing to report.
    pub notice: Option<Notice>,
}

impl Snapshot {
    /// Display metadata only; budget and delivery never depend on this label.
    pub fn cloud_trial(&self) -> bool {
        self.backend
            .as_deref()
            .is_some_and(|backend| backend.starts_with("Cloud trial:"))
    }
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

const READY_DETAIL: &str = "Ready. Ctrl+Alt+R captures only the default multimedia playback endpoint; audio routed only to the default communication endpoint or another device is outside this capture. Closing the result panel keeps Listen running in the tray.";

/// The honest local-processing detail while the stopped capture waits for the
/// still-loading model. Nothing has been delivered or failed yet.
const PROCESSING_WHILE_PREPARING_DETAIL: &str = "The local recognition model is still being prepared. The buffered audio will be recognized when the model is ready; no messages are sent.";

/// The same waiting state after the model reported readiness but recognition is
/// still running.
const PROCESSING_READY_DETAIL: &str = "The local recognition model is ready. The buffered audio is being recognized locally; no messages are sent.";

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
    /// What a successful append left behind, so the same appended transcript can be
    /// replaced later without guessing. Only a fully confirmed append sets it.
    memory: Option<DeliveryMemory>,
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

/// What a successful append left behind. The bound target is kept alive so a later
/// refine can read it again through the same adapter; `before` is the complete
/// content read before the append, `after` is the content written, and `appended` is
/// the transcript itself. A refine may only replace the appended transcript when the
/// target still holds exactly `after`.
struct DeliveryMemory {
    target: Box<dyn Target>,
    before: String,
    after: String,
    appended: String,
}

/// One delivery attempt's user-visible outcome. A complete append is only reported
/// after the target contract confirms that the whole replacement was read back.
struct DeliveryOutcome {
    delivery: Delivery,
    detail: String,
    /// Present only for a confirmed append. Fallback deliveries have no target to
    /// read again, so they can never be replaced in place.
    memory: Option<DeliveryMemory>,
}

impl DeliveryOutcome {
    fn fallback(detail: String) -> Self {
        Self {
            delivery: Delivery::Fallback,
            detail,
            memory: None,
        }
    }
}

fn deliver(target: AppResult<Option<Box<dyn Target>>>, text: &str) -> DeliveryOutcome {
    let mut target = match target {
        Ok(Some(target)) => target,
        Ok(None) => {
            return DeliveryOutcome::fallback(not_delivered(
                "No supported editable target was selected at capture start".into(),
            ));
        }
        Err(error) => return DeliveryOutcome::fallback(not_delivered(error)),
    };
    let latest = match target.content() {
        Ok(latest) => latest,
        Err(error) => return DeliveryOutcome::fallback(not_delivered(error)),
    };
    let replacement = if latest.is_empty() {
        text.to_string()
    } else {
        format!("{latest}\n{text}")
    };
    match target.replace(&latest, &replacement) {
        Ok(()) => DeliveryOutcome {
            delivery: Delivery::Appended,
            detail: "The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.".into(),
            memory: Some(DeliveryMemory {
                target,
                before: latest,
                after: replacement,
                appended: text.to_string(),
            }),
        },
        Err(error) => {
            // Diagnose the failed write by reading the target again. Reading is not a
            // retry: the write is never repeated and the previous text is never restored.
            let observed = target.content();
            let (delivery, detail) = failed_write(&latest, &replacement, &error, observed);
            DeliveryOutcome {
                delivery,
                detail,
                memory: None,
            }
        }
    }
}

/// The result of one refine request. The delivery memory was moved to the worker so
/// the target read, replace and read-back all happen off the UI thread; a target that
/// must not be written leaves the current result untouched.
enum RefineOutcome {
    Applied { refined: String, note: String },
    NotApplied { refined: String, reason: String },
    Failed { reason: String },
}

/// Run one refine and, when it is safe, replace only the appended transcript. This
/// function is the whole backfill safety boundary:
///
/// * the refine port only ever receives `text` (the local 转写文本);
/// * a target is written only when it still holds exactly the content delivery wrote
///   and the appended transcript is still the current result;
/// * the write is confirmed by reading it back before success is claimed;
/// * any failure keeps the complete result and never re-appends or restores.
fn refine_delivery(
    refine: Arc<dyn RefineText>,
    text: String,
    memory: Option<DeliveryMemory>,
    current_result: PathBuf,
) -> RefineOutcome {
    let refined = match refine.refine(&text) {
        Ok(refined) => refined.trim().to_string(),
        Err(error) => {
            return RefineOutcome::Failed {
                reason: format!("the refine request failed: {error}"),
            };
        }
    };
    if refined.is_empty() {
        return RefineOutcome::Failed {
            reason:
                "the refine service returned an empty correction, so the current result is kept"
                    .into(),
        };
    }
    let Some(mut memory) = memory else {
        // Nothing was ever appended (the result was a fallback), so there is no
        // target to protect: the refined text simply becomes the current result.
        return match fs::write(&current_result, &refined) {
            Ok(()) => RefineOutcome::Applied {
                refined,
                note: "There was no writable target input to replace, so the refined text became the current result and is shown in the panel; nothing was written to any control.".into(),
            },
            Err(error) => RefineOutcome::Failed {
                reason: format!(
                    "the refined text could not be saved as the current result: {error}"
                ),
            },
        };
    };
    if memory.appended != text {
        // Defensive: only replace a target that still belongs to the article being
        // refined. A mismatch means the memory no longer describes this result.
        return RefineOutcome::NotApplied {
            refined,
            reason:
                "the current result is not the transcript that was delivered, so the target was not written"
                    .into(),
        };
    }
    let observed = match memory.target.content() {
        Ok(observed) => observed,
        Err(error) => {
            return RefineOutcome::NotApplied {
                refined,
                reason: format!(
                    "the original target could not be read back to confirm it is unchanged ({error}), so it was not written"
                ),
            };
        }
    };
    if normalize_newlines(&observed) != normalize_newlines(&memory.after) {
        return RefineOutcome::NotApplied {
            refined,
            reason:
                "the original target was edited after delivery, so it was not overwritten and the refined text stays in the panel"
                    .into(),
        };
    }
    let replacement = if memory.before.is_empty() {
        refined.clone()
    } else {
        format!("{}\n{}", memory.before, refined)
    };
    if let Err(error) = memory.target.replace(&observed, &replacement) {
        return RefineOutcome::NotApplied {
            refined,
            reason: format!(
                "writing the refined text to the original target failed ({error}), so the target was left as it was and the refined text stays in the panel"
            ),
        };
    }
    match memory.target.content() {
        Ok(readback) if normalize_newlines(&readback) == normalize_newlines(&replacement) => {
            let mut note = "The refined text replaced the appended transcript in the original target and was read back in full; review and send it manually.".to_string();
            if let Err(error) = fs::write(&current_result, &refined) {
                note.push_str(&format!(
                    " The refined text is the current result here, but it could not be saved to current-result.txt ({error})."
                ));
            }
            RefineOutcome::Applied { refined, note }
        }
        Ok(_) => RefineOutcome::NotApplied {
            refined,
            reason:
                "the original target did not confirm the complete refined text, so no complete write is claimed and the current result is kept"
                    .into(),
        },
        Err(error) => RefineOutcome::NotApplied {
            refined,
            reason: format!(
                "the refined text could not be confirmed in the original target ({error}), so the current result is kept; the target is not written again"
            ),
        },
    }
}

/// Nothing was sent to the target because selection, reading, or saving failed first.
fn not_delivered(reason: String) -> String {
    format!(
        "{reason}. No text was written to any control. The complete result is in the expanded result panel. Review and copy it to the intended input."
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
    let fallback = "The complete result is in the expanded result panel.";
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
struct ActiveSession {
    id: SessionId,
    directory: PathBuf,
    handle: Box<dyn RecognitionSession>,
    stopped: bool,
    terminal: bool,
    stop_requested: Option<Instant>,
    retry_audio: Option<CapturedAudio>,
}

pub struct Application {
    snapshot: Snapshot,
    sessions: Box<dyn RecognitionSessions>,
    runtime: PathBuf,
    active: Option<ActiveSession>,
    next_session_id: u64,
    completion: Option<mpsc::Receiver<Completion>>,
    retry: Option<RetainedCapture>,
    control_error: Option<String>,
    targets: Option<Arc<dyn TargetSelection>>,
    bound_target: AppResult<Option<Box<dyn Target>>>,
    glossary_paths: Vec<PathBuf>,
    glossary: glossary::Glossary,
    glossary_note: Option<String>,
    refine_port: Option<Arc<dyn RefineText>>,
    refine_job: Option<mpsc::Receiver<RefineOutcome>>,
    delivery_memory: Option<DeliveryMemory>,
}

impl Application {
    /// The default remains entirely local. Only an explicit Refine control can
    /// use the separately configured text-refine adapter.
    pub fn new(
        audio: Arc<dyn PlaybackAudio>,
        transcriber: Arc<dyn LocalTranscriber>,
        runtime: PathBuf,
        hotkey_registration: AppResult<()>,
    ) -> Self {
        Self::with_recognition_sessions(
            Box::new(LocalSessions::new(audio, transcriber)),
            runtime,
            hotkey_registration,
        )
    }

    /// Use a recognition-session adapter while retaining the same application
    /// controls, persistence, safe target delivery and result panel.
    pub fn with_recognition_sessions(
        mut sessions: Box<dyn RecognitionSessions>,
        runtime: PathBuf,
        hotkey_registration: AppResult<()>,
    ) -> Self {
        let control_error = hotkey_registration.err();
        let readiness = sessions.preparation();
        let initial_error = control_error
            .clone()
            .map(|error| format!("Global hotkey unavailable: {error}"))
            .or(readiness.error);
        let preparation = if initial_error.is_some() {
            Preparation::Failed
        } else {
            readiness.state
        };
        let (phase, detail) = match initial_error {
            Some(error) => (Phase::Failure, error),
            None if preparation == Preparation::Pending => (
                Phase::Preparing,
                "Preparing the local recognition model in the background; capture can start now and its audio is buffered until the model reports Ready.".into(),
            ),
            None => (Phase::Ready, ready_detail(readiness.backend.as_deref())),
        };
        let glossary_paths =
            glossary::default_paths(&runtime, std::env::current_exe().ok().as_deref());
        Self {
            snapshot: Snapshot {
                phase,
                preparation,
                recognition: None,
                detail,
                text: fs::read_to_string(runtime.join("current-result.txt")).unwrap_or_default(),
                retained_audio: Vec::new(),
                progress: CaptureProgress::default(),
                delivery: None,
                retry_available: false,
                endpoint: None,
                backend: readiness.backend,
                timing: Timing {
                    preparation_ms: readiness.elapsed_ms,
                    ..Timing::default()
                },
                refine: Refine::Unavailable(NO_REFINE_PORT.into()),
                refined_text: None,
                notice: None,
            },
            sessions,
            runtime,
            active: None,
            next_session_id: 0,
            completion: None,
            retry: None,
            control_error,
            targets: None,
            bound_target: Ok(None),
            glossary_paths,
            glossary: glossary::Glossary::default(),
            glossary_note: None,
            refine_port: None,
            refine_job: None,
            delivery_memory: None,
        }
    }

    fn is_capturing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|session| !session.stopped && !session.terminal)
    }

    fn is_processing(&self) -> bool {
        self.completion.is_some()
            || self
                .active
                .as_ref()
                .is_some_and(|session| session.stopped && !session.terminal)
    }

    fn next_id(&mut self) -> SessionId {
        self.next_session_id += 1;
        SessionId(self.next_session_id)
    }

    /// Use a specific 术语表 file for this application. The default is the first
    /// existing of `<runtime>/glossary.txt` and `glossary.txt` next to the
    /// executable, unless the process default was set from `--glossary`.
    pub fn with_glossary(mut self, path: impl Into<PathBuf>) -> Self {
        self.glossary_paths = vec![path.into()];
        self
    }

    /// The 术语表 file currently used, if one exists. The runtime copy wins over
    /// the copy next to the executable so a per-user edit still applies.
    pub fn glossary_file(&self) -> Option<&Path> {
        self.glossary_paths
            .iter()
            .map(PathBuf::as_path)
            .find(|path| path.is_file())
    }

    /// The `status.txt` spelling of the active 术语表: its path, or `none` when
    /// no file exists (correction is then a silent no-op).
    pub fn glossary_status(&self) -> String {
        self.glossary_file()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".into())
    }

    /// Read the glossary again so an edit applies to this capture without a
    /// restart. A malformed file is reported and never loses the capture: the
    /// transcript is still delivered, only without correction.
    fn reload_glossary(&mut self) -> AppResult<()> {
        let Some(path) = self.glossary_file().map(Path::to_path_buf) else {
            self.glossary = glossary::Glossary::default();
            self.glossary_note = None;
            return Ok(());
        };
        match glossary::Glossary::load(&path) {
            Ok(loaded) => {
                self.glossary = loaded;
                self.glossary_note = None;
            }
            Err(error) => {
                self.glossary = glossary::Glossary::default();
                self.glossary_note = Some(format!(
                    "Glossary correction is unavailable: {error}. The uncorrected transcript is still delivered."
                ));
                return Err(format!(
                    "Cloud trial glossary could not be loaded: {error}. No cloud request was sent"
                ));
            }
        }
        Ok(())
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

    /// Connect the optional 精修 port. Without one the refine entry stays
    /// unavailable and local recognition is untouched; with one, a request still
    /// only happens when the user explicitly asks for it.
    pub fn with_refine(mut self, refine: Arc<dyn RefineText>) -> Self {
        self.refine_port = Some(refine);
        self.snapshot.refine = match self.refine_availability() {
            Ok(()) => Refine::Unavailable("No complete result is available to refine yet.".into()),
            Err(reason) => Refine::Unavailable(reason),
        };
        self
    }

    /// Copy the complete current result through the caller's clipboard writer. Only
    /// the clipboard is written: never a target, never a keystroke. A failure is
    /// surfaced as [`Notice::CopyFailed`] instead of being dropped, because a user
    /// who asked for a copy must not be left assuming the clipboard is pasteable.
    pub fn copy_result(&mut self, copy: impl FnOnce(&str) -> AppResult<()>) {
        if self.snapshot.text.is_empty() {
            return;
        }
        match copy(&self.snapshot.text) {
            Ok(()) => {
                if matches!(self.snapshot.notice, Some(Notice::CopyFailed(_))) {
                    self.snapshot.notice = None;
                }
            }
            Err(error) => {
                self.snapshot.notice =
                    Some(Notice::CopyFailed(error.trim_end_matches('.').to_string()));
            }
        }
    }

    /// Report that the platform refused to hide the resident capsule from screen
    /// capture. The ADR promises the capsule is hidden from every screen share, so a
    /// failure must not stay visible only as `capsule_capture_excluded=0`.
    pub fn report_capture_exclusion(&mut self, reason: &str) {
        self.snapshot.notice = Some(Notice::CaptureExcluded(
            reason.trim_end_matches('.').to_string(),
        ));
    }

    /// Whether the connected port can run at all. Availability never performs
    /// network work: the DeepSeek implementation only checks for the local key.
    fn refine_availability(&self) -> Result<(), String> {
        match self.refine_port.as_ref() {
            Some(port) => port.availability(),
            None => Err(NO_REFINE_PORT.into()),
        }
    }

    /// Record a finished refine. `Refine::Running` and `Phase::Result` are the only
    /// states the worker can finish from; the current result is only replaced after a
    /// confirmed write or when there was no target at all.
    fn apply_refine_outcome(&mut self, outcome: RefineOutcome) {
        self.delivery_memory = None;
        match outcome {
            RefineOutcome::Applied { refined, note } => {
                self.snapshot.text = refined;
                self.snapshot.refined_text = None;
                self.snapshot.refine = Refine::Applied;
                self.snapshot.detail =
                    format!("Refine applied. {note} Nothing is sent automatically.");
            }
            RefineOutcome::NotApplied { refined, reason } => {
                self.snapshot.refined_text = Some(refined);
                self.snapshot.refine = Refine::NotApplied(reason.clone());
                let reason = reason.trim_end_matches('.').trim_end();
                self.snapshot.detail = format!(
                    "Refine was not written back: {reason}. The refined text is in the expanded panel and the complete result is unchanged. Nothing is sent automatically."
                );
            }
            RefineOutcome::Failed { reason } => {
                self.snapshot.refined_text = None;
                self.snapshot.refine = Refine::Failed(reason.clone());
                let reason = reason.trim_end_matches('.').trim_end();
                self.snapshot.detail = format!(
                    "Refine failed: {reason}. The complete result is unchanged and remains available; nothing was appended again or sent."
                );
            }
        }
    }

    /// What the application is doing right now, phrased for refusal messages that
    /// must not claim "nothing is being captured" while a capture is running.
    fn activity(&self) -> &'static str {
        if self.is_capturing() {
            "Playback is being captured and buffered"
        } else if self.is_processing() {
            "The previous capture is still being processed"
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
        // A refine may be running while the user asks for another capture. Refusing
        // the start is the simplest honest rule: the refine worker owns the delivery
        // memory and a capture would clear it mid-flight.
        if matches!(control, Control::Start) && self.refine_job.is_some() {
            self.snapshot.detail = "Refine is still running; the current result is unchanged. Wait for the refined text before starting a new capture.".into();
            return;
        }
        match control {
            Control::Start if !self.is_capturing() && !self.is_processing() => {
                let start_requested = Instant::now();
                if self.retry.is_some() {
                    self.snapshot.detail = "A failed capture's audio is retained for local retry. Retry local recognition or clean the failed audio before starting a new capture; the current result is unchanged.".into();
                    return;
                }
                // Bind once before any UI feedback. Recognition receives neither
                // the target draft nor an editor identity.
                self.bound_target = self
                    .targets
                    .as_ref()
                    .map_or(Ok(None), |targets| targets.focused_target());
                self.snapshot.endpoint = None;
                let mut started_directory = None;
                let result = (|| {
                    if let Some(error) = &self.control_error {
                        return Err(format!("Global hotkey unavailable: {error}"));
                    }
                    let glossary_ready = self.reload_glossary();
                    if self.sessions.requires_valid_glossary() {
                        glossary_ready?;
                    }
                    fs::create_dir_all(&self.runtime).map_err(|error| error.to_string())?;
                    let unique = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_nanos();
                    let directory = self
                        .runtime
                        .join(format!("session-{}-{unique}", std::process::id()));
                    started_directory = Some(directory.clone());
                    let id = self.next_id();
                    let handle = self.sessions.start(SessionRequest {
                        id,
                        directory: directory.clone(),
                        glossary: self.glossary.clone(),
                    })?;
                    let status = handle.status();
                    self.snapshot.endpoint = status.endpoint;
                    self.snapshot.recognition = Some(status.state);
                    self.snapshot.progress = status.progress;
                    self.snapshot.detail = self.with_glossary_note(status.detail);
                    self.active = Some(ActiveSession {
                        id,
                        directory,
                        handle,
                        stopped: false,
                        terminal: false,
                        stop_requested: None,
                        retry_audio: None,
                    });
                    Ok::<_, String>(())
                })();
                match result {
                    Ok(()) => {
                        self.snapshot.phase = Phase::Capturing;
                        self.snapshot.delivery = None;
                        self.snapshot.retained_audio.clear();
                        self.snapshot.retry_available = false;
                        self.delivery_memory = None;
                        self.snapshot.refined_text = None;
                        self.snapshot.refine = Refine::Unavailable(
                            "Playback is being captured; stop and wait for the complete result before refining.".into(),
                        );
                        if matches!(self.snapshot.notice, Some(Notice::CopyFailed(_))) {
                            self.snapshot.notice = None;
                        }
                        self.snapshot.timing = Timing {
                            preparation_ms: self.snapshot.timing.preparation_ms,
                            capture_start_ms: Some(elapsed_ms(start_requested)),
                            ..Timing::default()
                        };
                    }
                    Err(error) => {
                        self.bound_target = Ok(None);
                        self.active = None;
                        self.snapshot.recognition = Some(SessionState::Failed);
                        self.fail(started_directory.as_deref(), error);
                    }
                }
            }
            Control::Stop if self.is_capturing() => {
                let active = self.active.as_mut().unwrap();
                active.stop_requested = Some(Instant::now());
                active.stopped = true;
                active.handle.stop();
                self.snapshot.recognition = Some(SessionState::AwaitingFinal);
                self.snapshot.phase = Phase::Processing;
                self.snapshot.detail = active.handle.status().detail;
            }
            Control::Refine => {
                if self.refine_job.is_some() {
                    self.snapshot.detail = "Refine is already running; the current result is unchanged. Wait for the refined text before asking again.".into();
                    return;
                }
                if self.is_capturing() {
                    self.snapshot.refine = Refine::Unavailable(
                        "Playback is being captured; stop and wait for the complete result before refining."
                            .into(),
                    );
                    self.snapshot.detail = "Refine is unavailable while playback is being captured. Stop the capture and wait for the complete result; the current result is unchanged.".into();
                    return;
                }
                if self.is_processing() {
                    self.snapshot.refine = Refine::Unavailable(
                        "The previous capture is still processing; wait for the complete result before refining."
                            .into(),
                    );
                    self.snapshot.detail = "Refine is unavailable while the current playback is still being processed. The current result is unchanged.".into();
                    return;
                }
                // The entry is only enabled for a complete result that the port can
                // actually refine; every other state is refused explicitly rather
                // than silently doing nothing.
                if !matches!(self.snapshot.refine, Refine::Available) {
                    let reason = self
                        .snapshot
                        .refine
                        .reason()
                        .unwrap_or("no complete result is available")
                        .trim_end_matches('.')
                        .trim_end()
                        .to_string();
                    self.snapshot.detail = format!(
                        "Refine is unavailable: {reason}. The current result is unchanged and can still be copied."
                    );
                    return;
                }
                let Some(refine) = self.refine_port.clone() else {
                    self.snapshot.refine = Refine::Unavailable(NO_REFINE_PORT.into());
                    self.snapshot.detail = format!(
                        "Refine is unavailable: {NO_REFINE_PORT} The current result is unchanged."
                    );
                    return;
                };
                // Only the transcript text is handed to the port. The target memory
                // moves to the worker so the target read/write never blocks the UI.
                let text = self.snapshot.text.clone();
                let memory = self.delivery_memory.take();
                let current_result = self.runtime.join("current-result.txt");
                let (tx, rx) = mpsc::channel();
                self.refine_job = Some(rx);
                self.snapshot.refine = Refine::Running;
                self.snapshot.refined_text = None;
                self.snapshot.detail = "Refining the current result through the configured DeepSeek API; only the transcript text is sent, never the audio. The current result stays available meanwhile.".into();
                thread::spawn(move || {
                    let _ = tx.send(refine_delivery(refine, text, memory, current_result));
                });
            }
            Control::Retry => {
                if self.snapshot.preparation == Preparation::Pending {
                    let activity = self.activity();
                    self.snapshot.detail = format!(
                        "The local recognition model is still being prepared; wait until Listen reports Ready before retrying. {activity} and the current result is unchanged."
                    );
                    return;
                }
                if self.is_capturing() {
                    self.snapshot.detail = "Still capturing the current playback locally; stop it and wait for the result before retrying.".into();
                    return;
                }
                if self.is_processing() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before retrying.".into();
                    return;
                }
                self.retry_local_recognition();
            }
            Control::CleanupAudio => {
                if self.snapshot.preparation == Preparation::Pending {
                    let activity = self.activity();
                    self.snapshot.detail = format!(
                        "The local recognition model is still being prepared; wait until Listen reports Ready before cleaning failed audio. {activity} and the current result is unchanged."
                    );
                    return;
                }
                if self.is_capturing() {
                    self.snapshot.detail = "Still capturing the current playback locally; stop it and wait for the result before cleaning failed audio.".into();
                    return;
                }
                if self.is_processing() {
                    self.snapshot.detail = "Still processing the current playback locally; wait for the result before cleaning failed audio.".into();
                    return;
                }
                self.cleanup_failed_audio();
            }
            _ if self.snapshot.preparation == Preparation::Pending => {
                let activity = self.activity();
                self.snapshot.detail = format!(
                    "The local recognition model is still being prepared; {activity}. The current result is unchanged."
                );
            }
            _ if self.is_processing() => {
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
        let id = self.next_id();
        let request = SessionRequest {
            id,
            directory: retry.directory.clone(),
            glossary: self.glossary.clone(),
        };
        match self.sessions.retry(request, retry.audio.clone()) {
            Ok(handle) => {
                self.bound_target = retry.target;
                self.active = Some(ActiveSession {
                    id,
                    directory: retry.directory,
                    handle,
                    stopped: true,
                    terminal: false,
                    stop_requested: None,
                    retry_audio: Some(retry.audio),
                });
                self.snapshot.phase = Phase::Processing;
                self.snapshot.recognition = Some(SessionState::AwaitingFinal);
                self.snapshot.delivery = None;
                self.snapshot.retry_available = false;
                self.snapshot.detail = "Retrying recognition locally on the retained audio; no replay is needed and no messages are sent.".into();
            }
            Err(error) => {
                let directory = retry.directory.clone();
                self.retry = Some(retry);
                self.snapshot.retry_available = true;
                self.fail(Some(&directory), error);
            }
        }
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
        if let Some(job) = &self.refine_job {
            match job.try_recv() {
                Ok(outcome) => {
                    self.refine_job = None;
                    self.apply_refine_outcome(outcome);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.refine_job = None;
                    self.delivery_memory = None;
                    self.snapshot.refined_text = None;
                    self.snapshot.refine = Refine::Failed(
                        "the refine worker stopped unexpectedly before reporting a correction"
                            .into(),
                    );
                    self.snapshot.detail = "Refine failed: the worker stopped unexpectedly before reporting a correction. The complete result is unchanged and remains available; nothing was appended again or sent.".into();
                }
            }
        }
        let readiness = self.sessions.preparation();
        let preparation_changed = self.snapshot.preparation != readiness.state;
        if self.control_error.is_none() {
            self.snapshot.preparation = readiness.state;
        }
        self.snapshot.backend = readiness.backend;
        self.snapshot.timing.preparation_ms = readiness.elapsed_ms;
        if self.snapshot.phase == Phase::Preparing {
            match readiness.state {
                Preparation::Ready => {
                    self.snapshot.phase = Phase::Ready;
                    self.snapshot.detail = ready_detail(self.snapshot.backend.as_deref());
                }
                Preparation::Failed => self.fail(
                    None,
                    readiness
                        .error
                        .unwrap_or_else(|| "Recognition preparation failed".into()),
                ),
                Preparation::Pending => {}
            }
        }
        if let Some(active) = self.active.as_mut().filter(|session| !session.terminal) {
            let event = active.handle.poll();
            let status = active.handle.status();
            // A retry processes the already captured audio; keep that capture's
            // route/progress rather than reporting a new unidentified device.
            if active.retry_audio.is_none() {
                self.snapshot.progress = status.progress;
                self.snapshot.endpoint = status.endpoint;
            }
            if status.state == SessionState::AwaitingFinal && !active.stopped {
                active.stopped = true;
                active.stop_requested = Some(Instant::now());
                self.snapshot.phase = Phase::Processing;
            }
            if !active.stopped || preparation_changed {
                self.snapshot.detail = match &self.glossary_note {
                    Some(note) => format!("{}\n{note}", status.detail),
                    None => status.detail,
                };
            }
            // A stable sentence remains observable until the next session event;
            // a status refresh must not turn it into a completed result.
            if self.snapshot.recognition != Some(SessionState::Stable) || active.stopped {
                self.snapshot.recognition = Some(status.state);
            }
            if let Some(event) = event {
                self.apply_session_event(event);
            }
        }
        let Some(rx) = &self.completion else {
            return;
        };
        let completion = match rx.try_recv() {
            Ok(completion) => completion,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.completion = None;
                self.snapshot.recognition = Some(SessionState::Failed);
                self.fail(None, "Local processing worker stopped unexpectedly".into());
                return;
            }
        };
        self.completion = None;
        self.apply_completion(completion);
    }

    fn apply_session_event(&mut self, event: SessionEvent) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.id != event.id || active.terminal {
            return;
        }
        match event.update {
            SessionUpdate::Stable(_) if !active.stopped => {
                self.snapshot.recognition = Some(SessionState::Stable);
            }
            SessionUpdate::Finished(result) if active.stopped => {
                active.terminal = true;
                self.snapshot.recognition = Some(SessionState::Complete);
                self.finish_recognition(result);
            }
            SessionUpdate::Failed(failure) => {
                active.terminal = true;
                self.snapshot.recognition = Some(SessionState::Failed);
                let directory = active.directory.clone();
                let target = std::mem::replace(&mut self.bound_target, Ok(None));
                let retry = failure.retry_audio.map(|audio| RetainedCapture {
                    audio,
                    directory: directory.clone(),
                    target,
                });
                let mut timing = failure.timing;
                timing.preparation_ms = self.snapshot.timing.preparation_ms;
                timing.capture_start_ms = self.snapshot.timing.capture_start_ms;
                self.apply_completion(Completion::Failed {
                    error: failure.error,
                    directory,
                    retry,
                    timing,
                    retry_scope: failure.retry_scope,
                });
            }
            _ => {}
        }
    }

    fn finish_recognition(&mut self, result: SessionResult) {
        let active = self.active.as_mut().unwrap();
        let directory = active.directory.clone();
        let retry_audio = active.retry_audio.take();
        let stopped_at = match (active.stop_requested, result.stopped_at) {
            (Some(control), Some(session)) => Some(control.min(session)),
            (control, session) => control.or(session),
        };
        let target = std::mem::replace(&mut self.bound_target, Ok(None));
        let current_result = self.runtime.join("current-result.txt");
        let mut timing = result.timing;
        timing.capture_start_ms = self.snapshot.timing.capture_start_ms;
        timing.preparation_ms = self.snapshot.timing.preparation_ms;
        let (tx, rx) = mpsc::channel();
        self.completion = Some(rx);
        thread::spawn(move || {
            let text = result.text;
            let completion = if text.trim().is_empty() {
                timing.stop_to_result_ms = stopped_at.map(elapsed_ms);
                Completion::Finished(Completed {
                    text,
                    description: result.description,
                    delivery: None,
                    detail: String::new(),
                    save_error: None,
                    directory,
                    timing,
                    memory: None,
                })
            } else {
                let save_started = Instant::now();
                let save_error = cloud::persist_result(&current_result, text.as_bytes())
                    .err()
                    .map(|error| error.to_string());
                timing.save_ms = Some(elapsed_ms(save_started));
                match (save_error, retry_audio) {
                    (Some(error), Some(audio)) => Completion::Failed {
                        error: format!("Could not save the current result: {error}"),
                        retry: Some(RetainedCapture {
                            audio,
                            directory: directory.clone(),
                            target,
                        }),
                        directory,
                        timing,
                        retry_scope: None,
                    },
                    (save_error, _) => {
                        let delivery_started = Instant::now();
                        let outcome = match &save_error {
                            Some(error) => DeliveryOutcome::fallback(not_delivered(format!(
                                "Could not save the current result: {error}"
                            ))),
                            None => deliver(target, &text),
                        };
                        timing.delivery_ms = Some(elapsed_ms(delivery_started));
                        timing.stop_to_result_ms = stopped_at.map(elapsed_ms);
                        Completion::Finished(Completed {
                            text,
                            description: result.description,
                            delivery: Some(outcome.delivery),
                            detail: outcome.detail,
                            save_error,
                            directory,
                            timing,
                            memory: outcome.memory,
                        })
                    }
                }
            };
            let _ = tx.send(completion);
        });
    }

    fn apply_completion(&mut self, completion: Completion) {
        match completion {
            Completion::Finished(completed) if completed.text.trim().is_empty() => {
                // A completed no-speech session is not a new transcript. In particular,
                // leave the previous copyable result and its on-disk copy untouched.
                self.snapshot.timing = completed.timing.clone();
                self.snapshot.delivery = None;
                self.record_latency("NoSpeech", &completed.timing, &completed.directory);
                self.snapshot.retained_audio.clear();
                self.snapshot.retry_available = false;
                // The previous result is kept, but it is not what this capture
                // produced: refine stays off until a complete result exists again.
                self.delivery_memory = None;
                self.snapshot.refined_text = None;
                self.snapshot.refine = Refine::Unavailable(
                    "The latest capture contained no speech, so there is no complete result to refine."
                        .into(),
                );
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
                // Only a confirmed append leaves a target that a later refine may
                // safely replace; a fallback result still offers refine but has no
                // target to write back to.
                self.delivery_memory = completed.memory;
                self.snapshot.refined_text = None;
                self.snapshot.refine = match self.refine_availability() {
                    Ok(()) => Refine::Available,
                    Err(reason) => Refine::Unavailable(reason),
                };
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
                    self.snapshot.detail.push_str("\nThe audio from this capture is retained. Local retry recognizes the same audio again without replaying the speech; the target bound at capture start is kept, or the expanded result panel is used if it is no longer valid.");
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
            "capture_start_ms": timing.capture_start_ms,
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
                        } else if path
                            .extension()
                            .is_some_and(|extension| extension == "wav" || extension == "pcm")
                            && !self.snapshot.retained_audio.contains(&path)
                        {
                            self.snapshot.retained_audio.push(path);
                        }
                    }
                }
            }
        }
        self.snapshot.phase = Phase::Failure;
        self.snapshot.recognition = Some(SessionState::Failed);
        // A failed operation leaves no complete result to refine, and no delivery
        // memory that a later refine could act on.
        self.delivery_memory = None;
        self.snapshot.refined_text = None;
        self.snapshot.refine = Refine::Unavailable(
            "The latest operation failed, so there is no complete result to refine.".into(),
        );
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
                    self.snapshot.detail.push_str(&format!(". The capture was interrupted, so this text is incomplete: it is shown in the expanded result panel and stored at {}. This is not a completed transcription.", path.display()));
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}

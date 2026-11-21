use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
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
pub(crate) mod segments;
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
    LocalSessions, RecognitionSession, RecognitionSessions, SegmentUsage, SessionEvent,
    SessionFailure, SessionId, SessionRequest, SessionResult, SessionState, SessionUpdate,
};
use segments::SegmentStore;
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

/// One start/stop segment's user-visible state (#33). It is deliberately a
/// different vocabulary from [`Phase`]: the phase describes the one operation the
/// capsule shows, while every segment keeps its own place in the panel until the
/// user takes its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentState {
    /// Playback is being captured for this segment.
    Capturing,
    /// A transient cloud failure interrupted this segment; it is recovering.
    WaitingForNetwork,
    /// The segment was stopped and its whole result is still being confirmed.
    Processing,
    /// A complete result exists for this segment.
    Result,
    /// The segment completed without any speech.
    NoSpeech,
    /// The segment ended without a complete result.
    Failed,
}

impl SegmentState {
    /// The stable spelling used by `segments.json` and the status file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Capturing => "capturing",
            Self::WaitingForNetwork => "waiting_for_network",
            Self::Processing => "processing",
            Self::Result => "result",
            Self::NoSpeech => "no_speech",
            Self::Failed => "failed",
        }
    }

    /// Whether this segment still occupies capture or recognition work.
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            Self::Capturing | Self::WaitingForNetwork | Self::Processing
        )
    }
}

/// One capture's independent record as the panel sees it: capture order, state,
/// complete text, delivery and what the engine metered. A later segment never
/// replaces an earlier entry; [`Snapshot::text`] is only the newest complete
/// result, so an older pending result stays readable in its own entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentSummary {
    /// 1-based capture order. Stable for the life of the segment.
    pub sequence: u64,
    pub state: SegmentState,
    /// The segment's own status line; never contains a credential.
    pub detail: String,
    /// The complete text, empty until the segment has one.
    pub text: String,
    pub delivery: Option<Delivery>,
    /// What the recognition engine recorded for this segment's paid requests.
    /// `None` means the engine cannot meter it; it is never reported as zero.
    pub usage: Option<SegmentUsage>,
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
    /// Every segment of this run in capture order. An older waiting segment keeps
    /// its own entry and its own complete text; `text` above is only the newest
    /// complete result.
    pub segments: Vec<SegmentSummary>,
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

/// Audio from a failed recognition attempt, kept on disk so the user can retry
/// without replaying the speech. The target and glossary stay on the segment:
/// retrying never selects the focus that happens to be active later.
struct RetainedCapture {
    audio: CapturedAudio,
}

/// One segment's saved result whose target delivery finished off the UI thread.
/// It carries its own capture order, so an older segment's completion can never be
/// mistaken for the newest result.
struct Delivered {
    sequence: u64,
    text: String,
    description: String,
    outcome: DeliveryOutcome,
    timing: Timing,
    directory: PathBuf,
    stopped_at: Option<Instant>,
    usage: Option<SegmentUsage>,
}

/// One start/stop segment. It owns its capture directory, its bound target and its
/// own complete result; another segment never reads or writes them.
struct SegmentSession {
    id: SessionId,
    /// 1-based capture order; the newest segment is the one the capsule and phase
    /// describe.
    sequence: u64,
    directory: PathBuf,
    /// `None` once the session reported a terminal event; the handle is dropped so
    /// no further event from it can reach the application.
    handle: Option<Box<dyn RecognitionSession>>,
    stopped: bool,
    terminal: bool,
    /// A saved result's target delivery is still running.
    finalizing: bool,
    stop_requested: Option<Instant>,
    retry_audio: Option<CapturedAudio>,
    glossary: glossary::Glossary,
    /// The target bound at this segment's start; used exactly once.
    target: AppResult<Option<Box<dyn Target>>>,
    retained: Option<RetainedCapture>,
    /// Audio paths kept on disk from a failed attempt, shown to the user.
    retained_paths: Vec<PathBuf>,
    state: SegmentState,
    session_state: SessionState,
    detail: String,
    endpoint: Option<CaptureEndpoint>,
    progress: CaptureProgress,
    capture_start_ms: Option<u64>,
    text: Option<String>,
    delivery: Option<Delivery>,
    usage: Option<SegmentUsage>,
    timing: Timing,
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

/// Only one delivery may write to a target at a time. Segments finish at their own
/// pace, but one editor's read/modify/write pair must never interleave with another
/// segment's, or a confirmed append would be misread as a user edit. The lock is
/// taken on a delivery worker's thread, never on the application thread, so a slow
/// editor can never block a new capture.
static TARGET_WRITER: Mutex<()> = Mutex::new(());

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
    let _writer = TARGET_WRITER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
    let _writer = TARGET_WRITER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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

/// A result that only exists because the session waited for the network and later
/// recovered is a late result. Until the late-backfill safety checks can prove the
/// original draft is unchanged (#34), it is never written to the target
/// automatically; the complete text is saved and shown in the panel instead.
fn recovered_not_delivered() -> String {
    "The complete result was recovered after waiting for the network, so it was not written to the original target automatically. The complete text is saved on this machine and shown in the expanded result panel; review and copy it to the intended input. Nothing is sent automatically.".into()
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

pub struct Application {
    snapshot: Snapshot,
    sessions: Box<dyn RecognitionSessions>,
    runtime: PathBuf,
    /// Every segment of this run in capture order. The last one is the newest and
    /// drives `Phase`, the capsule and `Snapshot::text`; older segments keep their
    /// own state and complete text until the user takes them.
    segments: Vec<SegmentSession>,
    /// The durable per-segment record (#33). Only the application thread writes it,
    /// so no two workers can corrupt the index.
    store: SegmentStore,
    record_problem: Option<String>,
    next_session_id: u64,
    /// The highest capture order whose complete text is the current result. An older
    /// segment that completes later must never overwrite it.
    current_sequence: u64,
    completion_tx: mpsc::Sender<Delivered>,
    completion_rx: mpsc::Receiver<Delivered>,
    /// Deliveries that already finished but are not applied yet, keyed by capture
    /// order so old segments are applied in capture order.
    deliveries: BTreeMap<u64, Delivered>,
    control_error: Option<String>,
    targets: Option<Arc<dyn TargetSelection>>,
    glossary_paths: Vec<PathBuf>,
    glossary: glossary::Glossary,
    glossary_note: Option<String>,
    refine_port: Option<Arc<dyn RefineText>>,
    refine_job: Option<mpsc::Receiver<RefineOutcome>>,
    delivery_memory: Option<DeliveryMemory>,
    segments_dirty: bool,
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
        let (phase, mut detail) = match initial_error {
            Some(error) => (Phase::Failure, error),
            None if preparation == Preparation::Pending => (
                Phase::Preparing,
                "Preparing the local recognition model in the background; capture can start now and its audio is buffered until the model reports Ready.".into(),
            ),
            None => (Phase::Ready, ready_detail(readiness.backend.as_deref())),
        };
        let glossary_paths =
            glossary::default_paths(&runtime, std::env::current_exe().ok().as_deref());
        let store = SegmentStore::open(&runtime);
        if let Some(problem) = store.problem() {
            detail.push_str(&format!("\n{problem}"));
        }
        let (completion_tx, completion_rx) = mpsc::channel();
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
                segments: Vec::new(),
            },
            sessions,
            runtime,
            segments: Vec::new(),
            store,
            record_problem: None,
            next_session_id: 0,
            current_sequence: 0,
            completion_tx,
            completion_rx,
            deliveries: BTreeMap::new(),
            control_error,
            targets: None,
            glossary_paths,
            glossary: glossary::Glossary::default(),
            glossary_note: None,
            refine_port: None,
            refine_job: None,
            delivery_memory: None,
            segments_dirty: false,
        }
    }

    /// The segment whose capture is still running. The playback device admits one
    /// at a time; an older segment that is only waiting or processing is not it.
    fn capturing_index(&self) -> Option<usize> {
        self.segments
            .iter()
            .position(|segment| segment.handle.is_some() && !segment.stopped && !segment.terminal)
    }

    fn is_capturing(&self) -> bool {
        self.capturing_index().is_some()
    }

    /// Any segment still waiting for its complete result or for a target delivery.
    fn is_processing(&self) -> bool {
        self.segments.iter().any(|segment| {
            segment.finalizing
                || (segment.handle.is_some() && segment.stopped && !segment.terminal)
        })
    }

    fn any_retained(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.retained.is_some())
    }

    /// The newest failed segment that still owns recoverable audio. Retry and
    /// CleanupAudio act on it; an older failure keeps its own audio and record.
    fn newest_retained(&self) -> Option<usize> {
        self.segments
            .iter()
            .rposition(|segment| segment.retained.is_some())
    }

    fn current_segment_index(&self) -> Option<usize> {
        self.segments
            .iter()
            .position(|segment| segment.sequence == self.current_sequence)
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

    /// Record a finished refine. The current result is only replaced after a
    /// confirmed write or when there was no target at all; the segment that owns the
    /// current result keeps the refined text, so the panel and the record agree.
    fn apply_refine_outcome(&mut self, outcome: RefineOutcome) {
        self.delivery_memory = None;
        match outcome {
            RefineOutcome::Applied { refined, note } => {
                self.snapshot.text = refined.clone();
                self.snapshot.refined_text = None;
                self.snapshot.refine = Refine::Applied;
                self.snapshot.detail =
                    format!("Refine applied. {note} Nothing is sent automatically.");
                self.keep_refined_text(refined);
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
        self.segments_dirty = true;
        self.refresh_segments();
    }

    /// The refined text becomes the current segment's own complete text, and its
    /// durable copy follows when it exists. The target was already verified above.
    fn keep_refined_text(&mut self, refined: String) {
        let Some(index) = self.current_segment_index() else {
            return;
        };
        self.segments[index].text = Some(refined.clone());
        let sequence = self.segments[index].sequence;
        if let Err(error) = self.store.write_text(sequence, &refined) {
            self.note_record_problem(error);
        } else if let Some(record) = self.store.record_mut(sequence) {
            record.text_chars = refined.chars().count();
            record.updated_utc = segments::now_utc();
            if let Err(error) = self.store.persist() {
                self.note_record_problem(error);
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
            // A new capture is refused only while another capture runs. An older
            // segment that waits for the network or is still being processed must
            // never block the next sentence.
            Control::Start if !self.is_capturing() => self.start_capture(),
            Control::Stop if self.is_capturing() => self.stop_capture(),
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
                    self.snapshot.detail = "Still capturing the current playback; stop it and wait for the result before retrying.".into();
                    return;
                }
                if self.is_processing() {
                    self.snapshot.detail = "Still processing the current playback; wait for the result before retrying.".into();
                    return;
                }
                self.retry_retained_recognition();
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
            // A Start while a capture is live is about the live capture, whatever
            // older segments are doing; it must say so instead of reporting them.
            Control::Start => {
                self.snapshot.detail =
                    "Already capturing playback audio. Stop to transcribe the current recording."
                        .into();
            }
            _ if self.snapshot.preparation == Preparation::Pending => {
                let activity = self.activity();
                self.snapshot.detail = format!(
                    "The local recognition model is still being prepared; {activity}. The current result is unchanged."
                );
            }
            _ if self.is_processing() => {
                self.snapshot.detail = "Still processing the current playback; wait for the result before starting again.".into();
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
        self.segments_dirty = true;
        self.refresh_segments();
    }

    /// Bind the target once and create one independent segment. Older segments keep
    /// their own target, audio and result; only the playback device is exclusive.
    fn start_capture(&mut self) {
        let start_requested = Instant::now();
        if self.any_retained() {
            self.snapshot.detail = "A failed capture's audio is retained for retry. Retry recognition or clean the failed audio before starting a new capture; the current result is unchanged.".into();
            return;
        }
        // Bind once before any UI feedback. Recognition receives neither the target
        // draft nor an editor identity.
        let bound_target = self
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
            let glossary = self.glossary.clone();
            let handle = self.sessions.start(SessionRequest {
                id,
                directory: directory.clone(),
                glossary: glossary.clone(),
            })?;
            let status = handle.status();
            Ok::<_, String>((id, directory, handle, status, glossary))
        })();
        match result {
            Ok((id, directory, handle, status, glossary)) => {
                let (sequence, record_error) = self.store.begin(&directory, &status.detail);
                let capture_start_ms = Some(elapsed_ms(start_requested));
                let segment = SegmentSession {
                    id,
                    sequence,
                    directory,
                    handle: Some(handle),
                    stopped: false,
                    terminal: false,
                    finalizing: false,
                    stop_requested: None,
                    retry_audio: None,
                    glossary,
                    target: bound_target,
                    retained: None,
                    retained_paths: Vec::new(),
                    state: SegmentState::Capturing,
                    session_state: status.state,
                    detail: status.detail.clone(),
                    endpoint: status.endpoint.clone(),
                    progress: status.progress.clone(),
                    capture_start_ms,
                    text: None,
                    delivery: None,
                    usage: None,
                    timing: Timing::default(),
                };
                self.snapshot.endpoint = segment.endpoint.clone();
                self.snapshot.recognition = Some(status.state);
                self.snapshot.progress = segment.progress.clone();
                self.snapshot.detail = self.with_glossary_note(status.detail);
                self.segments.push(segment);
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
                    capture_start_ms,
                    ..Timing::default()
                };
                if let Some(error) = record_error {
                    self.note_record_problem(error);
                }
            }
            Err(error) => {
                self.snapshot.recognition = Some(SessionState::Failed);
                self.snapshot.phase = Phase::Failure;
                self.snapshot.detail = error;
            }
        }
    }

    /// Stop only the live capture. Every older segment keeps waiting or finalizing
    /// on its own.
    fn stop_capture(&mut self) {
        let Some(index) = self.capturing_index() else {
            return;
        };
        let stop_requested = Instant::now();
        let detail = {
            let segment = &mut self.segments[index];
            segment.stop_requested = Some(stop_requested);
            segment.stopped = true;
            segment.state = SegmentState::Processing;
            if let Some(handle) = segment.handle.as_mut() {
                handle.stop();
            }
            segment
                .handle
                .as_ref()
                .map(|handle| handle.status().detail)
                .unwrap_or_else(|| segment.detail.clone())
        };
        if index + 1 == self.segments.len() {
            self.snapshot.recognition = Some(SessionState::AwaitingFinal);
            self.snapshot.phase = Phase::Processing;
            self.snapshot.detail = detail;
        }
        self.persist_segment(index);
    }

    /// Reprocess the exact audio kept from the newest failed segment. This never
    /// starts capture, never re-selects a target and never asks for the speech to be
    /// replayed: the configured recognizer processes the same recording again.
    fn retry_retained_recognition(&mut self) {
        let Some(index) = self.newest_retained() else {
            self.snapshot.retry_available = false;
            self.snapshot.detail = "No retained audio is available for a retry, so nothing was recognized and no result was delivered. The current result is unchanged.".into();
            return;
        };
        let Some(retained) = self.segments[index].retained.as_ref() else {
            return;
        };
        let audio = retained.audio.clone();
        if !audio.path.is_file() {
            self.segments[index].retained = None;
            self.segments[index].retained_paths.clear();
            let _ = fs::remove_dir_all(&self.segments[index].directory);
            self.persist_segment(index);
            self.refresh_recovery_snapshot();
            self.snapshot.detail = "The audio retained from the failed capture is no longer on disk, so it was not retried. The current result is unchanged.".into();
            return;
        }
        let id = self.next_id();
        let glossary = self.segments[index].glossary.clone();
        let request = SessionRequest {
            id,
            directory: self.segments[index].directory.clone(),
            glossary,
        };
        match self.sessions.retry(request, audio.clone()) {
            Ok(handle) => {
                let segment = &mut self.segments[index];
                segment.id = id;
                segment.handle = Some(handle);
                segment.stopped = true;
                segment.terminal = false;
                segment.finalizing = false;
                segment.retry_audio = Some(audio);
                segment.retained = None;
                segment.retained_paths.clear();
                segment.state = SegmentState::Processing;
                segment.session_state = SessionState::AwaitingFinal;
                segment.detail =
                    "Retrying recognition on the retained audio; the speech is not replayed."
                        .into();
                self.persist_segment(index);
                self.refresh_recovery_snapshot();
                if index + 1 == self.segments.len() {
                    self.snapshot.phase = Phase::Processing;
                    self.snapshot.recognition = Some(SessionState::AwaitingFinal);
                    self.snapshot.delivery = None;
                    self.snapshot.retry_available = false;
                    self.snapshot.detail =
                        "Retrying recognition on the retained audio; the speech is not replayed."
                            .into();
                }
            }
            Err(error) => {
                self.fail_segment(index, error, Timing::default());
            }
        }
    }

    /// Delete the newest failed segment's retained audio at the user's explicit
    /// request. Every other segment's recordings and results stay untouched.
    fn cleanup_failed_audio(&mut self) {
        let Some(index) = self.newest_retained() else {
            self.snapshot.retry_available = false;
            self.snapshot.detail =
                "There is no failed capture audio to clean. The current result is unchanged."
                    .into();
            return;
        };
        match fs::remove_dir_all(&self.segments[index].directory) {
            Ok(()) => {
                self.segments[index].retained = None;
                self.segments[index].retained_paths.clear();
                self.segments[index].detail = "The temporarily retained audio from the failed capture was deleted at your request. It will not be retried, and the current result is unchanged.".into();
                self.remove_segment_record(index);
                self.refresh_recovery_snapshot();
                if index + 1 == self.segments.len() {
                    self.snapshot.retained_audio.clear();
                    self.snapshot.retry_available = false;
                    self.snapshot.detail = "The temporarily retained audio from the failed capture was deleted at your request. It will not be retried, and the current result is unchanged.".into();
                }
            }
            Err(error) => {
                self.segments[index].detail = format!(
                    "The temporarily retained audio could not be deleted ({error}), so it is still available for a retry."
                );
                self.refresh_recovery_snapshot();
                if index + 1 == self.segments.len() {
                    self.snapshot.retry_available = true;
                    self.snapshot.detail = self.segments[index].detail.clone();
                }
            }
        }
        self.persist_segment(index);
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
        // Refresh every live segment in capture order, but apply at most one session
        // event per poll: the oldest waiting segment cannot be reordered behind a
        // newer one inside one poll, and the poll stays bounded. Only the segments up
        // to the first event are polled, so no session's event is consumed and
        // dropped when two of them finish between polls.
        for index in 0..self.segments.len() {
            if self.segments[index].handle.is_none() || self.segments[index].terminal {
                continue;
            }
            let status = self.segments[index]
                .handle
                .as_mut()
                .expect("checked above")
                .status();
            self.note_segment_status(index, status, preparation_changed);
        }
        let mut event: Option<(usize, SessionEvent)> = None;
        for index in 0..self.segments.len() {
            if self.segments[index].handle.is_none() || self.segments[index].terminal {
                continue;
            }
            let session_event = self.segments[index]
                .handle
                .as_mut()
                .expect("checked above")
                .poll();
            if let Some(session_event) = session_event {
                event = Some((index, session_event));
                break;
            }
        }
        if let Some((index, event)) = event {
            self.apply_session_event(index, event);
        }
        // Apply finished deliveries oldest capture order first, at most one per
        // poll, so a burst of completions cannot reorder the panel.
        loop {
            match self.completion_rx.try_recv() {
                Ok(delivered) => {
                    self.deliveries
                        .entry(delivered.sequence)
                        .or_insert(delivered);
                }
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if let Some((_, delivered)) = self.deliveries.pop_first() {
            self.apply_delivery(delivered);
        }
        self.refresh_segments();
    }

    /// Keep one segment's own status up to date, and copy it into the phase/capsule
    /// snapshot only while that segment is the newest one.
    fn note_segment_status(
        &mut self,
        index: usize,
        status: recognition::SessionStatus,
        preparation_changed: bool,
    ) {
        let primary = index + 1 == self.segments.len();
        let previous_state = self.segments[index].state;
        {
            let segment = &mut self.segments[index];
            segment.session_state = status.state;
            segment.detail = status.detail.clone();
            // A retry processes the already captured audio; keep that capture's
            // route/progress rather than reporting a new unidentified device.
            if segment.retry_audio.is_none() {
                segment.progress = status.progress.clone();
                segment.endpoint = status.endpoint.clone();
            }
            if !matches!(
                segment.state,
                SegmentState::Result | SegmentState::NoSpeech | SegmentState::Failed
            ) {
                segment.state = match status.state {
                    SessionState::WaitingForNetwork => SegmentState::WaitingForNetwork,
                    SessionState::Capturing | SessionState::Stable => SegmentState::Capturing,
                    // A session reports `Complete`/`Failed` only once its terminal
                    // event is queued; the segment becomes a result (or a failure)
                    // when that event is applied, not before.
                    SessionState::Preparing
                    | SessionState::AwaitingFinal
                    | SessionState::Complete
                    | SessionState::Failed => SegmentState::Processing,
                };
            }
            if status.state == SessionState::AwaitingFinal && !segment.stopped {
                segment.stopped = true;
                segment.stop_requested = Some(Instant::now());
                segment.state = SegmentState::Processing;
                if primary {
                    self.snapshot.phase = Phase::Processing;
                }
            }
        }
        let changed = self.segments[index].state != previous_state;
        if changed {
            self.segments_dirty = true;
            self.persist_segment(index);
        }
        if !primary {
            return;
        }
        if self.segments[index].retry_audio.is_none() {
            self.snapshot.progress = self.segments[index].progress.clone();
            self.snapshot.endpoint = self.segments[index].endpoint.clone();
        }
        // A transition into waiting-for-network must be visible even after stop,
        // when the detail is otherwise held stable. A normal stop keeps the last
        // capture detail, which carries the backlog/interruption note.
        let state_changed = self.snapshot.recognition != Some(status.state);
        let waiting_now = status.state == SessionState::WaitingForNetwork;
        if !self.segments[index].stopped || preparation_changed || (waiting_now && state_changed) {
            self.snapshot.detail = match &self.glossary_note {
                Some(note) => format!("{}\n{note}", status.detail),
                None => status.detail,
            };
        }
        // A stable sentence stays observable until the next session event; a status
        // refresh must not turn it into a completed result. A waiting or failed
        // state still supersedes it.
        let keep_stable = self.snapshot.recognition == Some(SessionState::Stable)
            && !self.segments[index].stopped
            && status.state == SessionState::Capturing;
        if !keep_stable {
            self.snapshot.recognition = Some(status.state);
        }
    }

    fn apply_session_event(&mut self, index: usize, event: SessionEvent) {
        let Some(segment) = self.segments.get(index) else {
            return;
        };
        if segment.terminal || segment.id != event.id {
            return;
        }
        let primary = index + 1 == self.segments.len();
        match event.update {
            SessionUpdate::Stable(_) if !segment.stopped => {
                if primary {
                    self.snapshot.recognition = Some(SessionState::Stable);
                }
            }
            SessionUpdate::Finished(result) if segment.stopped => {
                self.segments[index].terminal = true;
                if primary {
                    self.snapshot.recognition = Some(SessionState::Complete);
                }
                self.finish_recognition(index, result);
            }
            SessionUpdate::Failed(failure) => {
                let SessionFailure {
                    error,
                    retry_audio,
                    retry_scope,
                    mut timing,
                } = failure;
                timing.preparation_ms = self.snapshot.timing.preparation_ms;
                timing.capture_start_ms = self.segments[index].capture_start_ms;
                self.segments[index].terminal = true;
                if let Some(audio) = retry_audio {
                    self.segments[index].retained = Some(RetainedCapture { audio });
                }
                self.segments[index].timing = timing.clone();
                if primary {
                    self.snapshot.recognition = Some(SessionState::Failed);
                }
                self.fail_segment(index, error, timing);
                // The retry/recovery advice is appended after the failure detail so
                // the reason itself is never hidden by it.
                let note = if self.any_retained() {
                    Some("The audio from this capture is retained. Retry processes the same audio again without replaying the speech; the target bound at capture start is kept, or the expanded result panel is used if it is no longer valid.".to_string())
                } else {
                    retry_scope
                };
                if let Some(note) = note {
                    self.segments[index].detail.push('\n');
                    self.segments[index].detail.push_str(&note);
                    if primary {
                        self.snapshot.detail.push('\n');
                        self.snapshot.detail.push_str(&note);
                    }
                    self.persist_segment(index);
                }
            }
            _ => {}
        }
    }

    /// Save one segment's complete result, reliably and before any target write.
    /// Only the newest complete result is mirrored to `current-result.txt`, so an
    /// older segment that finishes late cannot overwrite it.
    fn save_complete_text(&mut self, sequence: u64, text: &str, current: bool) -> AppResult<()> {
        let name = self.store.write_text(sequence, text)?;
        if let Some(record) = self.store.record_mut(sequence) {
            record.text_file = Some(name);
            record.text_chars = text.chars().count();
            record.updated_utc = segments::now_utc();
            if self.store.problem().is_none() {
                if let Err(error) = self.store.persist() {
                    self.note_record_problem(error);
                }
            }
        }
        if current {
            cloud::persist_result(&self.runtime.join("current-result.txt"), text.as_bytes())
                .map_err(|error| format!("Could not save the current result: {error}"))?;
        }
        Ok(())
    }

    fn finish_recognition(&mut self, index: usize, result: SessionResult) {
        let sequence = self.segments[index].sequence;
        let directory = self.segments[index].directory.clone();
        let description = result.description.clone();
        let recovered = result.recovered;
        let text = result.text.clone();
        let retry_audio = self.segments[index].retry_audio.take();
        let target = std::mem::replace(&mut self.segments[index].target, Ok(None));
        let stopped_at = match (self.segments[index].stop_requested, result.stopped_at) {
            (Some(control), Some(session)) => Some(control.min(session)),
            (control, session) => control.or(session),
        };
        let mut timing = result.timing;
        timing.capture_start_ms = self.segments[index].capture_start_ms;
        timing.preparation_ms = self.snapshot.timing.preparation_ms;
        if text.trim().is_empty() {
            self.apply_no_speech(index, description, directory, timing, stopped_at);
            return;
        }
        let usage = self.sessions.segment_usage(&directory);
        let is_current = sequence >= self.current_sequence;
        let save_started = Instant::now();
        let saved = self.save_complete_text(sequence, &text, is_current);
        timing.save_ms = Some(elapsed_ms(save_started));
        if let Err(error) = saved {
            // Nothing is written to a target and the audio stays as the recovery
            // basis. The complete text stays readable for this segment.
            if is_current {
                self.current_sequence = sequence;
                self.snapshot.text = text.clone();
                self.snapshot.delivery = None;
                self.snapshot.refined_text = None;
            }
            self.segments[index].text = Some(text);
            self.segments[index].usage = usage;
            self.segments[index].timing = timing.clone();
            if let Some(audio) = retry_audio {
                self.segments[index].retained = Some(RetainedCapture { audio });
            }
            self.fail_segment(index, error, timing);
            return;
        }
        self.current_sequence = self.current_sequence.max(sequence);
        self.segments[index].usage = usage.clone();
        self.segments[index].timing = timing.clone();
        self.segments[index].finalizing = true;
        let tx = self.completion_tx.clone();
        let worker_text = text.clone();
        let worker_timing = timing.clone();
        let worker_directory = directory.clone();
        thread::spawn(move || {
            let delivery_started = Instant::now();
            let outcome = if recovered {
                DeliveryOutcome::fallback(recovered_not_delivered())
            } else {
                deliver(target, &worker_text)
            };
            let mut timing = worker_timing;
            timing.delivery_ms = Some(elapsed_ms(delivery_started));
            let _ = tx.send(Delivered {
                sequence,
                text: worker_text,
                description,
                outcome,
                timing,
                directory: worker_directory,
                stopped_at,
                usage,
            });
        });
    }

    fn apply_no_speech(
        &mut self,
        index: usize,
        description: String,
        directory: PathBuf,
        mut timing: Timing,
        stopped_at: Option<Instant>,
    ) {
        timing.stop_to_result_ms = stopped_at.map(elapsed_ms);
        let primary = index + 1 == self.segments.len();
        {
            let segment = &mut self.segments[index];
            segment.state = SegmentState::NoSpeech;
            segment.session_state = SessionState::Complete;
            segment.detail = format!(
                "No valid speech detected; nothing was delivered.\nPlayback: {description}"
            );
            segment.timing = timing.clone();
        }
        // A no-speech segment has no result to keep, so its audio and record go.
        let cleanup = fs::remove_dir_all(&directory).map_err(|error| error.to_string());
        self.remove_segment_record(index);
        self.segments_dirty = true;
        self.persist_segment(index);
        if primary {
            self.snapshot.timing = timing.clone();
            self.snapshot.delivery = None;
            self.record_latency("NoSpeech", &timing, &directory, 0, None);
            self.snapshot.retained_audio.clear();
            self.snapshot.retry_available = false;
            self.delivery_memory = None;
            self.snapshot.refined_text = None;
            self.snapshot.refine = Refine::Unavailable(
                "The latest capture contained no speech, so there is no complete result to refine."
                    .into(),
            );
            match cleanup {
                Ok(()) => {
                    self.snapshot.phase = Phase::NoSpeech;
                    self.snapshot.detail = format!(
                        "No valid speech detected; nothing was delivered. The previous current result is unchanged.\nPlayback: {description}"
                    );
                }
                Err(error) => {
                    self.segments[index].retained_paths = audio_paths(&directory);
                    self.fail(
                        Some(&directory),
                        format!(
                            "No valid speech detected, but temporary audio could not be cleaned: {error}"
                        ),
                    );
                }
            }
        } else if let Err(error) = cleanup {
            self.segments[index].detail = format!(
                "No valid speech detected, but this segment's temporary audio could not be cleaned: {error}"
            );
        }
        self.refresh_recovery_snapshot();
    }

    fn apply_delivery(&mut self, delivered: Delivered) {
        let Some(index) = self
            .segments
            .iter()
            .position(|segment| segment.sequence == delivered.sequence)
        else {
            return;
        };
        let primary = index + 1 == self.segments.len();
        let current = delivered.sequence == self.current_sequence;
        let delivery = delivered.outcome.delivery;
        let detail = delivered.outcome.detail.clone();
        {
            let segment = &mut self.segments[index];
            segment.finalizing = false;
            segment.state = SegmentState::Result;
            segment.session_state = SessionState::Complete;
            segment.text = Some(delivered.text.clone());
            segment.delivery = Some(delivery);
            segment.usage = delivered.usage.clone();
            segment.timing = delivered.timing.clone();
            segment.detail = detail.clone();
        }
        let mut cleanup_error = None;
        match fs::remove_dir_all(&delivered.directory) {
            Ok(()) => {}
            Err(error) => cleanup_error = Some(error.to_string()),
        }
        let keep_record = delivery == Delivery::Fallback || cleanup_error.is_some();
        if let Some(record) = self.store.record_mut(delivered.sequence) {
            record.state = self.segments[index].state.name().to_owned();
            record.detail = self.segments[index].detail.clone();
            record.text_file = Some(SegmentStore::text_name(delivered.sequence));
            record.text_chars = delivered.text.chars().count();
            record.delivery = Some(delivery_name(delivery).into());
            record.usage = delivered.usage.clone();
            record.updated_utc = segments::now_utc();
            if self.store.problem().is_none() {
                if let Err(error) = self.store.persist() {
                    self.note_record_problem(error);
                }
            }
        }
        if let Some(error) = &cleanup_error {
            self.segments[index].retained_paths = audio_paths(&delivered.directory);
            self.segments[index].detail = format!(
                "{detail}\nThe temporary audio for this segment could not be cleaned ({error}); the result is still complete."
            );
            self.persist_segment(index);
        }
        if current {
            self.snapshot.text = delivered.text.clone();
            self.snapshot.delivery = Some(delivery);
            self.delivery_memory = delivered.outcome.memory;
            self.snapshot.refined_text = None;
            self.snapshot.refine = match self.refine_availability() {
                Ok(()) => Refine::Available,
                Err(reason) => Refine::Unavailable(reason),
            };
            let mut timing = delivered.timing.clone();
            timing.stop_to_result_ms = delivered.stopped_at.map(elapsed_ms);
            self.snapshot.timing = timing.clone();
            self.record_latency(
                "Result",
                &timing,
                &delivered.directory,
                delivered.text.chars().count(),
                Some(delivery),
            );
        }
        if primary {
            self.snapshot.retained_audio.clear();
            self.snapshot.retry_available = false;
            match &cleanup_error {
                None => {
                    self.snapshot.phase = Phase::Result;
                    self.snapshot.detail = self.with_glossary_note(format!(
                        "{}\nPlayback: {}",
                        detail, delivered.description
                    ));
                }
                Some(error) => self.fail(
                    Some(&delivered.directory),
                    format!(
                        "Could not save the current result or clean temporary audio: {error}"
                    ),
                ),
            }
        }
        if keep_record {
            self.persist_segment(index);
        } else {
            self.remove_segment_record(index);
        }
        self.refresh_recovery_snapshot();
        self.segments_dirty = true;
    }

    /// Record one failure for exactly one segment. A failure from an older segment
    /// changes nothing the capsule shows; only the newest segment can move the
    /// global phase to `Failure`.
    fn fail_segment(&mut self, index: usize, error: String, timing: Timing) {
        let directory = self.segments[index].directory.clone();
        let paths = audio_paths(&directory);
        let partial = fs::read_to_string(directory.join("partial-result.txt"))
            .ok()
            .filter(|text| !text.is_empty());
        let detail = failure_detail(&error, &paths);
        {
            let segment = &mut self.segments[index];
            segment.state = SegmentState::Failed;
            segment.session_state = SessionState::Failed;
            segment.terminal = true;
            segment.retained_paths = paths.clone();
            segment.timing = timing.clone();
            segment.detail = detail;
            if let Some(partial) = &partial {
                segment.detail.push_str(&format!(
                    ". The capture was interrupted, so this text is incomplete and kept with this segment: {}. This is not a completed transcription.",
                    partial.chars().count()
                ));
            }
        }
        self.segments_dirty = true;
        self.persist_segment(index);
        if index + 1 == self.segments.len() {
            self.snapshot.timing = timing.clone();
            self.record_latency("Failure", &timing, &directory, 0, None);
            self.fail(Some(&directory), error);
        }
        self.refresh_recovery_snapshot();
    }

    /// Fill `segments.json` from the current segment state. Cheap enough to call on
    /// every transition; the index is a small text document.
    fn persist_segment(&mut self, index: usize) {
        let Some(segment) = self.segments.get(index) else {
            return;
        };
        let sequence = segment.sequence;
        let state = segment.state.name().to_owned();
        let detail = segment.detail.clone();
        let Some(record) = self.store.record_mut(sequence) else {
            return;
        };
        if record.state == state && record.detail == detail {
            return;
        }
        record.state = state;
        record.detail = detail;
        record.updated_utc = segments::now_utc();
        if self.store.problem().is_some() {
            return;
        }
        if let Err(error) = self.store.persist() {
            self.note_record_problem(error);
        }
    }

    fn remove_segment_record(&mut self, index: usize) {
        let Some(segment) = self.segments.get(index) else {
            return;
        };
        let sequence = segment.sequence;
        if let Err(error) = self.store.remove(sequence) {
            self.note_record_problem(error);
        }
    }

    fn note_record_problem(&mut self, error: String) {
        if self.record_problem.is_some() || error.is_empty() {
            return;
        }
        self.record_problem = Some(error.clone());
        if !self.snapshot.detail.contains(&error) {
            self.snapshot.detail.push_str(&format!("\n{error}"));
        }
    }

    fn refresh_recovery_snapshot(&mut self) {
        self.snapshot.retry_available = self.any_retained();
        match self
            .segments
            .iter()
            .rev()
            .find(|segment| !segment.retained_paths.is_empty())
        {
            Some(segment) => self.snapshot.retained_audio = segment.retained_paths.clone(),
            None => self.snapshot.retained_audio.clear(),
        }
    }

    /// Rebuild the panel's segment list, in capture order, when anything changed.
    fn refresh_segments(&mut self) {
        if !self.segments_dirty {
            return;
        }
        self.segments_dirty = false;
        self.snapshot.segments = self
            .segments
            .iter()
            .map(|segment| SegmentSummary {
                sequence: segment.sequence,
                state: segment.state,
                detail: segment.detail.clone(),
                text: segment.text.clone().unwrap_or_default(),
                delivery: segment.delivery,
                usage: segment.usage.clone(),
            })
            .collect();
    }

    /// Append one machine-readable latency record. The session directory is
    /// removed after success, so this keeps the measured breakdown in the
    /// runtime directory for later review instead of relying on GUI timing.
    fn record_latency(
        &self,
        phase: &str,
        timing: &Timing,
        directory: &Path,
        text_chars: usize,
        delivery: Option<Delivery>,
    ) {
        use std::io::Write;
        let entry = serde_json::json!({
            "utc": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or_default(),
            "session": directory.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            "phase": phase,
            "backend": self.snapshot.backend,
            "delivery": format!("{delivery:?}"),
            "text_chars": text_chars,
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
        let mut retained_audio = Vec::new();
        if let Some(directory) = directory {
            retained_audio = audio_paths(directory);
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
        self.snapshot.detail = failure_detail(&error, &retained_audio);
        self.snapshot.retained_audio.clear();
        for path in retained_audio {
            if !self.snapshot.retained_audio.contains(&path) {
                self.snapshot.retained_audio.push(path);
            }
        }
        if let Some(directory) = directory {
            let path = directory.join("partial-result.txt");
            if let Ok(text) = fs::read_to_string(&path) {
                if !text.is_empty() {
                    self.snapshot.text = text;
                    self.snapshot.detail.push_str(&format!(". The capture was interrupted, so this text is incomplete: it is shown in the expanded result panel and stored at {}. This is not a completed transcription.", path.display()));
                }
            }
        }
        self.segments_dirty = true;
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
}

fn delivery_name(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Appended => "appended",
        Delivery::Fallback => "fallback",
    }
}

/// One failure message with the retained audio paths, never a credential.
fn failure_detail(error: &str, retained_audio: &[PathBuf]) -> String {
    let error = error.trim_end_matches('.').to_string();
    if retained_audio.is_empty() {
        error
    } else {
        format!(
            "{error}. Audio retained for local recovery: {}",
            retained_audio
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Every audio file still under one segment's directory, recursively. Used only to
/// report what is recoverable; nothing here is deleted.
fn audio_paths(directory: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
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
                {
                    out.push(path);
                }
            }
        }
    }
    out
}

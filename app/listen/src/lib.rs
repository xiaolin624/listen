use std::{
    collections::{BTreeMap, BTreeSet},
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
pub(crate) mod delivery;
pub mod glossary;
#[cfg(windows)]
pub mod paste;
pub mod recognition;
pub(crate) mod recovery;
pub(crate) mod segments;
#[cfg(windows)]
pub mod sensevoice;
pub mod storage;
pub mod streaming;
pub mod usage;
pub use usage::UsageSummary;
#[cfg(windows)]
pub mod target;
#[cfg(windows)]
pub mod whisper;
#[cfg(windows)]
pub mod whisper_host;
#[cfg(windows)]
pub mod whisper_timing;
use delivery::{DeliveryLedger, LatePlan};
use recognition::{
    LocalSessions, RecognitionSession, RecognitionSessions, SegmentUsage, SessionEvent,
    SessionFailure, SessionId, SessionRequest, SessionResult, SessionState, SessionUpdate,
};
use segments::SegmentStore;
use storage::Storage;
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
///
/// `stop_to_confirmed_ms` is the ticket #38 end-to-end number: from the Stop
/// control to the moment the real target's complete replacement was read back and
/// the confirmed append was durably recorded. It is present only for a confirmed
/// append; a fallback deliberately has no target read-back to claim. The stages
/// partition that interval: stop finalisation ([`Timing::finalize_ms`]), the cloud
/// final confirmation ([`Timing::cloud_drain_ms`] + [`Timing::cloud_final_ms`]), the
/// durable save ([`Timing::save_ms`]), application scheduling
/// ([`Timing::schedule_ms`] and its three measured gaps) and target delivery
/// ([`Timing::delivery_ms`] and its read/write/lock/record parts).
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
    /// Stop control to the confirmed, durably recorded target read-back. `None` when
    /// nothing was written to the bound target (a fallback result).
    pub stop_to_confirmed_ms: Option<u64>,
    /// Cloud: the successful transport connect of the final attempt.
    pub cloud_connect_ms: Option<u64>,
    /// Cloud: session start to the vendor's first `task-started`, the first
    /// connection/handshake cost. It happens while audio is still being captured.
    pub first_task_started_ms: Option<u64>,
    /// Cloud: capture finalisation to `finish-task` sent. This is the final upload
    /// drain of received audio.
    pub cloud_drain_ms: Option<u64>,
    /// Cloud: `finish-task` sent to the vendor's `task-finished`. Only a whole-task
    /// confirmation counts; an intermediate sentence is never this stage.
    pub cloud_final_ms: Option<u64>,
    /// Application scheduling: from the Stop control to the start of the durable
    /// save, minus the adapter's own final stages. It includes the stop-time
    /// durable segment record, event delivery and the poll cadence.
    pub event_wait_ms: Option<u64>,
    /// Application scheduling: the gap between the durable save and the delivery
    /// worker starting (thread start plus the one-target-writer gate).
    pub delivery_queue_ms: Option<u64>,
    /// Application scheduling: the gap between the confirmed delivery and the poll
    /// that applies it to the panel/phase. It cannot change the target text.
    pub apply_wait_ms: Option<u64>,
    /// The sum of the three application scheduling gaps above.
    pub schedule_ms: Option<u64>,
    /// Time spent waiting for the one-target-writer gate (an older segment's
    /// delivery). A backlog must never make the new segment's write wait.
    pub delivery_lock_ms: Option<u64>,
    /// The target draft read immediately before the write.
    pub delivery_read_ms: Option<u64>,
    /// The one write attempt and its complete read-back confirmation, as the target
    /// adapter reports them.
    pub delivery_write_ms: Option<u64>,
    /// The fail-closed delivery record (`delivery.json`) saves before and after the
    /// write. This is the part that grows with confirmed history.
    pub delivery_save_ms: Option<u64>,
}

#[derive(Clone, Copy)]
pub enum Control {
    Start,
    Stop,
    Toggle,
    Retry,
    CleanupAudio,
    /// Explicitly delete the pending recording and/or pending result of exactly one
    /// capture order, at the user's request (#36). Every other segment, including
    /// unfinished work, is untouched; a segment that is still recovering is stopped
    /// safely first, and its late events can never recreate it.
    DeleteSegment(u64),
    /// 精修: ask the configured text-refine port to correct the complete current
    /// result and replace only the transcript that was appended from it.
    Refine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Appended,
    Fallback,
    /// A write to the original target had started (or may have happened) and its
    /// outcome cannot be proven — a lost response, or a process that ended while
    /// the write was in flight. The complete result is kept and is never written
    /// again automatically (#34, #35); the panel says the state may already hold it.
    Uncertain,
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
    /// This month's own cloud-recognition cost reached the CNY 100 reminder line.
    /// It is a reminder only: nothing was stopped, cancelled, purchased or raised,
    /// and the capture flow is unaffected.
    UsageReminder,
    /// Pending capture audio is approaching the configured budget (#36). Capture
    /// still works; the reminder names the consequence so the user can clean up or
    /// raise the limit before the hard stop.
    StorageWarning,
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
            Notice::UsageReminder => "本月云端识别费用估计已达每月 100 元提醒线。这只是提醒：采集、识别与安全回填照常继续，不会自动停用功能、取消当前录音、购买套餐或提高额度。".into(),
            Notice::StorageWarning => "待处理录音已接近缓存上限。达到上限或磁盘余量低于底线时，Listen 会停止开始新的采集并保留已经收到的声音和旧段；可清理不再需要的段，或调整待处理音频预算。".into(),
        }
    }
}

/// Line endings are not part of a target's meaning: editors may report the same
/// content with CRLF or LF, so every refine comparison uses this form.
pub(crate) fn normalize_newlines(value: &str) -> String {
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
    /// Cumulative usage and cost of this tool's own cloud recognition requests,
    /// read from the durable trial ledger. Confirmed, conservatively estimated and
    /// unknown parts stay separate. `None` when the recognition adapter keeps no
    /// such ledger (the local engine).
    pub usage: Option<UsageSummary>,
    /// Every segment of this run in capture order. An older waiting segment keeps
    /// its own entry and its own complete text; `text` above is only the newest
    /// complete result.
    pub segments: Vec<SegmentSummary>,
    /// The measured storage conditions (#36): pending audio across every segment,
    /// the configured budget and warning line, free disk against the floor, the
    /// separate saved-text bound and the last real storage fault. `None` only in a
    /// synthetic snapshot that was never produced by an `Application`.
    pub storage: Option<storage::StorageStatus>,
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

/// The elapsed milliseconds from `earlier` to `later`, clamped at zero so a
/// timestamp taken a fraction later than its reference can never underflow.
fn millis_between(later: Instant, earlier: Instant) -> u64 {
    later
        .saturating_duration_since(earlier)
        .as_millis()
        .min(u64::MAX as u128) as u64
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
    /// When the delivery worker returned: the append was confirmed by a target
    /// read-back (or the fallback decision was made). The next poll only applies it.
    delivery_finished_at: Instant,
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
    /// The one stop-time draft read has been started for this segment (or was
    /// already recorded). It runs off the UI thread and may still be in flight.
    stop_draft_requested: bool,
    /// A saved result's target delivery is still running.
    finalizing: bool,
    stop_requested: Option<Instant>,
    retry_audio: Option<CapturedAudio>,
    glossary: glossary::Glossary,
    /// The target bound at this segment's start; used exactly once. It is shared
    /// with the stop-time draft reader so a late result can still reach the same
    /// editor after the application thread moved on (#34).
    target: AppResult<Option<Arc<Mutex<Box<dyn Target>>>>>,
    retained: Option<RetainedCapture>,
    /// Audio paths kept on disk from a failed attempt, shown to the user.
    retained_paths: Vec<PathBuf>,
    state: SegmentState,
    session_state: SessionState,
    detail: String,
    /// Set only when the storage bounds stopped this segment's capture (#36). It is
    /// kept with the segment so a later progress detail cannot erase why capture
    /// stopped, and it is never mistaken for a network wait.
    storage_note: Option<String>,
    endpoint: Option<CaptureEndpoint>,
    progress: CaptureProgress,
    capture_start_ms: Option<u64>,
    text: Option<String>,
    delivery: Option<Delivery>,
    usage: Option<SegmentUsage>,
    timing: Timing,
    /// This segment was recovered from a previous process after a restart (#35).
    /// Its original target cannot be reused, so its result is always a late,
    /// panel-only result even when the recognition adapter reports a normal one.
    recovered: bool,
}

/// What a successful append left behind. The bound target is kept alive so a later
/// refine can read it again through the same adapter; `before` is the complete
/// content read before the append, `after` is the content written, and `appended` is
/// the transcript itself. A refine may only replace the appended transcript when the
/// target still holds exactly `after`.
struct DeliveryMemory {
    target: Arc<Mutex<Box<dyn Target>>>,
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
    /// The measured parts of the delivery, carried into [`Timing`]. A fallback on
    /// the no-target path has none of them.
    lock_ms: Option<u64>,
    read_ms: Option<u64>,
    write_ms: Option<u64>,
    save_ms: Option<u64>,
}

impl DeliveryOutcome {
    fn fallback(detail: String) -> Self {
        Self {
            delivery: Delivery::Fallback,
            detail,
            memory: None,
            lock_ms: None,
            read_ms: None,
            write_ms: None,
            save_ms: None,
        }
    }
}

/// Only one delivery may write to a target at a time. Segments finish at their own
/// pace, but one editor's read/modify/write pair must never interleave with another
/// segment's, or a confirmed append would be misread as a user edit. The lock is
/// taken on a delivery worker's thread, never on the application thread, so a slow
/// editor can never block a new capture.
static TARGET_WRITER: Mutex<()> = Mutex::new(());

/// Which delivery rule produced this append. A normal result appends to the latest
/// draft; a late one may only append to a draft proven unchanged since its Stop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppendKind {
    Normal,
    Late,
}

/// A normal result append. The latest draft is read at delivery time and kept, so
/// edits made during the capture stay in place (#33 keeps this per segment).
fn deliver(
    target: AppResult<Option<Arc<Mutex<Box<dyn Target>>>>>,
    text: &str,
    sequence: u64,
    ledger: &DeliveryLedger,
) -> DeliveryOutcome {
    let target = match target {
        Ok(Some(target)) => target,
        Ok(None) => {
            let detail =
                not_delivered("No supported editable target was selected at capture start".into());
            ledger.keep_pending(sequence, &detail);
            return DeliveryOutcome::fallback(detail);
        }
        Err(error) => {
            let detail = not_delivered(error);
            ledger.keep_pending(sequence, &detail);
            return DeliveryOutcome::fallback(detail);
        }
    };
    let lock_started = Instant::now();
    let _writer = TARGET_WRITER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let lock_ms = elapsed_ms(lock_started);
    let read_started = Instant::now();
    let latest = match target
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .content()
    {
        Ok(latest) => latest,
        Err(error) => {
            let detail = not_delivered(error);
            ledger.keep_pending(sequence, &detail);
            return DeliveryOutcome::fallback(detail);
        }
    };
    let read_ms = elapsed_ms(read_started);
    let mut outcome = append_to_target(target, sequence, ledger, latest, text, AppendKind::Normal);
    outcome.lock_ms = Some(lock_ms);
    outcome.read_ms = Some(read_ms);
    outcome
}

/// A late result (it waited for the network or was recovered from retained audio).
/// The only proof that appending is safe is the draft read when this segment
/// stopped plus the tool's own confirmed appends since then. Everything else keeps
/// the complete result pending; the target is never retargeted, overwritten or
/// rolled back.
fn deliver_late(
    target: AppResult<Option<Arc<Mutex<Box<dyn Target>>>>>,
    text: &str,
    sequence: u64,
    ledger: &DeliveryLedger,
) -> DeliveryOutcome {
    let target = match target {
        Ok(Some(target)) => target,
        Ok(None) => {
            return keep_late(
                ledger,
                sequence,
                "no supported editable target was selected at capture start",
            );
        }
        Err(error) => return keep_late(ledger, sequence, &error),
    };
    if let Err(reason) = ledger.wait_baseline(sequence) {
        return keep_late(ledger, sequence, &reason);
    }
    let lock_started = Instant::now();
    let _writer = TARGET_WRITER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let lock_ms = elapsed_ms(lock_started);
    let read_started = Instant::now();
    let current = match target
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .content()
    {
        Ok(current) => current,
        Err(error) => {
            return keep_late(
                ledger,
                sequence,
                &format!("the original target could not be read ({error})"),
            );
        }
    };
    let read_ms = elapsed_ms(read_started);
    match ledger.plan_late_append(sequence, &current) {
        LatePlan::Keep { reason } => keep_late(ledger, sequence, &reason),
        LatePlan::Append => {
            let mut outcome =
                append_to_target(target, sequence, ledger, current, text, AppendKind::Late);
            outcome.lock_ms = Some(lock_ms);
            outcome.read_ms = Some(read_ms);
            outcome
        }
    }
}

/// The complete result is kept as this segment's own pending result. Never a write.
fn keep_late(ledger: &DeliveryLedger, sequence: u64, reason: &str) -> DeliveryOutcome {
    ledger.keep_pending(sequence, reason);
    DeliveryOutcome::fallback(late_kept(reason))
}

/// One append attempt, shared by both delivery rules. The write is never repeated:
/// a failed or unconfirmed write is diagnosed by exactly one read-back and the
/// complete text stays available. `begin_append` records the started state first;
/// a late append is refused when that durable marker cannot be written, because
/// then the single write could never be proven after a lost response.
fn append_to_target(
    target: Arc<Mutex<Box<dyn Target>>>,
    sequence: u64,
    ledger: &DeliveryLedger,
    before: String,
    text: &str,
    kind: AppendKind,
) -> DeliveryOutcome {
    let replacement = if before.is_empty() {
        text.to_string()
    } else {
        format!("{before}\n{text}")
    };
    // The fail-closed marker must be on disk before the write, and the confirmed
    // append on disk before the outcome is trusted. Both are measured: this is the
    // part of a delivery that grows with confirmed history.
    let save_started = Instant::now();
    let begin = ledger.begin_append(sequence, &before);
    let begin_ms = elapsed_ms(save_started);
    if let Err(reason) = begin {
        if kind == AppendKind::Late {
            return keep_late(ledger, sequence, &reason);
        }
    }
    let write_started = Instant::now();
    let write = target
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .replace(&before, &replacement);
    let write_ms = elapsed_ms(write_started);
    match write {
        Ok(()) => {
            let confirm_started = Instant::now();
            ledger.confirm_append(sequence, &before, &replacement);
            let confirm_ms = elapsed_ms(confirm_started);
            DeliveryOutcome {
                delivery: Delivery::Appended,
                detail: match kind {
                    AppendKind::Normal => "The complete result was appended to the original target. Review and send it manually. A copy remains in Listen.".into(),
                    AppendKind::Late => late_appended(),
                },
                memory: Some(DeliveryMemory {
                    target,
                    before,
                    after: replacement,
                    appended: text.to_string(),
                }),
                lock_ms: None,
                read_ms: None,
                write_ms: Some(write_ms),
                save_ms: Some(begin_ms + confirm_ms),
            }
        }
        Err(error) => {
            // Diagnose the failed write by reading the target again. Reading is not a
            // retry: the write is never repeated and the previous text is never restored.
            let observed = target
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .content();
            let failed = failed_write(&before, &replacement, &error, observed);
            if failed.uncertain {
                ledger.mark_uncertain(sequence, &failed.detail);
            } else {
                ledger.keep_pending(sequence, &failed.detail);
            }
            let detail = match kind {
                AppendKind::Normal => failed.detail,
                AppendKind::Late => late_failed(failed.detail),
            };
            DeliveryOutcome {
                delivery: failed.delivery,
                detail,
                memory: None,
                lock_ms: None,
                read_ms: None,
                write_ms: Some(write_ms),
                save_ms: Some(begin_ms),
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
    let Some(memory) = memory else {
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
    let observed = {
        let mut target = memory
            .target
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        target.content()
    };
    let observed = match observed {
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
    if let Err(error) = memory
        .target
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .replace(&observed, &replacement)
    {
        return RefineOutcome::NotApplied {
            refined,
            reason: format!(
                "writing the refined text to the original target failed ({error}), so the target was left as it was and the refined text stays in the panel"
            ),
        };
    }
    let readback = {
        let mut target = memory
            .target
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        target.content()
    };
    match readback {
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

/// A late result was appended because the original draft was proven unchanged
/// since the segment stopped.
fn late_appended() -> String {
    "The complete result was recovered after waiting for the network. The original target still held the draft recorded when this segment stopped, including Listen's own confirmed appends, so the complete result was appended once to its end and confirmed by reading it back. Review and send it manually; a copy remains in Listen.".into()
}

/// A late result keeps its complete text pending because appending could not be
/// proven safe. Nothing was written to any control.
fn late_kept(reason: &str) -> String {
    let reason = reason.trim_end_matches('.').trim_end();
    format!(
        "The complete result was recovered after waiting for the network. It was not appended automatically: {reason}. The complete text is saved on this machine and shown in the expanded result panel; review and copy it to the intended input. Nothing is sent automatically."
    )
}

/// The write itself was attempted for a late result and its outcome is not
/// confirmed. The failed-write diagnosis already says what the target holds.
fn late_failed(detail: String) -> String {
    format!("This complete result was recovered after waiting for the network. {detail}")
}

/// One failed write's honest report, including whether anything about it is still
/// uncertain. `uncertain` is true whenever the target may hold a complete or partial
/// write: the segment is then never appended again automatically.
struct FailedWrite {
    delivery: Delivery,
    detail: String,
    uncertain: bool,
}

/// Reports what the target actually holds after a failed write, without ever claiming
/// a complete delivery. The read-back distinguishes a refused write from a truncated
/// one, a late complete write, and a possibly partial one.
fn failed_write(
    previous: &str,
    intended: &str,
    error: &str,
    observed: AppResult<String>,
) -> FailedWrite {
    let advice = "Check the original target before copying; output is not retried automatically and the original text is not reverted.";
    let fallback = "The complete result is in the expanded result panel.";
    let observed = match observed {
        Ok(observed) => observed,
        Err(read_error) => {
            return FailedWrite {
                delivery: Delivery::Fallback,
                detail: format!(
                    "The write to the original target could not be confirmed ({error}); the target could not be read back ({read_error}). {advice} {fallback}"
                ),
                uncertain: true,
            };
        }
    };
    // Line endings are not part of the delivered text's meaning.
    let observed = observed.replace("\r\n", "\n");
    let previous = previous.replace("\r\n", "\n");
    let intended = intended.replace("\r\n", "\n");
    if observed == previous {
        FailedWrite {
            delivery: Delivery::Fallback,
            detail: format!(
                "The original target still holds its previous content, so nothing was written to it ({error}). {fallback} Review and copy it to the intended input."
            ),
            uncertain: false,
        }
    } else if observed == intended {
        FailedWrite {
            delivery: Delivery::Fallback,
            detail: format!(
                "The write to the original target could not be confirmed ({error}), but it now contains the complete appended text. {advice} A copy remains in Listen."
            ),
            uncertain: true,
        }
    } else if !observed.is_empty() && intended.starts_with(&observed) {
        FailedWrite {
            delivery: Delivery::Fallback,
            detail: format!(
                "The original target holds only {} of {} characters of the intended text, so the appended transcript is truncated; this is not a complete delivery. {advice} {fallback}",
                observed.chars().count(),
                intended.chars().count()
            ),
            uncertain: true,
        }
    } else {
        FailedWrite {
            delivery: Delivery::Fallback,
            detail: format!(
                "The write to the original target could not be confirmed ({error}); the target content changed and may be partially written. {advice} {fallback}"
            ),
            uncertain: true,
        }
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
    /// The durable delivery record and the late-backfill safety check (#34). It is
    /// shared with the delivery workers, which record the stopped draft, the
    /// started write and its confirmed outcome off the UI thread.
    delivery: Arc<DeliveryLedger>,
    record_problem: Option<String>,
    /// The configured storage bounds and the injected free-space probe (#36). All
    /// accounting decisions go through this one module.
    storage: Storage,
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
    /// Whether this process already raised [`Notice::UsageReminder`] for the
    /// current month, so one reached line is announced once instead of on every
    /// ledger refresh. Reset when the month figure falls back under the line.
    usage_reminder_announced: bool,
    /// The cumulative cost summary must be re-read, but only once no recognition
    /// session is in flight: the ledger's exclusive lock is also the paid-request
    /// admission lock (#37), and an app-side read must never make a worker's
    /// reservation fail.
    usage_pending: bool,
    /// Segments whose own cost still needs a safe-point ledger read.
    segment_usage_pending: BTreeSet<u64>,
    /// Startup recovery from the previous process (#35): what was found, and the
    /// capture-ordered queue of recordings that must be processed again. Only one
    /// old segment is processed at a time, and only while no capture or current
    /// result owns the recognition capacity.
    recovery: recovery::Recovery,
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
        let delivery = Arc::new(DeliveryLedger::open(&runtime));
        let delivery_problem = delivery.problem();
        if let Some(problem) = &delivery_problem {
            detail.push_str(&format!("\n{problem}"));
        }
        let (completion_tx, completion_rx) = mpsc::channel();
        let mut application = Self {
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
                usage: None,
                segments: Vec::new(),
                storage: None,
            },
            sessions,
            runtime,
            segments: Vec::new(),
            store,
            delivery,
            record_problem: delivery_problem,
            storage: Storage::new(),
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
            usage_reminder_announced: false,
            usage_pending: false,
            segment_usage_pending: BTreeSet::new(),
            recovery: recovery::Recovery::default(),
        };
        // The durable ledger already exists before the first capture, so the cost
        // this tool has accumulated is readable immediately and after a restart.
        // This is also the one safe place to raise an already-reached reminder.
        application.refresh_storage();
        application.refresh_usage();
        // Whatever the previous process left behind is recovered before the first
        // user action, so a restart continues the work instead of losing it (#35).
        application.recover_previous_session();
        application
    }

    /// Apply an explicit storage policy (#36). The default is a 1 GiB pending-audio
    /// budget, a 1 GiB free-disk floor and a warning at four fifths of the budget.
    pub fn with_storage_policy(mut self, policy: storage::StoragePolicy) -> Self {
        self.storage.set_policy(policy);
        self.refresh_storage();
        self
    }

    /// Replace the free-space probe. Production reads the runtime volume; tests
    /// inject a value so storage conditions are a state, never real disk pressure.
    pub fn with_free_space(mut self, free_space: Arc<dyn storage::FreeSpace>) -> Self {
        self.storage.set_free_space(free_space);
        self.refresh_storage();
        self
    }

    /// Measure the storage bounds and publish them beside the capture state. A
    /// warning is recorded once as a [`Notice`] and disappears when the measurement
    /// falls back below the warning line; a real storage fault is carried separately
    /// and is never turned into a network wait.
    fn refresh_storage(&mut self) {
        let status = self.storage.status(&self.runtime);
        self.apply_storage_status(status);
    }

    fn apply_storage_status(&mut self, status: storage::StorageStatus) {
        if status.near_limit() && !status.blocking() {
            if self.snapshot.notice.is_none()
                || matches!(self.snapshot.notice, Some(Notice::StorageWarning))
            {
                self.snapshot.notice = Some(Notice::StorageWarning);
            }
        } else if matches!(self.snapshot.notice, Some(Notice::StorageWarning)) {
            self.snapshot.notice = None;
        }
        self.snapshot.storage = Some(status);
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
            segment.finalizing || (segment.handle.is_some() && segment.stopped && !segment.terminal)
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

    /// Re-read the durable cost summary. It is deliberately **not** called while a
    /// cloud session is active: the ledger's exclusive lock is also the paid-request
    /// admission lock, so a UI read that wins the race would make the worker's next
    /// reservation fail and block an otherwise healthy capture. The safe points are
    /// construction, a start that failed before any worker existed, and a completed
    /// or failed session (its worker has already returned). A locked or unreadable
    /// ledger reports "unknown", never zero.
    fn refresh_usage(&mut self) {
        let usage = self.sessions.usage();
        self.apply_usage(usage);
    }

    /// Publish one cost summary and the one reminder it may raise. The reminder is
    /// reported beside the capture flow; it never refuses, stops or cancels one.
    fn apply_usage(&mut self, usage: Option<UsageSummary>) {
        let Some(usage) = usage else {
            // The local engine has no cloud ledger; that is not "zero spent".
            self.snapshot.usage = None;
            return;
        };
        // A lock held by the worker is not evidence that the cost became unknown:
        // keep the last readable figures instead of claiming an unknown total.
        if usage.unreadable
            && self
                .snapshot
                .usage
                .as_ref()
                .is_some_and(|known| !known.unreadable)
        {
            return;
        }
        let reached = usage.reminder_reached();
        self.snapshot.usage = Some(usage);
        if !reached {
            // A new month (or a re-read below the line) may announce again exactly
            // once; the reminder is never turned into a budget decision.
            self.usage_reminder_announced = false;
        } else if !self.usage_reminder_announced && self.snapshot.notice.is_none() {
            self.usage_reminder_announced = true;
            self.snapshot.notice = Some(Notice::UsageReminder);
        }
    }

    /// Whether any recognition session is still alive. While one is, the ledger
    /// must not be read at all: its exclusive lock is the paid-request admission
    /// lock, so a UI read that wins the race would make the worker's next
    /// reservation fail and block a healthy capture (#37).
    fn recognition_in_flight(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.handle.is_some() && !segment.terminal)
    }

    /// Ask for the cumulative cost summary once no recognition session is active.
    fn mark_usage_pending(&mut self) {
        self.usage_pending = true;
    }

    /// Ask for one segment's own cost once no recognition session is active.
    fn mark_segment_usage_pending(&mut self, sequence: u64) {
        self.segment_usage_pending.insert(sequence);
    }

    /// Read the deferred cost summaries, if it is safe now. Both reads are skipped
    /// while a recognition session is alive; the pending flags stay set until then.
    fn flush_deferred_usage(&mut self) {
        if self.recognition_in_flight() {
            return;
        }
        if self.usage_pending {
            self.usage_pending = false;
            self.refresh_usage();
        }
        if self.segment_usage_pending.is_empty() {
            return;
        }
        let pending: Vec<u64> = self.segment_usage_pending.iter().copied().collect();
        self.segment_usage_pending.clear();
        for sequence in pending {
            let Some(index) = self
                .segments
                .iter()
                .position(|segment| segment.sequence == sequence)
            else {
                continue;
            };
            let directory = self.segments[index].directory.clone();
            let Some(usage) = self.sessions.segment_usage(&directory) else {
                // No metered request for this segment, or the ledger is unreadable
                // from another process. It is never reported as zero.
                continue;
            };
            self.segments[index].usage = Some(usage.clone());
            if let Some(record) = self.store.record_mut(sequence) {
                record.usage = Some(usage);
                record.updated_utc = segments::now_utc();
                if self.store.problem().is_none() {
                    if let Err(error) = self.store.persist() {
                        self.note_record_problem(error);
                    }
                }
            }
            self.segments_dirty = true;
        }
    }

    /// Recover what the previous process left in the runtime directory (#35).
    ///
    /// A segment whose complete text is already durable becomes its own pending
    /// result and is never recognized again; its audio is released because the
    /// text is now the durable copy. A segment that still only has its recording
    /// is validated and queued for background processing in capture order. No
    /// target is bound: a target handle cannot survive a restart, so every
    /// recovered result stays in the panel.
    fn recover_previous_session(&mut self) {
        let deliveries = self.delivery.states();
        let delivery_problem = self.delivery.problem().is_some();
        let mut plan = recovery::plan(
            &self.runtime,
            self.store.records(),
            &deliveries,
            delivery_problem,
        );
        if plan.is_empty() {
            return;
        }
        // The current 术语表 is the only term source for a segment whose index entry
        // was lost; the detail says the capture-time snapshot could not be kept.
        let current_terms: Vec<String> = self
            .glossary_file()
            .and_then(|path| glossary::Glossary::load(path).ok())
            .map(|loaded| loaded.canonical_terms().map(str::to_owned).collect())
            .unwrap_or_default();
        let mut problems = plan.problems.clone();
        problems.extend(plan.adopt(&mut self.store, &current_terms));
        let mut recovered = plan.segments;
        let mut resumed = 0;
        let mut texts = 0;
        for segment in &mut recovered {
            // A version-1 record from before the term snapshot existed cannot prove
            // which terms this capture used; the current file is used and said so.
            if segment.resumable && segment.glossary_terms.is_empty() && !current_terms.is_empty()
            {
                segment.glossary_terms = current_terms.clone();
                segment.detail.push_str(" The fixed term snapshot of this segment was not recorded by the previous version, so the current 术语表 is used for the recovered processing.");
            }
            if segment.resumable {
                resumed += 1;
            } else if segment.text.is_some() {
                texts += 1;
            }
            // The complete text is durably readable, so the production audio is no
            // longer the only recovery basis. This is the crash window "the result
            // was saved but the audio was not cleaned yet".
            if segment.release_audio
                && let Some(directory) = &segment.directory
            {
                match fs::remove_dir_all(directory) {
                    Ok(()) => {
                        segment.retained_paths.clear();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        segment.retained_paths = audio_paths(directory);
                        problems.push(format!(
                            "Segment {}'s complete text was recovered, but its temporary audio could not be released ({error}); the result is still complete and the remaining audio is kept.",
                            segment.sequence
                        ));
                    }
                }
            }
            let recovered_segment = self.recovered_segment(segment);
            // The durable record now says what recovery decided, so the next launch
            // repeats no work and the pending result keeps its own delivery state.
            if let Some(record) = self.store.record_mut(segment.sequence) {
                record.state = recovered_segment.state.name().to_owned();
                record.detail = recovered_segment.detail.clone();
                if let Some(text) = &recovered_segment.text {
                    record.text_file = Some(SegmentStore::text_name(segment.sequence));
                    record.text_chars = text.chars().count();
                }
                if let Some(delivery) = recovered_segment.delivery {
                    record.delivery = Some(delivery_name(delivery).into());
                }
                record.updated_utc = segments::now_utc();
            }
            self.segments.push(recovered_segment);
        }
        if self.store.problem().is_none()
            && let Err(error) = self.store.persist()
        {
            self.note_record_problem(error);
        }
        // The newest recovered complete result is the current result, exactly as it
        // was before the restart. `text` is only the newest one; every older result
        // keeps its own entry in the panel.
        self.adopt_recovered_current_result();
        let problems = {
            let mut all = problems;
            if let Some(problem) = self.store.problem() {
                all.push(problem.to_string());
            }
            all
        };
        self.recovery.start(recovered, problems);
        if let Some(summary) = self.recovery.summary() {
            let detail = recovery::summary_detail(summary);
            if !detail.is_empty() {
                if self.snapshot.detail.is_empty() {
                    self.snapshot.detail = detail;
                } else {
                    self.snapshot.detail.push('\n');
                    self.snapshot.detail.push_str(&detail);
                }
            }
        }
        if self.control_error.is_none() && self.snapshot.phase != Phase::Failure {
            if resumed > 0 {
                self.snapshot.phase = Phase::Processing;
                self.snapshot.recognition = Some(SessionState::AwaitingFinal);
            } else if texts > 0 {
                self.snapshot.phase = Phase::Result;
                self.snapshot.recognition = Some(SessionState::Complete);
                self.snapshot.delivery = self.recovered_current_delivery();
            }
        }
        // Whatever was already durable stays listed even before the first poll.
        if !self.snapshot.text.is_empty() {
            self.snapshot.refine = match self.refine_availability() {
                Ok(()) => Refine::Available,
                Err(reason) => Refine::Unavailable(reason),
            };
        }
        self.segments_dirty = true;
        self.refresh_segments();
        self.refresh_recovery_snapshot();
        // Start the oldest recovered recording at once; the rest follow one at a
        // time from `poll`, so a new capture always wins the capacity first.
        self.advance_recovery();
    }

    /// Rebuild one application segment from what the previous process left. It has
    /// no session handle and no usable target: a recovered result is panel-only.
    fn recovered_segment(
        &mut self,
        recovered: &recovery::RecoveredSegment,
    ) -> SegmentSession {
        let session_state = match recovered.state {
            SegmentState::Processing => SessionState::AwaitingFinal,
            SegmentState::Failed => SessionState::Failed,
            SegmentState::NoSpeech | SegmentState::Result => SessionState::Complete,
            SegmentState::Capturing | SegmentState::WaitingForNetwork => {
                SessionState::AwaitingFinal
            }
        };
        SegmentSession {
            id: SessionId(0),
            sequence: recovered.sequence,
            directory: recovered.directory.clone().unwrap_or_default(),
            handle: None,
            stopped: true,
            terminal: !recovered.resumable,
            // A draft read needs a live target; after a restart there is none, so
            // the delivery record already says the original target is unusable.
            stop_draft_requested: true,
            finalizing: false,
            stop_requested: None,
            retry_audio: None,
            glossary: glossary::Glossary::from_canonical_terms(&recovered.glossary_terms),
            target: Err(recovery::NO_TARGET_AFTER_RESTART.into()),
            retained: None,
            retained_paths: recovered.retained_paths.clone(),
            state: recovered.state,
            session_state,
            detail: recovered.detail.clone(),
            storage_note: None,
            endpoint: None,
            progress: CaptureProgress::default(),
            capture_start_ms: None,
            text: recovered.text.clone(),
            delivery: recovered.delivery,
            usage: recovered.usage.clone(),
            timing: Timing::default(),
            recovered: true,
        }
    }

    /// Make the newest recovered complete text the current result, and keep the
    /// durable `current-result.txt` in step when it lags behind.
    fn adopt_recovered_current_result(&mut self) {
        let newest = self
            .segments
            .iter()
            .filter_map(|segment| {
                segment
                    .text
                    .as_ref()
                    .map(|text| (segment.sequence, text.clone()))
            })
            .max_by_key(|(sequence, _)| *sequence);
        let Some((sequence, text)) = newest else {
            return;
        };
        self.current_sequence = sequence;
        self.snapshot.text = text.clone();
        self.snapshot.delivery = self.recovered_current_delivery();
        let current_result = self.runtime.join("current-result.txt");
        let differs = fs::read_to_string(&current_result)
            .map(|existing| existing != text)
            .unwrap_or(true);
        if differs
            && let Err(error) = cloud::persist_result(&current_result, text.as_bytes())
        {
            self.note_record_problem(format!(
                "Could not save the recovered current result: {error}"
            ));
        }
    }

    fn recovered_current_delivery(&self) -> Option<Delivery> {
        self.segments
            .iter()
            .find(|segment| segment.sequence == self.current_sequence)
            .and_then(|segment| segment.delivery)
    }

    /// Process the next recovered recording, but only while no capture and no
    /// current result owns the recognition capacity. Only one old segment runs at
    /// a time, so a new capture always wins and an old segment's failure cannot
    /// block it.
    fn advance_recovery(&mut self) {
        let busy = self.refine_job.is_some()
            || self
                .segments
                .iter()
                .any(|segment| segment.handle.is_some() && !segment.terminal);
        if busy {
            return;
        }
        let Some(resume) = self.recovery.take_next() else {
            return;
        };
        self.resume_recovered(resume);
    }

    /// Hand one recovered recording back to the recognition adapter. Its original
    /// target is recorded as unusable first, so no later delivery can claim a
    /// write this process cannot prove.
    fn resume_recovered(&mut self, resume: recovery::Resume) {
        let sequence = resume.sequence;
        let Some(index) = self
            .segments
            .iter()
            .position(|segment| segment.sequence == sequence)
        else {
            return;
        };
        self.delivery
            .record_no_target(sequence, recovery::NO_TARGET_AFTER_RESTART);
        let audio = CapturedAudio {
            path: resume.audio.path.clone(),
            description: resume.audio.description.clone(),
        };
        let id = self.next_id();
        let request = SessionRequest {
            id,
            directory: self.segments[index].directory.clone(),
            glossary: self.segments[index].glossary.clone(),
        };
        let detail = self.segments[index].detail.clone();
        match self.sessions.retry(request, audio) {
            Ok(handle) => {
                let segment = &mut self.segments[index];
                segment.id = id;
                segment.handle = Some(handle);
                segment.state = SegmentState::Processing;
                segment.session_state = SessionState::AwaitingFinal;
                // The recovered/incomplete note stays until the adapter's own
                // status says something more specific.
                segment.detail = detail;
                self.persist_segment(index);
            }
            Err(error) => {
                let error = format!(
                    "The recovered recording could not be processed again automatically: {error}"
                );
                self.fail_segment(index, error, Timing::default());
                self.segments[index].detail.push_str(recovery::KEPT_AFTER_FAILED_RESUME);
                self.segments[index].retained_paths = resume.audio.retained.clone();
                self.segments[index].state = SegmentState::Failed;
                self.segments[index].terminal = true;
                self.persist_segment(index);
                self.segments_dirty = true;
                self.refresh_recovery_snapshot();
                if index + 1 == self.segments.len() {
                    self.snapshot.detail = self.segments[index].detail.clone();
                    if let Some(summary) = self.recovery.summary() {
                        let line = recovery::summary_detail(summary);
                        if !line.is_empty() {
                            self.snapshot.detail.push('\n');
                            self.snapshot.detail.push_str(&line);
                        }
                    }
                }
            }
        }
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
            Ok(()) if !self.snapshot.text.is_empty() => Refine::Available,
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
            // An explicit per-segment delete never waits for the whole backlog: it
            // removes exactly the selected segment's pending recording/result and
            // stops its later processing safely.
            Control::DeleteSegment(sequence) => self.delete_segment(sequence),
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
        // Storage measurements change with every action (a completed segment may
        // release its audio, a delete may free it); publish the fresh values.
        self.refresh_storage();
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
        // Storage is checked before the target is bound and before any session
        // directory exists: over the pending-audio budget or at/below the free-disk
        // floor, no capture may start and nothing may be overwritten.
        let storage_status = self.storage.status(&self.runtime);
        if let Some(refusal) = storage::start_refusal(&storage_status) {
            self.apply_storage_status(storage_status);
            self.snapshot.phase = Phase::Failure;
            self.snapshot.recognition = Some(SessionState::Failed);
            self.snapshot.detail = refusal;
            self.segments_dirty = true;
            self.refresh_segments();
            return;
        }
        // Bind once before any UI feedback. Recognition receives neither the target
        // draft nor an editor identity. The handle is shared so the draft read at
        // Stop and a later late delivery still address this same bound editor (#34).
        let bound_target = self
            .targets
            .as_ref()
            .map_or(Ok(None), |targets| targets.focused_target())
            .map(|target| target.map(|target| Arc::new(Mutex::new(target))));
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
                let terms: Vec<String> =
                    glossary.canonical_terms().map(str::to_owned).collect();
                let (sequence, record_error) =
                    self.store.begin(&directory, &status.detail, &terms);
                let capture_start_ms = Some(elapsed_ms(start_requested));
                let segment = SegmentSession {
                    id,
                    sequence,
                    directory,
                    handle: Some(handle),
                    stopped: false,
                    terminal: false,
                    stop_draft_requested: false,
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
                    storage_note: None,
                    endpoint: status.endpoint.clone(),
                    progress: status.progress.clone(),
                    capture_start_ms,
                    text: None,
                    delivery: None,
                    usage: None,
                    timing: Timing::default(),
                    recovered: false,
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
                // A capture that started means the storage state is usable again;
                // the next real write failure will set a new fault.
                self.storage.clear_fault();
                self.refresh_storage();
            }
            Err(error) => {
                self.snapshot.recognition = Some(SessionState::Failed);
                self.snapshot.phase = Phase::Failure;
                self.snapshot.detail = error;
                // A start may have reserved a request before it failed. No worker
                // exists in this branch, so the ledger can be read at the next poll.
                self.mark_usage_pending();
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
        self.capture_stop_draft(index);
        self.persist_segment(index);
        // A retry may have reserved its paid request before it spawned a worker; the
        // summary becomes visible once no recognition session is active.
        self.mark_usage_pending();
    }

    /// Read the target draft once, when a segment stops, without blocking Stop.
    ///
    /// This read is the only reference a late result is judged against: it is what
    /// the user had written when the segment ended. It runs on its own thread
    /// because a slow or hung editor must never delay the Stop control or the next
    /// capture; a late delivery waits for it off the UI thread with a bounded
    /// timeout and keeps the result pending when the draft cannot be read (#34).
    fn capture_stop_draft(&mut self, index: usize) {
        let Some(segment) = self.segments.get_mut(index) else {
            return;
        };
        if segment.stop_draft_requested {
            return;
        }
        segment.stop_draft_requested = true;
        let sequence = segment.sequence;
        match &segment.target {
            Ok(Some(target)) => {
                let target = target.clone();
                let ledger = self.delivery.clone();
                thread::spawn(move || {
                    let observed = {
                        let mut target = target.lock().unwrap_or_else(|error| error.into_inner());
                        target.content()
                    };
                    ledger.record_baseline(sequence, observed);
                });
            }
            Ok(None) => self.delivery.record_no_target(
                sequence,
                "no supported editable target was selected at capture start",
            ),
            Err(error) => self.delivery.record_no_target(sequence, error),
        }
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
        // A capture that failed before Stop never read its draft. The first retry
        // records it now, once, before the retried result can be delivered: a later
        // read would describe a draft the user may already have edited.
        let sequence = self.segments[index].sequence;
        if !self.delivery.has_baseline_attempt(sequence) {
            self.capture_stop_draft(index);
        }
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
                // A retry reserves its paid request before it spawns a worker; when
                // it fails first, that reservation must still become visible.
                self.mark_usage_pending();
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
                self.delivery.forget(self.segments[index].sequence);
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

    /// Explicitly delete exactly one segment's pending recording and/or pending
    /// result (#36). Nothing else is touched. A segment that is still recovering is
    /// stopped and its session handle dropped first, so a late event can never
    /// recreate the entry or write to a target. A segment whose delivery is already
    /// writing to its target is refused, so a delete can never race a target write.
    fn delete_segment(&mut self, sequence: u64) {
        let Some(index) = self
            .segments
            .iter()
            .position(|segment| segment.sequence == sequence)
        else {
            self.snapshot.detail = format!(
                "No segment {sequence} is recorded, so nothing was deleted. The current result and every other segment are unchanged."
            );
            return;
        };
        if self.segments[index].handle.is_some()
            && !self.segments[index].stopped
            && !self.segments[index].terminal
        {
            self.snapshot.detail = format!(
                "Segment {sequence} is still being captured. Stop it before deleting; nothing was deleted and capture continues."
            );
            return;
        }
        if self.segments[index].finalizing {
            self.snapshot.detail = format!(
                "Segment {sequence}'s complete result is being written to its target right now. Wait for that write to finish before deleting; nothing was deleted."
            );
            return;
        }
        if self.refine_job.is_some() {
            // The refine worker owns the current result's delivery memory and may
            // still write the refined text; deleting under it could bring the text
            // back after the delete.
            self.snapshot.detail = format!(
                "A refine request is still running for the current result. Wait for it to finish before deleting segment {sequence}; nothing was deleted."
            );
            return;
        }
        let segment = &self.segments[index];
        let pending_payload = segment.state.in_flight()
            || segment.state == SegmentState::Failed
            || (segment.state == SegmentState::Result
                && matches!(
                    segment.delivery,
                    Some(Delivery::Fallback) | Some(Delivery::Uncertain)
                ))
            || segment.retained.is_some()
            || !segment.retained_paths.is_empty();
        if !pending_payload {
            self.snapshot.detail = format!(
                "Segment {sequence} has no pending recording or pending result to delete (its result was already delivered), so nothing was deleted. The text already in the target is untouched and the current result is unchanged."
            );
            return;
        }
        let directory = self.segments[index].directory.clone();
        let text_name = SegmentStore::text_name(sequence);
        let primary = index + 1 == self.segments.len();
        let current = sequence == self.current_sequence;
        let had_text = self.segments[index].text.is_some();
        // Stop this segment's later processing first, then close its event channel by
        // dropping the handle. A confirmation or failure already in flight can no
        // longer reach the application, so it cannot resurrect this segment.
        if let Some(mut handle) = self.segments[index].handle.take() {
            handle.stop();
            self.segments[index].terminal = true;
        }
        let mut problem: Option<String> = None;
        if let Err(error) = fs::remove_dir_all(&directory) {
            if error.kind() != std::io::ErrorKind::NotFound {
                problem = Some(format!("its audio could not be deleted ({error})"));
            }
        }
        if let Err(error) = fs::remove_file(self.runtime.join(&text_name)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                let text_problem = format!("its saved text could not be deleted ({error})");
                problem = Some(match problem {
                    Some(existing) => format!("{existing}; {text_problem}"),
                    None => text_problem,
                });
            }
        }
        if let Some(problem) = problem {
            // Keep the segment and its durable record so the user can try again;
            // every other segment is still untouched. Its session is already gone,
            // so it must not keep claiming that it is still processing.
            if self.segments[index].state.in_flight() {
                self.segments[index].state = SegmentState::Failed;
                self.segments[index].session_state = SessionState::Failed;
            }
            self.segments[index].retained_paths = audio_paths(&directory);
            self.segments[index].detail = format!(
                "Segment {sequence} could not be completely deleted: {problem}. Its record and remaining files are kept; every other segment is unchanged."
            );
            self.persist_segment(index);
            self.snapshot.detail = self.segments[index].detail.clone();
            self.refresh_recovery_snapshot();
            return;
        }
        self.segments.remove(index);
        self.segments_dirty = true;
        // A deleted segment can never be processed again, so its background
        // recovery entry must not survive the delete.
        self.recovery.forget(sequence);
        if let Err(error) = self.store.remove(sequence) {
            self.note_record_problem(error);
        }
        // A deleted segment can never be retried, so its stop-time draft reference
        // must not survive to judge a future result (#34).
        self.delivery.forget(sequence);
        if current {
            // The deleted pending result must not come back as the current result
            // after a restart, so the durable copy is cleared as well.
            self.snapshot.text = String::new();
            self.snapshot.delivery = None;
            self.snapshot.refined_text = None;
            self.delivery_memory = None;
            self.snapshot.refine = Refine::Unavailable(
                "The deleted segment's result is no longer the current result. Other pending results stay in the expanded panel."
                    .into(),
            );
            if let Err(error) = cloud::persist_result(&self.runtime.join("current-result.txt"), b"")
            {
                self.note_record_problem(format!(
                    "Could not clear current-result.txt after deleting segment {sequence}: {error}"
                ));
            }
        }
        if primary {
            if self.is_processing() {
                self.snapshot.phase = Phase::Processing;
                self.snapshot.recognition = Some(SessionState::AwaitingFinal);
            } else if !self.is_capturing() {
                self.snapshot.phase = Phase::Ready;
                self.snapshot.recognition = None;
            }
        }
        let what = if had_text {
            "its pending recording and pending result are"
        } else {
            "its pending recording is"
        };
        let mut detail = format!(
            "Segment {sequence} was deleted at your request: {what} gone. Every other segment's audio, result and record is untouched."
        );
        if current {
            detail.push_str(
                " It was the current result, so it is no longer shown as the current result.",
            );
        }
        self.snapshot.detail = detail;
        self.refresh_recovery_snapshot();
    }

    /// Stop a live capture when the pending-audio budget or the free-disk floor is
    /// reached. The received audio stays on disk and the segment still finishes
    /// normally: the application never pretends to keep recording into a full store.
    fn check_storage_pressure(&mut self) {
        let status = self.storage.status(&self.runtime);
        if status.blocking() {
            if let Some(index) = self.capturing_index() {
                let reason = storage::capture_stopped_detail(&status);
                self.stop_capture_for_storage(index, &reason);
            }
        }
        self.apply_storage_status(status);
    }

    fn stop_capture_for_storage(&mut self, index: usize, reason: &str) {
        let primary = index + 1 == self.segments.len();
        {
            let segment = &mut self.segments[index];
            if segment.stopped {
                return;
            }
            segment.stopped = true;
            segment.stop_requested = Some(Instant::now());
            segment.state = SegmentState::Processing;
            segment.storage_note = Some(reason.to_string());
            segment.detail = reason.to_string();
            if let Some(handle) = segment.handle.as_mut() {
                handle.stop();
            }
        }
        if primary {
            self.snapshot.phase = Phase::Processing;
            self.snapshot.recognition = Some(SessionState::AwaitingFinal);
            self.snapshot.detail = reason.to_string();
        }
        // A storage stop is still a stop: a later late result is judged against the
        // draft read now (#34), not against one taken a poll later.
        self.capture_stop_draft(index);
        self.persist_segment(index);
        self.mark_usage_pending();
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
        // Refresh every live segment's own status and progress. This is cheap and
        // display-only; it never consumes an event.
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
        // The newest segment's event is consumed first, so the segment the user just
        // stopped never waits a whole poll tick behind an older backlog. At most one
        // older segment follows per poll, oldest first: a finished backlog is still
        // recovered one at a time in capture order (#33) and cannot occupy the
        // current segment's save or delivery slots.
        let primary = self.segments.len().saturating_sub(1);
        if let Some(event) = self.poll_segment_event(primary) {
            self.apply_session_event(primary, event);
        }
        for index in 0..primary {
            if let Some(event) = self.poll_segment_event(index) {
                self.apply_session_event(index, event);
                break;
            }
        }
        // Apply a finished delivery, newest first. The current segment's confirmed
        // append is never held behind an older background delivery; an older one is
        // applied on the next poll, and the panel order still comes from the segment
        // list, not from the order deliveries happen to finish.
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
        if let Some((_, delivered)) = self.deliveries.pop_last() {
            self.apply_delivery(delivered);
        }
        // Measure storage after statuses, events and deliveries: a segment that just
        // released its audio no longer counts, and a budget that is reached while
        // recording stops the capture instead of pretending it still works.
        self.check_storage_pressure();
        // Cost summaries and per-segment cost are read only once no recognition
        // session is alive, so the ledger's admission lock is never contended.
        self.flush_deferred_usage();
        // Continue the capture-ordered background recovery now that any finished
        // session released the capacity; a new capture always wins it first.
        self.advance_recovery();
        // A degraded delivery record is reported once. It never stops capture or a
        // normal append; it only means a late result cannot be proven safe and stays
        // pending.
        if let Some(problem) = self.delivery.problem() {
            self.note_record_problem(problem);
        }
        self.refresh_segments();
    }

    /// Poll one segment's pending session event, if it still owns a live session.
    fn poll_segment_event(&mut self, index: usize) -> Option<SessionEvent> {
        let segment = self.segments.get_mut(index)?;
        if segment.handle.is_none() || segment.terminal {
            return None;
        }
        segment.handle.as_mut().expect("checked above").poll()
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
        // A storage stop's reason is kept beside the adapters' own progress detail,
        // so a later status refresh cannot silently turn it back into a normal
        // capture message.
        let mut detail = status.detail.clone();
        if let Some(note) = &self.segments[index].storage_note {
            detail.push('\n');
            detail.push_str(note);
        }
        {
            let segment = &mut self.segments[index];
            segment.session_state = status.state;
            segment.detail = detail.clone();
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
        // A session that reports its own stop must still get the one stop-time draft
        // read a late result is judged against (#34); without it the result can only
        // stay pending.
        if self.segments[index].stopped && !self.segments[index].stop_draft_requested {
            self.capture_stop_draft(index);
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
                Some(note) => format!("{detail}\n{note}"),
                None => detail,
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
                    // A recovered segment already keeps its audio on disk and must
                    // never block a new capture with a stale retry entry (#35); the
                    // failure detail still lists the retained paths.
                    if !self.segments[index].recovered {
                        self.segments[index].retained = Some(RetainedCapture { audio });
                    }
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
        // Saved text has its own bound, independent of the pending-audio budget: a
        // full audio store never blocks saving text, and a text over its own bound
        // is a storage fault that keeps the audio as the recovery basis.
        self.storage.check_text_budget(&self.runtime, text)?;
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
        // A complete result saved durably means storage is writable again.
        self.storage.clear_fault();
        Ok(())
    }

    fn finish_recognition(&mut self, index: usize, result: SessionResult) {
        let sequence = self.segments[index].sequence;
        let directory = self.segments[index].directory.clone();
        let description = result.description.clone();
        // A result is late when the adapter had to recover it, and a recovered
        // segment's result is late by definition: its target cannot survive a
        // restart, so it must never be appended automatically (#35).
        let recovered = result.recovered || self.segments[index].recovered;
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
        // The segment's own cost is read later, at a safe point after every
        // recognition session returned: the ledger must not be read mid-capture.
        self.mark_segment_usage_pending(sequence);
        self.mark_usage_pending();
        let is_current = sequence >= self.current_sequence;
        let save_started = Instant::now();
        let saved = self.save_complete_text(sequence, &text, is_current);
        let save_finished_at = Instant::now();
        timing.save_ms = Some(millis_between(save_finished_at, save_started));
        // The application only learns about the confirmed session result when it is
        // polled. Everything between the adapter's own final stages and this durable
        // save is scheduling, and it is measured instead of being folded into the
        // total.
        timing.event_wait_ms = stopped_at.map(|stopped_at| {
            let adapter = timing.finalize_ms.unwrap_or(0)
                + timing.cloud_drain_ms.unwrap_or(0)
                + timing.cloud_final_ms.unwrap_or(0);
            millis_between(save_started, stopped_at).saturating_sub(adapter)
        });
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
            self.segments[index].timing = timing.clone();
            if let Some(audio) = retry_audio {
                self.segments[index].retained = Some(RetainedCapture { audio });
            }
            self.fail_segment(index, error, timing);
            return;
        }
        self.current_sequence = self.current_sequence.max(sequence);
        self.segments[index].timing = timing.clone();
        self.segments[index].finalizing = true;
        let tx = self.completion_tx.clone();
        let ledger = self.delivery.clone();
        let worker_text = text.clone();
        let worker_timing = timing.clone();
        let worker_directory = directory.clone();
        thread::spawn(move || {
            let delivery_started_at = Instant::now();
            let outcome = if recovered {
                deliver_late(target, &worker_text, sequence, ledger.as_ref())
            } else {
                deliver(target, &worker_text, sequence, ledger.as_ref())
            };
            let delivery_finished_at = Instant::now();
            let mut timing = worker_timing;
            timing.delivery_ms = Some(millis_between(delivery_finished_at, delivery_started_at));
            timing.delivery_queue_ms = Some(millis_between(delivery_started_at, save_finished_at));
            // The target read and the write/read-back are part of `delivery_ms`;
            // keeping them separate says where a slow editor spent the time.
            timing.delivery_lock_ms = outcome.lock_ms;
            timing.delivery_read_ms = outcome.read_ms;
            timing.delivery_write_ms = outcome.write_ms;
            timing.delivery_save_ms = outcome.save_ms;
            let _ = tx.send(Delivered {
                sequence,
                text: worker_text,
                description,
                outcome,
                timing,
                directory: worker_directory,
                stopped_at,
                delivery_finished_at,
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
        let applied_at = Instant::now();
        timing.stop_to_result_ms =
            stopped_at.map(|stopped_at| millis_between(applied_at, stopped_at));
        // Silence has no target delivery, but the wait for the session's whole-task
        // confirmation is still scheduling rather than recognition.
        timing.event_wait_ms = stopped_at.map(|stopped_at| {
            let adapter = timing.finalize_ms.unwrap_or(0)
                + timing.cloud_drain_ms.unwrap_or(0)
                + timing.cloud_final_ms.unwrap_or(0);
            millis_between(applied_at, stopped_at).saturating_sub(adapter)
        });
        timing.schedule_ms = timing.event_wait_ms;
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
        self.mark_segment_usage_pending(self.segments[index].sequence);
        self.mark_usage_pending();
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
        let applied_at = Instant::now();
        // Complete the one measurement from the user's Stop to the real target's
        // confirmed read-back, and account for the scheduling gaps around it. This
        // is derived here because only the application thread knows when it applied
        // the delivery; the worker already recorded when the target was confirmed.
        let mut timing = delivered.timing.clone();
        timing.stop_to_result_ms = delivered
            .stopped_at
            .map(|stopped_at| millis_between(applied_at, stopped_at));
        timing.apply_wait_ms = Some(millis_between(applied_at, delivered.delivery_finished_at));
        timing.schedule_ms = match (
            timing.event_wait_ms,
            timing.delivery_queue_ms,
            timing.apply_wait_ms,
        ) {
            (Some(event_wait), Some(queue), Some(apply_wait)) => {
                Some(event_wait + queue + apply_wait)
            }
            _ => None,
        };
        if delivery == Delivery::Appended {
            timing.stop_to_confirmed_ms = delivered
                .stopped_at
                .map(|stopped_at| millis_between(delivered.delivery_finished_at, stopped_at));
        }
        {
            let segment = &mut self.segments[index];
            segment.finalizing = false;
            segment.state = SegmentState::Result;
            segment.session_state = SessionState::Complete;
            segment.text = Some(delivered.text.clone());
            segment.delivery = Some(delivery);
            segment.timing = timing.clone();
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
                    format!("Could not save the current result or clean temporary audio: {error}"),
                ),
            }
        }
        if keep_record {
            self.persist_segment(index);
        } else {
            self.remove_segment_record(index);
        }
        // The request's final usage was observed before the result was reported, so
        // the per-segment and cumulative figures become exact at this safe point.
        self.mark_segment_usage_pending(delivered.sequence);
        self.mark_usage_pending();
        self.refresh_recovery_snapshot();
        self.segments_dirty = true;
    }

    /// Record one failure for exactly one segment. A failure from an older segment
    /// changes nothing the capsule shows; only the newest segment can move the
    /// global phase to `Failure`.
    fn fail_segment(&mut self, index: usize, error: String, timing: Timing) {
        // A local write/save failure is presented as a storage fault, not as a
        // network wait or a playback-device interruption. The recorded fault stays
        // observable in `Snapshot::storage` until a later write succeeds.
        let error = match storage::storage_fault_message(&error) {
            Some(message) => self.storage.note_fault(message),
            None => error,
        };
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
        // A failed segment still has paid requests; its own cost is persisted at the
        // next safe point even though no result was produced.
        self.mark_segment_usage_pending(self.segments[index].sequence);
        self.mark_usage_pending();
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
            // #38: one stop-to-confirmed number and every stage that makes it up.
            "stop_to_confirmed_ms": timing.stop_to_confirmed_ms,
            "cloud_connect_ms": timing.cloud_connect_ms,
            "first_task_started_ms": timing.first_task_started_ms,
            "cloud_drain_ms": timing.cloud_drain_ms,
            "cloud_final_ms": timing.cloud_final_ms,
            "event_wait_ms": timing.event_wait_ms,
            "delivery_queue_ms": timing.delivery_queue_ms,
            "apply_wait_ms": timing.apply_wait_ms,
            "schedule_ms": timing.schedule_ms,
            "delivery_lock_ms": timing.delivery_lock_ms,
            "delivery_read_ms": timing.delivery_read_ms,
            "delivery_write_ms": timing.delivery_write_ms,
            "delivery_save_ms": timing.delivery_save_ms,
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
        Delivery::Uncertain => "uncertain",
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
pub(crate) fn audio_paths(directory: &Path) -> Vec<PathBuf> {
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

//! Explicit cloud trial. The normal application constructor remains local.
mod budget;
mod capture;
mod pcm;
use capture::CaptureControl;
#[cfg(windows)]
mod credentials;
#[cfg(windows)]
mod free_only;
#[cfg(windows)]
mod transport;
use crate::{
    AppResult, CapturedAudio, PlaybackAudio, Preparation, Timing, UsageSummary, elapsed_ms,
    millis_between,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SegmentUsage, SessionEvent,
        SessionFailure, SessionRequest, SessionResult, SessionState, SessionStatus, SessionUpdate,
    },
};
pub(crate) use budget::atomic_write as persist_result;
pub use budget::{Attempt, LedgerSnapshot, TrialLedger, Usage};
#[cfg(windows)]
pub use credentials::{CREDENTIAL_TARGET, LocalCredentials, local_settings};
#[cfg(windows)]
pub use free_only::FreeOnlyStatusSource;
pub use pcm::{PcmSpoolWriter, PcmStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
#[cfg(windows)]
pub use transport::NativeMessageTransport;

pub const ALI_MODEL: &str = "qwen-audio-3.1-asr-flash-message";
pub const ALI_REGION: &str = "cn-beijing";

/// A transient cloud failure is retried with a bounded, growing delay. The
/// capture owns the received audio meanwhile, so waiting never discards it.
///
/// The attempt count is bounded per capture. The time window is bounded per
/// **outage**: it starts at the first transient failure that is retried, not at
/// capture start, so a connection that drops after minutes of healthy streaming
/// still gets a real bounded retry window (a capture-start anchor would make every
/// late drop fail with zero retries). A later successful connection ends the
/// outage, so the next drop gets its own window.
const MAX_CONNECTION_ATTEMPTS: u32 = 8;
const RETRY_WINDOW: Duration = Duration::from_secs(20);
const RETRY_BASE: Duration = Duration::from_millis(100);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
/// Sleep in slices so a stop or a device failure is noticed promptly instead of
/// after a whole backoff period.
const RETRY_POLL: Duration = Duration::from_millis(25);

fn retry_delay(attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(5);
    (RETRY_BASE * factor).min(RETRY_MAX_DELAY)
}

/// Transient failures keep the capture alive and are retried. Credential,
/// balance, model, configuration and protocol errors are not retried.
fn retryable(error: &CloudError) -> bool {
    matches!(
        error,
        CloudError::Network
            | CloudError::Timeout
            | CloudError::RateLimited
            | CloudError::RateOrBudget
            | CloudError::Provider
    )
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Link {
    #[default]
    Connecting,
    Streaming,
    Waiting,
    /// Recognition cannot continue, but the capture is still saving audio.
    Blocked,
}

/// The connection state the worker publishes so `status` can tell "waiting for
/// the network" apart from a failed capture or a finished task.
#[derive(Default)]
struct LinkState {
    state: Mutex<Link>,
    problem: Mutex<Option<String>>,
}

impl LinkState {
    fn set(&self, state: Link) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = state;
    }
    fn connecting(&self) {
        self.set(Link::Connecting);
    }
    fn streaming(&self) {
        self.set(Link::Streaming);
    }
    fn waiting(&self) {
        self.set(Link::Waiting);
    }
    fn is_waiting(&self) -> bool {
        matches!(
            *self.state.lock().unwrap_or_else(|error| error.into_inner()),
            Link::Waiting
        )
    }
    /// Publish a failure that retrying cannot fix. Capture keeps saving audio and
    /// the user sees the actionable reason instead of a bare "failed".
    fn blocked(&self, problem: &str) {
        *self
            .problem
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(problem.into());
        self.set(Link::Blocked);
    }
    fn problem(&self) -> Option<String> {
        self.problem
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AliSettings {
    pub workspace_id: String,
    pub model: String,
    pub region: String,
}
impl AliSettings {
    pub fn beijing(workspace_id: String) -> Self {
        Self {
            workspace_id,
            model: ALI_MODEL.into(),
            region: ALI_REGION.into(),
        }
    }
    pub fn description(&self) -> String {
        format!(
            "Cloud trial: Alibaba Model Studio; model={}; region={}; shared single-vendor trial cap=CNY 50",
            if self.model == ALI_MODEL {
                ALI_MODEL
            } else {
                "unsupported model (check local settings)"
            },
            if self.region == ALI_REGION {
                ALI_REGION
            } else {
                "unsupported region (check local settings)"
            }
        )
    }
    fn validate(&self) -> Result<(), CloudError> {
        if self.model != ALI_MODEL
            || self.region != ALI_REGION
            || self.workspace_id.is_empty()
            || self.workspace_id.len() > 64
            || !self
                .workspace_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(CloudError::Configuration);
        }
        Ok(())
    }
}
/// Deliberately has no Debug/Serialize implementation. No result or status owns it.
pub struct Secret(String);
impl Secret {
    pub fn new(value: String) -> Result<Self, CloudError> {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(CloudError::MissingKey);
        }
        if value.len() > 4096 || !value.bytes().all(|b| (33..=126).contains(&b)) {
            return Err(CloudError::Credential);
        }
        Ok(Self(value))
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        // Do not retain an additional cleartext key after the process has closed
        // its connection. Volatile writes keep this wipe from being optimized out.
        unsafe {
            for byte in self.0.as_bytes_mut() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}
pub trait CredentialSource: Send + Sync {
    fn load(&self) -> Result<Secret, CloudError>;
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedBounds {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_pcm_bytes: u64,
    pub evidence: String,
    pub verified_on: String,
}
/// Supplied only by an adapter which has checked active, model-scoped free-only
/// protection for the credential used by this task. Never loaded from trial JSON.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FreeOnlyPolicy {
    pub model: String,
    pub region: String,
    pub evidence: String,
    pub verified_on: String,
}
#[derive(Clone, Debug)]
pub enum TrialCostPolicy {
    TokenBounds(VerifiedBounds),
    FreeOnly(FreeOnlyPolicy),
}
impl TrialCostPolicy {
    fn max_pcm_bytes(&self) -> u64 {
        match self {
            Self::TokenBounds(bounds) => bounds.max_pcm_bytes,
            // This is the existing local retention allowance, not a Token or
            // billing bound. The free-only guarantee supplies the cash limit.
            Self::FreeOnly(_) => 1024 * 1024 * 1024,
        }
    }
    fn max_duration(&self) -> Option<Duration> {
        match self {
            Self::TokenBounds(_) => None,
            // 1 GiB / (16 kHz * 16-bit mono) plus bounded finalization. This
            // operational deadline also covers idle or slow upload sessions.
            Self::FreeOnly(_) => Some(Duration::from_secs(1024 * 1024 * 1024 / 32_000 + 60)),
        }
    }
}
/// Monotonic time is an external dependency; controlled application workflows
/// can advance it without waiting through a real multi-hour capture.
pub trait TrialClock: Send + Sync {
    fn now(&self) -> Instant;
}
struct SystemClock;
impl TrialClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
/// Only a maintained, independently verified contract may supply bounds. This is
/// not loaded from user JSON/environment. Native Message currently returns None.
pub trait MessageTransport: Send + Sync {
    /// Allows local PCM retention while the worker checks the live policy. It
    /// never authorizes connect; `cost_policy` must still succeed before that.
    fn requires_live_cost_check(&self) -> bool {
        false
    }
    fn verified_bounds(&self, _settings: &AliSettings) -> Option<VerifiedBounds> {
        None
    }
    fn cost_policy(
        &self,
        settings: &AliSettings,
        _key: &Secret,
    ) -> Result<TrialCostPolicy, CloudError> {
        self.verified_bounds(settings)
            .map(TrialCostPolicy::TokenBounds)
            .ok_or(CloudError::BudgetUnknown)
    }
    fn connect(
        &self,
        settings: &AliSettings,
        key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError>;
}
pub trait MessageSocket: Send {
    fn send_text(&mut self, text: &str) -> Result<(), CloudError>;
    fn send_audio(&mut self, pcm: &[u8]) -> Result<(), CloudError>;
    /// Non-blocking; implementations own bounded receive storage.
    fn poll(&mut self) -> Result<Option<String>, CloudError>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudError {
    MissingKey,
    Credential,
    Configuration,
    BudgetUnknown,
    ProtectionUnavailable,
    BudgetExhausted,
    Ledger,
    LedgerBusy,
    UsageMismatch,
    Network,
    Timeout,
    Protocol,
    Unauthorized,
    ModelUnavailable,
    BillingUnavailable,
    RateLimited,
    RateOrBudget,
    Provider,
}
impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingKey => "No Alibaba cloud API key is configured. Use scripts/configure-cloud-trial.ps1 locally, or inject LISTEN_ALI_API_KEY for this process; no cloud request was sent",
            Self::Credential => "The local cloud credential could not be read or has an invalid format; configure it again locally",
            Self::Configuration => "Cloud trial requires a valid Alibaba workspace, cn-beijing region and qwen-audio-3.1-asr-flash-message model; check the local configuration",
            Self::BudgetUnknown => "The maximum charge for this cloud trial cannot yet be confirmed. No audio was sent. Use local recognition for now; see the local cloud-trial configuration guide for the remaining requirements",
            Self::ProtectionUnavailable => "The Alibaba trial's free-only protection could not be verified for the selected default workspace and model. No audio was sent. Check the existing CLI login, quota protection and expiry locally; received audio is retained",
            Self::BudgetExhausted => "The shared CNY 50 trial budget has insufficient uncommitted funds; no new cloud task was sent",
            Self::Ledger => "The shared trial cost ledger cannot be read or safely persisted. No new paid request is allowed; preserve the ledger and check local storage",
            Self::LedgerBusy => "The shared trial cost ledger is in use by another process; no new cloud request was allowed",
            Self::UsageMismatch => "Provider usage exceeded or contradicted the verified reservation. New paid trials are blocked pending billing review",
            Self::Network => "The Alibaba cloud connection failed or closed without whole-task confirmation; received audio is retained and no local fallback was substituted",
            Self::Timeout => "The Alibaba cloud connection or task did not respond within the bounded wait; received audio is retained and unconfirmed text was not delivered",
            Self::Protocol => "The Alibaba response did not satisfy the whole-task protocol; received audio is retained and unconfirmed text was not delivered",
            Self::Unauthorized => "Alibaba rejected authentication or workspace access (401/403). Check the Beijing API key, account and workspace permissions locally",
            Self::ModelUnavailable => "The selected Alibaba ASR model is unavailable or not enabled for this account/workspace; no alternate model was selected",
            Self::BillingUnavailable => "Alibaba rejected the task because balance, quota or a billing budget is unavailable. Check the account locally; no automatic paid retry was started",
            Self::RateLimited => "Alibaba limited the request rate. This trial does not automatically retry; audio and the pending reservation are retained",
            Self::RateOrBudget => "Alibaba rejected the handshake (429): a rate limit or a configured billing budget may have stopped the request. Check account budgets and rate limits locally; this trial does not automatically retry",
            Self::Provider => "Alibaba rejected this recognition task; check account/model access and billing locally. The remote error body was omitted to protect credentials and audio content",
        })
    }
}
impl std::error::Error for CloudError {}

pub struct CloudSessions {
    audio: Arc<dyn PlaybackAudio>,
    transport: Arc<dyn MessageTransport>,
    key: Result<Arc<Secret>, CloudError>,
    settings: AliSettings,
    ledger: TrialLedger,
    clock: Arc<dyn TrialClock>,
}
impl CloudSessions {
    #[cfg(windows)]
    pub fn from_local(audio: Arc<dyn PlaybackAudio>) -> AppResult<Self> {
        Ok(Self::new(
            audio,
            Arc::new(NativeMessageTransport::default()),
            Arc::new(LocalCredentials),
            local_settings().map_err(|e| e.to_string())?,
            TrialLedger::shared_local().map_err(|e| e.to_string())?,
        ))
    }
    pub fn new(
        audio: Arc<dyn PlaybackAudio>,
        transport: Arc<dyn MessageTransport>,
        credentials: Arc<dyn CredentialSource>,
        settings: AliSettings,
        ledger: TrialLedger,
    ) -> Self {
        Self {
            audio,
            transport,
            key: credentials.load().map(Arc::new),
            settings,
            ledger,
            clock: Arc::new(SystemClock),
        }
    }
    pub fn with_clock(mut self, clock: Arc<dyn TrialClock>) -> Self {
        self.clock = clock;
        self
    }
    fn available(&self) -> Result<Option<TrialCostPolicy>, CloudError> {
        self.settings.validate()?;
        let key = self.key.as_ref().map_err(Clone::clone)?;
        if self.transport.requires_live_cost_check() {
            Ok(None)
        } else {
            self.transport.cost_policy(&self.settings, key).map(Some)
        }
    }
}
impl RecognitionSessions for CloudSessions {
    fn requires_valid_glossary(&self) -> bool {
        true
    }
    /// The durable trial ledger is the one record of what this tool spent, so the
    /// summary is read from it rather than accumulated in memory. An unreadable
    /// ledger is reported as unknown, never as zero.
    fn usage(&mut self) -> Option<UsageSummary> {
        Some(self.ledger.usage())
    }
    fn preparation(&mut self) -> RecognitionReadiness {
        let error = self.available().err().map(|e| e.to_string());
        RecognitionReadiness {
            state: if error.is_some() {
                Preparation::Failed
            } else {
                Preparation::Ready
            },
            backend: Some(self.settings.description()),
            elapsed_ms: None,
            error,
        }
    }
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        let policy = self.available().map_err(|e| e.to_string())?;
        let vocabulary = request_vocabulary(&request)?;
        let segment = segment_name(&request.directory)?;
        // The first request is reserved and named before capture starts, so an
        // unavailable budget refuses the capture instead of failing later.
        let initial_task = match &policy {
            Some(policy) => {
                let task = task_id()?;
                self.ledger
                    .reserve(&task, &segment, &self.settings, policy.clone())
                    .map_err(|e| e.to_string())?;
                Some(task)
            }
            None => None,
        };
        fs::create_dir(&request.directory).map_err(|e| e.to_string())?;
        let mut recording = self.audio.start(&request.directory)?;
        let stream = recording.take_pcm_stream().ok_or("Cloud trial requires continuous retained PCM; local overlapping windows are not a cloud stream")?;
        let endpoint = recording.endpoint();
        let progress = stream.progress.clone();
        let capture = CaptureControl::new(recording);
        let capture_worker = capture.clone();
        let stable = Arc::new(AtomicBool::new(false));
        let stable_signal = stable.clone();
        let link = Arc::new(LinkState::default());
        let worker_link = link.clone();
        let transport = self.transport.clone();
        let settings = self.settings.clone();
        let key = self.key.as_ref().map_err(|e| e.to_string())?.clone();
        let ledger = self.ledger.clone();
        let clock = self.clock.clone();
        let id = request.id;
        let (tx, result) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let update = match run(
                capture_worker,
                stream,
                stable_signal,
                worker_link,
                transport,
                &settings,
                &key,
                initial_task,
                &segment,
                policy,
                &ledger,
                clock,
                vocabulary,
                false,
            ) {
                Ok(result) => SessionUpdate::Finished(result),
                Err(failure) => SessionUpdate::Failed(failure),
            };
            let _ = tx.send(SessionEvent { id, update });
        });
        Ok(Box::new(CloudSession {
            id,
            capture,
            stable,
            link,
            result,
            state: SessionState::Capturing,
            endpoint,
            progress,
            description: self.settings.description(),
        }))
    }
    /// What the shared trial ledger recorded for exactly this segment, including
    /// every automatic rebuild and an explicit retry. The segment identity is the
    /// capture directory name, so another segment's cost can never be mixed in.
    ///
    /// This reads the ledger, so the application must call it only when no
    /// recognition session is in flight (#37): the ledger's exclusive lock is also
    /// the paid-request admission lock, and a UI read must never make a worker's
    /// reservation fail. A locked or unreadable ledger reports `None`, never zero.
    fn segment_usage(&self, directory: &std::path::Path) -> Option<SegmentUsage> {
        let segment = segment_name(directory).ok()?;
        let snapshot = self.ledger.snapshot().ok()?;
        let attempts: Vec<&Attempt> = snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.segment_id == segment)
            .collect();
        let first = attempts.first()?;
        let mut estimated = Some(0u64);
        let mut unknown = false;
        for attempt in &attempts {
            match attempt.estimated_nanoyuan.or(attempt.billed_nanoyuan) {
                Some(value) => {
                    estimated = estimated.map(|total| total.saturating_add(value));
                }
                None => {
                    // An unfinished request is not a zero-cost request.
                    unknown = true;
                    estimated = None;
                }
            }
        }
        Some(SegmentUsage {
            provider: first.provider.clone(),
            model: first.model.clone(),
            region: first.region.clone(),
            requests: attempts.len().min(u32::MAX as usize) as u32,
            estimated_nanoyuan: estimated,
            unknown,
        })
    }
    /// Reprocess the exact retained recording on the explicit user request. This
    /// is a new, separately reserved remote task tied to the same segment; it never
    /// uses the local engine and never claims the retransmission is free. The
    /// recovered result is delivered conservatively (the panel), because it is late.
    fn retry(
        &mut self,
        request: SessionRequest,
        audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        let policy = self.available().map_err(|e| e.to_string())?;
        let vocabulary = request_vocabulary(&request)?;
        let segment = segment_name(&request.directory)?;
        let initial_task = match &policy {
            Some(policy) => {
                let task = task_id()?;
                self.ledger
                    .reserve(&task, &segment, &self.settings, policy.clone())
                    .map_err(|e| e.to_string())?;
                Some(task)
            }
            None => None,
        };
        let stream = PcmStream::resume(&audio.path)?;
        let progress = stream.progress.clone();
        let capture = CaptureControl::retained(audio);
        let capture_worker = capture.clone();
        let stable = Arc::new(AtomicBool::new(false));
        let stable_signal = stable.clone();
        let link = Arc::new(LinkState::default());
        let worker_link = link.clone();
        let transport = self.transport.clone();
        let settings = self.settings.clone();
        let key = self.key.as_ref().map_err(|e| e.to_string())?.clone();
        let ledger = self.ledger.clone();
        let clock = self.clock.clone();
        let id = request.id;
        let (tx, result) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let update = match run(
                capture_worker,
                stream,
                stable_signal,
                worker_link,
                transport,
                &settings,
                &key,
                initial_task,
                &segment,
                policy,
                &ledger,
                clock,
                vocabulary,
                true,
            ) {
                Ok(result) => SessionUpdate::Finished(result),
                Err(failure) => SessionUpdate::Failed(failure),
            };
            let _ = tx.send(SessionEvent { id, update });
        });
        Ok(Box::new(CloudSession {
            id,
            capture,
            stable,
            link,
            result,
            state: SessionState::AwaitingFinal,
            endpoint: None,
            progress,
            description: self.settings.description(),
        }))
    }
}
struct CloudSession {
    id: crate::recognition::SessionId,
    capture: Arc<CaptureControl>,
    stable: Arc<AtomicBool>,
    link: Arc<LinkState>,
    result: mpsc::Receiver<SessionEvent>,
    state: SessionState,
    endpoint: Option<crate::CaptureEndpoint>,
    progress: Arc<pcm::PcmProgress>,
    description: String,
}
impl RecognitionSession for CloudSession {
    fn stop(&mut self) {
        self.capture.stop();
        self.state = SessionState::AwaitingFinal;
    }
    fn status(&self) -> SessionStatus {
        let problem = self.link.problem();
        let waiting = !matches!(self.state, SessionState::Complete | SessionState::Failed)
            && self.link.is_waiting();
        SessionStatus {
            state: if waiting {
                SessionState::WaitingForNetwork
            } else {
                self.state
            },
            progress: self.progress.snapshot(),
            endpoint: self.endpoint.clone(),
            detail: format!(
                "{}\n{}",
                self.description,
                if let Some(problem) = problem {
                    if self.capture.is_stopped() {
                        format!(
                            "Cloud recognition cannot continue: {problem}. The complete recording is saved on this machine; no unconfirmed text was delivered."
                        )
                    } else {
                        format!(
                            "Cloud recognition cannot continue: {problem}. Playback is still being captured and saved on this machine so the recording can be retried after the problem is fixed."
                        )
                    }
                } else if waiting {
                    "Waiting for the network. The received playback is saved on this machine; Listen retries the cloud connection automatically and never delivers an unconfirmed sentence.".into()
                } else if self.capture.is_stopped() {
                    "Waiting for whole-task cloud confirmation; unconfirmed sentences are not delivered.".into()
                } else {
                    "Capturing continuous playback to local PCM before upload. Only this capture and its glossary are supplied to Alibaba; no messages are sent automatically.".into()
                }
            ),
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        // The worker owns device-failure detection. Stopping the capture here
        // would race it and turn a real capture failure into a generic network
        // retry, losing the reason the audio source disappeared.
        match self.result.try_recv() {
            Ok(event) => {
                self.state = if matches!(event.update, SessionUpdate::Finished(_)) {
                    SessionState::Complete
                } else {
                    SessionState::Failed
                };
                Some(event)
            }
            Err(mpsc::TryRecvError::Disconnected)
                if !matches!(self.state, SessionState::Complete | SessionState::Failed) =>
            {
                self.state = SessionState::Failed;
                Some(SessionEvent { id: self.id, update: SessionUpdate::Failed(SessionFailure { error: "Cloud session ended before whole-task confirmation; received PCM and its reservation are retained".into(), retry_audio: None, retry_scope: None, timing: Timing::default() }) })
            }
            _ if self.stable.swap(false, Ordering::AcqRel) => Some(SessionEvent {
                id: self.id,
                update: SessionUpdate::Stable(String::new()),
            }),
            _ => None,
        }
    }
}
impl Drop for CloudSession {
    fn drop(&mut self) {
        self.capture.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    capture: Arc<CaptureControl>,
    mut stream: PcmStream,
    stable: Arc<AtomicBool>,
    link: Arc<LinkState>,
    transport: Arc<dyn MessageTransport>,
    settings: &AliSettings,
    key: &Secret,
    initial_task: Option<String>,
    segment: &str,
    policy: Option<TrialCostPolicy>,
    ledger: &TrialLedger,
    clock: Arc<dyn TrialClock>,
    vocabulary: serde_json::Map<String, Value>,
    // True when this session replays already retained audio on a later request,
    // so its result is late and must not be written to the target automatically.
    late: bool,
) -> Result<SessionResult, SessionFailure> {
    let started_at = Instant::now();
    let mut description = String::new();
    let mut recovered = late;
    let timing = Timing {
        recognition_ms: Some(elapsed_ms(started_at)),
        ..Timing::default()
    };
    let policy = match policy {
        Some(policy) => policy,
        None => match transport.cost_policy(settings, key) {
            Ok(policy) => policy,
            Err(error) => {
                return Err(blocked_failure(&capture, &link, error.to_string(), timing));
            }
        },
    };
    let session_started = clock.now();
    // One remote task per attempt. The first task was already reserved by
    // `start`; a rebuild reserves its own request so retry cost stays visible
    // and bounded instead of silently reusing a stale charge. Received audio is
    // replayed from the retained PCM, never from an in-memory copy.
    let mut next_task = initial_task;
    let mut attempts = 0u32;
    // Starts at the first transient failure that is retried, so the retry window
    // belongs to the outage rather than to the capture's age. A successful
    // connection clears it: the next drop gets a fresh window.
    let mut retry_window_started: Option<Instant> = None;
    // The last successful transport connect (#38). The first connection happens
    // during capture; the final one is the attempt whose confirmation the stop
    // path waits for.
    #[allow(unused_assignments)]
    let mut connect_ms = None;
    loop {
        attempts += 1;
        if let Some(error) = capture.failure() {
            return Err(device_failure(
                &capture,
                capture_failure(&error),
                timing.clone(),
            ));
        }
        let task = match next_task.take() {
            Some(task) => task,
            None => {
                let task = match task_id() {
                    Ok(task) => task,
                    Err(error) => {
                        return Err(blocked_failure(&capture, &link, error, timing.clone()));
                    }
                };
                if let Err(error) = ledger.reserve(&task, segment, settings, policy.clone()) {
                    return Err(blocked_failure(
                        &capture,
                        &link,
                        error.to_string(),
                        timing.clone(),
                    ));
                }
                task
            }
        };
        link.connecting();
        let connect_started = Instant::now();
        let mut socket = match transport.connect(settings, key) {
            Ok(socket) => {
                // The previous outage, if any, is over. A later transient failure
                // must open a new bounded retry window of its own.
                retry_window_started = None;
                connect_ms = Some(elapsed_ms(connect_started));
                socket
            }
            Err(error) if retryable(&error) => {
                // The session is not failed: the capture keeps saving audio
                // while the connection is retried with a bounded backoff.
                recovered = true;
                link.waiting();
                if retry_budget_exhausted(attempts, &mut retry_window_started, clock.as_ref()) {
                    return Err(blocked_failure(
                        &capture,
                        &link,
                        retry_exhausted(attempts, &error),
                        timing.clone(),
                    ));
                }
                let window_started = retry_window_started.unwrap_or_else(|| clock.now());
                if let Err(message) =
                    wait_before_retry(&capture, clock.as_ref(), window_started, attempts)
                {
                    return Err(device_failure(&capture, message, timing.clone()));
                }
                continue;
            }
            Err(error) => {
                return Err(blocked_failure(
                    &capture,
                    &link,
                    error.to_string(),
                    timing.clone(),
                ));
            }
        };
        link.streaming();
        match stream_attempt(
            &capture,
            &mut stream,
            socket.as_mut(),
            &stable,
            settings,
            &task,
            &policy,
            ledger,
            clock.as_ref(),
            session_started,
            &vocabulary,
            &mut description,
            started_at,
            connect_ms,
        ) {
            Ok(mut result) => {
                capture.stop();
                result.recovered = recovered;
                return Ok(result);
            }
            Err(AttemptError::Transient(error)) => {
                recovered = true;
                link.waiting();
                if retry_budget_exhausted(attempts, &mut retry_window_started, clock.as_ref()) {
                    return Err(blocked_failure(
                        &capture,
                        &link,
                        retry_exhausted(attempts, &error),
                        timing.clone(),
                    ));
                }
                let window_started = retry_window_started.unwrap_or_else(|| clock.now());
                if let Err(message) =
                    wait_before_retry(&capture, clock.as_ref(), window_started, attempts)
                {
                    return Err(device_failure(&capture, message, timing.clone()));
                }
                // The vendor has no resumable session here, so the whole task is
                // rebuilt from the retained continuous audio. Never assume a resume
                // offset: replaying from the start cannot lose a byte, and the
                // fresh task's text starts empty, so nothing is doubled.
                if let Err(error) = stream.rewind() {
                    return Err(blocked_failure(
                        &capture,
                        &link,
                        format!("Cannot re-read the retained audio for recovery: {error}"),
                        timing.clone(),
                    ));
                }
            }
            Err(AttemptError::Blocked(message)) => {
                return Err(blocked_failure(&capture, &link, message, timing.clone()));
            }
            Err(AttemptError::Device(message)) => {
                return Err(device_failure(&capture, message, timing.clone()));
            }
        }
    }
}

/// A failure that retrying cannot fix keeps the capture running and saving audio.
/// The actionable reason is published immediately; the session only reports a
/// failure once the user stops the capture (or the device itself fails), and then
/// keeps the recording available for an explicit retry.
fn blocked_failure(
    capture: &Arc<CaptureControl>,
    link: &LinkState,
    message: String,
    timing: Timing,
) -> SessionFailure {
    link.blocked(&message);
    let mut error = message;
    let retry_audio = loop {
        if let Some(device) = capture.failure() {
            capture.stop();
            error.push_str(&format!(
                ". The playback device also failed: {device}. Waiting for the network cannot restore this capture"
            ));
            break capture.wait().ok();
        }
        if capture.is_stopped() {
            break capture.wait().ok();
        }
        thread::sleep(Duration::from_millis(20));
    };
    SessionFailure {
        error,
        retry_audio,
        retry_scope: None,
        timing,
    }
}

fn device_failure(
    capture: &Arc<CaptureControl>,
    message: String,
    timing: Timing,
) -> SessionFailure {
    capture.stop();
    let retry_audio = capture.wait().ok();
    SessionFailure {
        error: message,
        retry_audio,
        retry_scope: Some(
            "The playback device failed, so this capture cannot continue; waiting for the network cannot restore it."
                .into(),
        ),
        timing,
    }
}

/// How one remote task ended before the whole result was confirmed.
enum AttemptError {
    /// A transient connection or service failure; the segment can be rebuilt.
    Transient(CloudError),
    /// A failure that retrying cannot fix (credentials, balance, model, protocol,
    /// budget, storage); the message is user-readable.
    Blocked(String),
    /// The playback device itself failed; the capture is over.
    Device(String),
}

fn retry_exhausted(attempts: u32, error: &CloudError) -> String {
    format!(
        "The cloud connection could not be restored automatically after {attempts} attempt(s): {error}. Received audio is retained on this machine; no unconfirmed text was delivered"
    )
}

/// Whether a transient failure may still be retried. `window_started` is opened on
/// the first retried failure of an outage; later failures in the same outage are
/// measured against that anchor. The attempt count stays bounded per capture, so an
/// endless failure still stops even when every attempt fails quickly.
fn retry_budget_exhausted(
    attempts: u32,
    window_started: &mut Option<Instant>,
    clock: &dyn TrialClock,
) -> bool {
    if attempts >= MAX_CONNECTION_ATTEMPTS {
        return true;
    }
    match *window_started {
        Some(started) => clock.now().saturating_duration_since(started) >= RETRY_WINDOW,
        None => {
            // First retried failure of this outage: it owns a fresh window.
            *window_started = Some(clock.now());
            false
        }
    }
}

/// Sleep in bounded slices so a stop or a device failure is noticed promptly.
fn wait_before_retry(
    capture: &Arc<CaptureControl>,
    clock: &dyn TrialClock,
    window_started: Instant,
    attempt: u32,
) -> Result<(), String> {
    let delay = retry_delay(attempt);
    let mut waited = Duration::ZERO;
    while waited < delay {
        if let Some(error) = capture.failure() {
            capture.stop();
            return Err(capture_failure(&error));
        }
        let slice = RETRY_POLL.min(delay - waited);
        thread::sleep(slice);
        waited += slice;
        if clock.now().saturating_duration_since(window_started) >= RETRY_WINDOW {
            break;
        }
    }
    Ok(())
}

fn classify(error: CloudError) -> AttemptError {
    if retryable(&error) {
        AttemptError::Transient(error)
    } else {
        AttemptError::Blocked(error.to_string())
    }
}

fn run_task_text(
    settings: &AliSettings,
    task: &str,
    vocabulary: &serde_json::Map<String, Value>,
) -> String {
    json!({
        "header": {"action":"run-task","task_id":task,"streaming":"duplex"},
        "payload": {
            "task_group":"audio","task":"asr","function":"recognition","model":settings.model,
            "parameters": {
                "format":"pcm","sample_rate":16000,
                "disfluency_removal_enabled":false,"intermediate_result_enabled":false,
                "keep_dialect":true,"heartbeat":true,"vocabulary":vocabulary
            },
            "input": {}
        }
    })
    .to_string()
}

fn finish_task_text(task: &str) -> String {
    json!({
        "header": {"action":"finish-task","task_id":task,"streaming":"duplex"},
        "payload": {"input": {}}
    })
    .to_string()
}

#[allow(clippy::too_many_arguments)]
fn stream_attempt(
    capture: &Arc<CaptureControl>,
    stream: &mut PcmStream,
    socket: &mut dyn MessageSocket,
    stable: &Arc<AtomicBool>,
    settings: &AliSettings,
    task: &str,
    policy: &TrialCostPolicy,
    ledger: &TrialLedger,
    clock: &dyn TrialClock,
    session_started: Instant,
    vocabulary: &serde_json::Map<String, Value>,
    description: &mut String,
    started_at: Instant,
    connect_ms: Option<u64>,
) -> Result<SessionResult, AttemptError> {
    let max_pcm_bytes = policy.max_pcm_bytes();
    let within_time = || {
        if policy
            .max_duration()
            .is_some_and(|limit| clock.now().saturating_duration_since(session_started) >= limit)
        {
            Err(AttemptError::Blocked(
                "Cloud trial time allowance reached. Received audio is retained; unconfirmed text was not delivered".into(),
            ))
        } else {
            Ok(())
        }
    };
    within_time()?;
    socket
        .send_text(&run_task_text(settings, task, vocabulary))
        .map_err(classify)?;
    let connecting_at = Instant::now();
    let mut ready = false;
    let mut finish_sent = None;
    let mut sent = 0u64;
    // #38 stages: the first whole-task handshake and the moment the capture itself
    // is fully finalised (device joined, spool flushed). The drain and the final
    // confirmation are timed from these.
    let mut first_task_started_ms = None;
    let mut outcome_at: Option<Instant> = None;
    // How many 100 ms PCM chunks are pushed per loop pass. A recovery replay may
    // have a whole recording queued; pushing one chunk per 2 ms sleep would make
    // the old segment's drain (and therefore its stop-to-result) scale with the
    // recording length instead of with the network.
    const MAX_CHUNKS_PER_PASS: usize = 64;
    // Text belongs to this attempt only: a rebuilt task regenerates all of it
    // from the replayed audio, so no sentence from an earlier connection is kept.
    let mut sentences = BTreeMap::<u64, String>::new();
    let mut pending = BTreeSet::new();
    loop {
        within_time()?;
        // Whether this pass moved audio or received an event. The loop only sleeps
        // when it made no progress, so the final drain of a stopped capture (and a
        // recovery replay) is limited by the socket, not by a fixed 2 ms per chunk.
        let mut progressed = false;
        if let Some(error) = capture.failure() {
            capture.stop();
            return Err(AttemptError::Device(capture_failure(&error)));
        }
        let captured = capture.outcome();
        if captured.is_some() && outcome_at.is_none() {
            outcome_at = Some(Instant::now());
        }
        if let Some(audio) = &captured {
            match audio {
                Ok(audio) => *description = audio.description.clone(),
                Err(error) => {
                    return Err(AttemptError::Device(format!(
                        "The playback capture could not be finished: {error}. Received audio is retained on this machine"
                    )));
                }
            }
        }
        for _ in 0..8 {
            let Some(raw) = socket.poll().map_err(classify)? else {
                break;
            };
            progressed = true;
            if raw.len() > 1024 * 1024 {
                return Err(AttemptError::Blocked(CloudError::Protocol.to_string()));
            }
            let event: Value = serde_json::from_str(&raw)
                .map_err(|_| AttemptError::Blocked(CloudError::Protocol.to_string()))?;
            if event["header"]["task_id"].as_str() != Some(task) {
                continue;
            }
            let kind = event["header"]["event"]
                .as_str()
                .ok_or_else(|| AttemptError::Blocked(CloudError::Protocol.to_string()))?;
            match kind {
                "task-started" => {
                    ready = true;
                    if first_task_started_ms.is_none() {
                        first_task_started_ms = Some(elapsed_ms(started_at));
                    }
                }
                "result-generated" if ready => {
                    ledger
                        .observe(task, reported_usage(&event["payload"]["usage"]), false)
                        .map_err(|error| AttemptError::Blocked(error.to_string()))?;
                    let sentence = &event["payload"]["output"]["sentence"];
                    if sentence["heartbeat"] == true {
                        continue;
                    }
                    if sentence["sentence_id"].as_u64().is_none()
                        || sentence["text"].as_str().is_none()
                        || sentence["sentence_end"].as_bool().is_none()
                    {
                        return Err(AttemptError::Blocked(CloudError::Protocol.to_string()));
                    }
                    if sentence["sentence_end"] == true {
                        let id = sentence["sentence_id"].as_u64().ok_or_else(|| {
                            AttemptError::Blocked(CloudError::Protocol.to_string())
                        })?;
                        let text = sentence["text"].as_str().ok_or_else(|| {
                            AttemptError::Blocked(CloudError::Protocol.to_string())
                        })?;
                        sentences.insert(id, text.into());
                        if sentences.len() > 10_000
                            || sentences.values().map(String::len).sum::<usize>() > 4 * 1024 * 1024
                        {
                            return Err(AttemptError::Blocked("The cloud transcript exceeded the bounded text allowance; received audio is retained and no partial result was delivered".into()));
                        }
                        pending.remove(&id);
                        stable.store(true, Ordering::Release);
                    } else if sentence["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
                    {
                        let id = sentence["sentence_id"].as_u64().ok_or_else(|| {
                            AttemptError::Blocked(CloudError::Protocol.to_string())
                        })?;
                        if !sentences.contains_key(&id) {
                            pending.insert(id);
                        }
                        if pending.len() > 10_000 {
                            return Err(AttemptError::Blocked(CloudError::Protocol.to_string()));
                        }
                    }
                }
                "task-finished" if finish_sent.is_some() => {
                    let finished_at = Instant::now();
                    ledger
                        .observe(task, reported_usage(&event["payload"]["usage"]), true)
                        .map_err(|error| AttemptError::Blocked(error.to_string()))?;
                    if !pending.is_empty() {
                        return Err(AttemptError::Blocked(CloudError::Protocol.to_string()));
                    }
                    return Ok(SessionResult {
                        text: sentences.into_values().collect::<Vec<_>>().join(""),
                        description: format!(
                            "{}; {}",
                            settings.description(),
                            description.as_str()
                        ),
                        timing: Timing {
                            recognition_ms: Some(elapsed_ms(started_at)),
                            cloud_connect_ms: connect_ms,
                            first_task_started_ms,
                            // Stop finalisation is the device join and spool flush;
                            // the vendor can only be asked to finish once both are
                            // complete.
                            finalize_ms: outcome_at
                                .zip(capture.stopped_at())
                                .map(|(outcome, stopped_at)| millis_between(outcome, stopped_at)),
                            cloud_drain_ms: outcome_at
                                .zip(finish_sent)
                                .map(|(outcome, finish_sent)| millis_between(finish_sent, outcome)),
                            // Whole-task confirmation only: this is the vendor's
                            // `task-finished`, never an intermediate sentence.
                            cloud_final_ms: finish_sent
                                .map(|finish_sent| millis_between(finished_at, finish_sent)),
                            ..Timing::default()
                        },
                        stopped_at: capture.stopped_at(),
                        // The attempt loop marks any rebuilt result as recovered.
                        recovered: false,
                    });
                }
                "task-failed" => {
                    return Err(classify(provider_failure(
                        event["header"]["error_code"].as_str(),
                    )));
                }
                _ => {}
            }
        }
        if ready {
            for _ in 0..MAX_CHUNKS_PER_PASS {
                let Some(pcm) = stream.next().map_err(|error| {
                    AttemptError::Blocked(format!(
                        "Cannot read the retained audio: {error}. Received audio is retained on this machine"
                    ))
                })?
                else {
                    break;
                };
                if sent.saturating_add(pcm.len() as u64) > max_pcm_bytes {
                    return Err(AttemptError::Blocked(
                        "Verified upload allowance reached; no unreserved PCM was sent, received audio is retained".into(),
                    ));
                }
                within_time()?;
                socket.send_audio(&pcm).map_err(classify)?;
                sent += pcm.len() as u64;
                progressed = true;
            }
            if stream.drained() && captured.is_some() && finish_sent.is_none() {
                within_time()?;
                socket
                    .send_text(&finish_task_text(task))
                    .map_err(classify)?;
                finish_sent = Some(Instant::now());
                progressed = true;
            }
        }
        if (!ready && connecting_at.elapsed() > Duration::from_secs(15))
            || (finish_sent.is_none()
                && capture
                    .stopped_at()
                    .is_some_and(|at| at.elapsed() > Duration::from_secs(15)))
            || finish_sent.is_some_and(|at| at.elapsed() > Duration::from_secs(15))
        {
            return Err(AttemptError::Transient(CloudError::Timeout));
        }
        if !progressed {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A capture failure is not a network wait: the audio source itself is gone, so
/// the session fails immediately instead of retrying a connection.
fn capture_failure(reason: &str) -> String {
    format!(
        "The playback device was interrupted ({reason}); this capture ended and waiting for the network cannot restore it. Received audio is retained on this machine"
    )
}
fn request_vocabulary(request: &SessionRequest) -> AppResult<serde_json::Map<String, Value>> {
    let mut vocabulary = serde_json::Map::new();
    for term in request.glossary.canonical_terms() {
        if vocabulary.len() >= 2000
            || (term.is_ascii() && term.split_whitespace().count() > 7)
            || (!term.is_ascii() && term.chars().count() > 15)
        {
            return Err("The capture glossary exceeds Alibaba Message limits (2000 terms, 15 characters for mixed/non-ASCII terms, 7 space-separated words for ASCII terms). Correct the local glossary; no cloud request was sent".into());
        }
        vocabulary.insert(term.into(), json!(4));
    }
    Ok(vocabulary)
}

/// The per-capture identity used by the ledger. Retries reuse it so every request
/// and its cost stay attached to the one recording.
fn segment_name(directory: &std::path::Path) -> AppResult<String> {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| "Invalid capture directory".to_string())
}

fn task_id() -> AppResult<String> {
    #[cfg(windows)]
    {
        let guid = unsafe { windows::Win32::System::Com::CoCreateGuid() }
            .map_err(|_| "Cannot create cloud task identity")?;
        Ok(format!("{guid:?}"))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "{:032x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ))
    }
}
fn provider_failure(code: Option<&str>) -> CloudError {
    match code {
        Some("InvalidApiKey" | "AccessDenied" | "Endpoint.AccessDenied") => {
            CloudError::Unauthorized
        }
        Some("Arrearage") => CloudError::BillingUnavailable,
        // The live account answers this when the model's free quota exists but the
        // model itself is not opened for the account/workspace.
        Some("ModelNotFound" | "NotFound" | "AccessDenied.Unpurchased") => {
            CloudError::ModelUnavailable
        }
        Some("Throttling.RateQuota" | "Throttling.AllocationQuota") => CloudError::RateLimited,
        _ => CloudError::Provider,
    }
}
fn reported_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value["input_tokens"].as_u64(),
        output_tokens: value["output_tokens"].as_u64(),
        total_tokens: value["total_tokens"].as_u64(),
        duration: value["duration"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0),
    }
}

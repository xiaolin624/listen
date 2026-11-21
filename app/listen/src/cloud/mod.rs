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
    AppResult, CapturedAudio, PlaybackAudio, Preparation, Timing, elapsed_ms,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SessionEvent,
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
        Arc,
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
            "Cloud trial: Alibaba Model Studio; model={}; region={}; shared two-provider trial cap=CNY 50",
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
        let task = task_id()?;
        let segment = request
            .directory
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("Invalid capture directory")?
            .to_owned();
        if let Some(policy) = &policy {
            self.ledger
                .reserve(&task, &segment, &self.settings, policy.clone())
                .map_err(|e| e.to_string())?;
        }
        fs::create_dir(&request.directory).map_err(|e| e.to_string())?;
        let mut recording = self.audio.start(&request.directory)?;
        let stream = recording.take_pcm_stream().ok_or("Cloud trial requires continuous retained PCM; local overlapping windows are not a cloud stream")?;
        let endpoint = recording.endpoint();
        let progress = stream.progress.clone();
        let capture = CaptureControl::new(recording);
        let capture_worker = capture.clone();
        let stable = Arc::new(AtomicBool::new(false));
        let stable_signal = stable.clone();
        let transport = self.transport.clone();
        let settings = self.settings.clone();
        let key = self.key.as_ref().map_err(|e| e.to_string())?.clone();
        let ledger = self.ledger.clone();
        let clock = self.clock.clone();
        let id = request.id;
        let (tx, result) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let update = match run(capture_worker, stream, stable_signal, transport, &settings, &key, &task, &segment, policy, &ledger, clock, vocabulary) {
                Ok(result) => SessionUpdate::Finished(result),
                Err(error) => SessionUpdate::Failed(SessionFailure { error, retry_audio: None, retry_scope: Some("Received continuous PCM is retained. Automatic network/restart recovery belongs to the later recovery task; no local result was substituted.".into()), timing: Timing::default() }),
            };
            let _ = tx.send(SessionEvent { id, update });
        });
        Ok(Box::new(CloudSession {
            id,
            capture,
            stable,
            result,
            state: SessionState::Capturing,
            endpoint,
            progress,
            description: self.settings.description(),
        }))
    }
    fn retry(
        &mut self,
        _request: SessionRequest,
        _audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        Err("Cloud trial does not silently retry or use the local engine. Received PCM and its pending reservation are retained for the later recovery task".into())
    }
}
struct CloudSession {
    id: crate::recognition::SessionId,
    capture: Arc<CaptureControl>,
    stable: Arc<AtomicBool>,
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
        SessionStatus {
            state: self.state,
            progress: self.progress.snapshot(),
            endpoint: self.endpoint.clone(),
            detail: format!(
                "{}\n{}",
                self.description,
                if self.capture.is_stopped() {
                    "Waiting for whole-task cloud confirmation; unconfirmed sentences are not delivered."
                } else {
                    "Capturing continuous playback to local PCM before upload. Only this capture and its glossary are supplied to Alibaba; no messages are sent automatically."
                }
            ),
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        if self.capture.failure().is_some() {
            self.stop();
        }
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
    transport: Arc<dyn MessageTransport>,
    settings: &AliSettings,
    key: &Secret,
    task: &str,
    segment: &str,
    policy: Option<TrialCostPolicy>,
    ledger: &TrialLedger,
    clock: Arc<dyn TrialClock>,
    vocabulary: serde_json::Map<String, Value>,
) -> AppResult<SessionResult> {
    let started_at = Instant::now();
    let mut description = String::new();
    let result = (|| -> AppResult<SessionResult> {
        let policy = match policy {
            Some(policy) => policy,
            None => {
                let policy = transport
                    .cost_policy(settings, key)
                    .map_err(|e| e.to_string())?;
                ledger
                    .reserve(task, segment, settings, policy.clone())
                    .map_err(|e| e.to_string())?;
                policy
            }
        };
        let max_pcm_bytes = policy.max_pcm_bytes();
        let session_started = clock.now();
        let within_time = || {
            if policy.max_duration().is_some_and(|limit| {
                clock.now().saturating_duration_since(session_started) >= limit
            }) {
                Err("Cloud trial time allowance reached. Received audio is retained; unconfirmed text was not delivered".to_owned())
            } else {
                Ok(())
            }
        };
        let connecting_at = Instant::now();
        let mut socket = transport
            .connect(settings, key)
            .map_err(|e| e.to_string())?;
        within_time()?;
        socket.send_text(&json!({"header":{"action":"run-task","task_id":task,"streaming":"duplex"},"payload":{"task_group":"audio","task":"asr","function":"recognition","model":settings.model,"parameters":{"format":"pcm","sample_rate":16000,"disfluency_removal_enabled":false,"intermediate_result_enabled":false,"keep_dialect":true,"heartbeat":true,"vocabulary":vocabulary},"input":{}}}).to_string()).map_err(|e| e.to_string())?;
        let mut ready = false;
        let mut finish_sent = None;
        let mut sent = 0u64;
        let mut sentences = BTreeMap::<u64, String>::new();
        let mut pending = BTreeSet::new();
        loop {
            within_time()?;
            if capture.failure().is_some() {
                capture.stop();
            }
            let captured = capture.outcome();
            if let Some(audio) = &captured {
                description = audio.as_ref().map_err(Clone::clone)?.description.clone();
            }
            for _ in 0..8 {
                let Some(raw) = socket.poll().map_err(|e| e.to_string())? else {
                    break;
                };
                if raw.len() > 1024 * 1024 {
                    return Err(CloudError::Protocol.to_string());
                }
                let event: Value =
                    serde_json::from_str(&raw).map_err(|_| CloudError::Protocol.to_string())?;
                if event["header"]["task_id"].as_str() != Some(task) {
                    continue;
                }
                let kind = event["header"]["event"]
                    .as_str()
                    .ok_or_else(|| CloudError::Protocol.to_string())?;
                match kind {
                    "task-started" => ready = true,
                    "result-generated" if ready => {
                        ledger
                            .observe(task, reported_usage(&event["payload"]["usage"]), false)
                            .map_err(|e| e.to_string())?;
                        let sentence = &event["payload"]["output"]["sentence"];
                        if sentence["heartbeat"] == true {
                            continue;
                        }
                        if sentence["sentence_id"].as_u64().is_none()
                            || sentence["text"].as_str().is_none()
                            || sentence["sentence_end"].as_bool().is_none()
                        {
                            return Err(CloudError::Protocol.to_string());
                        }
                        if sentence["sentence_end"] == true {
                            let id = sentence["sentence_id"]
                                .as_u64()
                                .ok_or_else(|| CloudError::Protocol.to_string())?;
                            let text = sentence["text"]
                                .as_str()
                                .ok_or_else(|| CloudError::Protocol.to_string())?;
                            sentences.insert(id, text.into());
                            if sentences.len() > 10_000
                                || sentences.values().map(String::len).sum::<usize>()
                                    > 4 * 1024 * 1024
                            {
                                return Err("The cloud transcript exceeded the bounded text allowance; received audio is retained and no partial result was delivered".into());
                            }
                            pending.remove(&id);
                            stable.store(true, Ordering::Release);
                        } else if sentence["text"]
                            .as_str()
                            .is_some_and(|text| !text.trim().is_empty())
                        {
                            let id = sentence["sentence_id"]
                                .as_u64()
                                .ok_or_else(|| CloudError::Protocol.to_string())?;
                            if !sentences.contains_key(&id) {
                                pending.insert(id);
                            }
                            if pending.len() > 10_000 {
                                return Err(CloudError::Protocol.to_string());
                            }
                        }
                    }
                    "task-finished" if finish_sent.is_some() => {
                        ledger
                            .observe(task, reported_usage(&event["payload"]["usage"]), true)
                            .map_err(|e| e.to_string())?;
                        if !pending.is_empty() {
                            return Err(CloudError::Protocol.to_string());
                        }
                        return Ok(SessionResult {
                            text: sentences.into_values().collect::<Vec<_>>().join(""),
                            description: format!("{}; {}", settings.description(), description),
                            timing: Timing {
                                recognition_ms: Some(elapsed_ms(started_at)),
                                ..Timing::default()
                            },
                            stopped_at: capture.stopped_at(),
                        });
                    }
                    "task-failed" => {
                        return Err(
                            provider_failure(event["header"]["error_code"].as_str()).to_string()
                        );
                    }
                    _ => {}
                }
            }
            if ready {
                if let Some(pcm) = stream.next()? {
                    if sent.saturating_add(pcm.len() as u64) > max_pcm_bytes {
                        return Err("Verified upload allowance reached; no unreserved PCM was sent, received audio is retained".into());
                    }
                    within_time()?;
                    socket.send_audio(&pcm).map_err(|e| e.to_string())?;
                    sent += pcm.len() as u64;
                }
                if stream.drained() && captured.is_some() && finish_sent.is_none() {
                    within_time()?;
                    socket.send_text(&json!({"header":{"action":"finish-task","task_id":task,"streaming":"duplex"},"payload":{"input":{}}}).to_string()).map_err(|e| e.to_string())?;
                    finish_sent = Some(Instant::now());
                }
            }
            if (!ready && connecting_at.elapsed() > Duration::from_secs(15))
                || (finish_sent.is_none()
                    && capture
                        .stopped_at()
                        .is_some_and(|at| at.elapsed() > Duration::from_secs(15)))
                || finish_sent.is_some_and(|at| at.elapsed() > Duration::from_secs(15))
            {
                return Err(CloudError::Timeout.to_string());
            }
            thread::sleep(Duration::from_millis(2));
        }
    })();
    capture.stop();
    match (result, capture.wait()) {
        (Err(error), Err(capture_error)) => Err(format!("{error}. Capture: {capture_error}")),
        (result, Ok(_)) => result,
        (Ok(_), Err(capture_error)) => Err(capture_error),
    }
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

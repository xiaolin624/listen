//! Scripted recognition adapters for integration tests.
//!
//! Integration tests are a separate crate, so the one shared scripted adapter has
//! to live in this crate to be reusable. It is hidden from the public
//! documentation and no product path constructs it: production recognition goes
//! through [`crate::recognition::LocalSessions`] or the cloud adapter.
//!
//! A test builds [`ScriptedSessions`], keeps the [`ScriptedControl`] handle, and
//! drives every started session through [`ScriptedSessionControl`]: it queues the
//! outcomes the session hands back in order, reports the state the adapter claims,
//! records every stop, and counts retry requests.

use crate::{
    AppResult, CapturedAudio, Preparation, UsageSummary,
    recognition::{
        RecognitionMode, RecognitionReadiness, RecognitionSession, RecognitionSessions,
        SegmentUsage, SessionEvent, SessionFailure, SessionId, SessionRequest, SessionResult,
        SessionState, SessionStatus, SessionUpdate,
    },
    streaming::CaptureProgress,
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};

/// A recognition adapter whose sessions the test drives. Each start creates a
/// session backed by its own ordered queue; the test enqueues outcomes whenever it
/// wants, and the session hands them back one poll at a time.
pub struct ScriptedSessions {
    shared: Arc<Shared>,
}

impl Default for ScriptedSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedSessions {
    /// An adapter that reports Ready, no cloud usage and no segment usage.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                sessions: Mutex::new(Vec::new()),
                backend: Mutex::new("scripted recognition".into()),
                mode: Mutex::new(RecognitionMode::Local),
                usage: Mutex::new(None),
                segment_usage: Mutex::new(HashMap::new()),
                retries: AtomicUsize::new(0),
            }),
        }
    }

    /// The note the result panel shows for this engine.
    pub fn with_backend(self, note: impl Into<String>) -> Self {
        *self.shared.backend.lock().unwrap() = note.into();
        self
    }

    /// Which recognition path this adapter declares. Defaults to the local engine.
    pub fn with_mode(self, mode: RecognitionMode) -> Self {
        *self.shared.mode.lock().unwrap() = mode;
        self
    }

    /// Publish the cumulative summary `usage()` reports.
    pub fn with_usage(self, usage: UsageSummary) -> Self {
        *self.shared.usage.lock().unwrap() = Some(usage);
        self
    }

    /// The handle a test uses to script sessions and observe the lifecycle.
    pub fn control(&self) -> ScriptedControl {
        ScriptedControl {
            shared: self.shared.clone(),
        }
    }

    /// Start one session: create its directory and retained audio, then queue it
    /// behind every session started before it.
    fn begin(&self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir_all(&request.directory).map_err(|error| error.to_string())?;
        fs::write(request.directory.join("audio.wav"), b"scripted capture")
            .map_err(|error| error.to_string())?;
        let (events, receiver) = mpsc::channel();
        let session = Arc::new(Session {
            id: request.id,
            directory: request.directory,
            events,
            stopped: AtomicUsize::new(0),
            state: Mutex::new(SessionState::Capturing),
        });
        self.shared.sessions.lock().unwrap().push(session.clone());
        Ok(Box::new(ScriptedSession {
            session,
            events: receiver,
        }))
    }
}

impl RecognitionSessions for ScriptedSessions {
    fn mode(&self) -> RecognitionMode {
        *self.shared.mode.lock().unwrap()
    }

    fn preparation(&mut self) -> RecognitionReadiness {
        RecognitionReadiness {
            state: Preparation::Ready,
            backend: Some(self.shared.backend.lock().unwrap().clone()),
            elapsed_ms: None,
            error: None,
        }
    }

    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        self.begin(request)
    }

    /// Record that the application asked, then answer like any engine that cannot
    /// retry. A test asserts [`ScriptedControl::retry_attempts`] is zero where the
    /// application must not ask at all.
    fn retry(
        &mut self,
        _request: SessionRequest,
        _audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        self.shared.retries.fetch_add(1, Ordering::SeqCst);
        Err(crate::recognition::RETRY_UNSUPPORTED.into())
    }

    fn usage(&mut self) -> Option<UsageSummary> {
        self.shared.usage.lock().unwrap().clone()
    }

    fn segment_usage(&self, directory: &Path) -> Option<SegmentUsage> {
        self.shared
            .segment_usage
            .lock()
            .unwrap()
            .get(&directory.file_name()?.to_string_lossy().into_owned())
            .cloned()
    }
}

/// The test-side handle to every session a [`ScriptedSessions`] started, in start
/// order.
#[derive(Clone)]
pub struct ScriptedControl {
    shared: Arc<Shared>,
}

impl ScriptedControl {
    /// How many sessions the adapter was asked to start.
    pub fn count(&self) -> usize {
        self.shared.sessions.lock().unwrap().len()
    }

    /// The session started at `index`, counting from zero. Panics when the
    /// application never started that many sessions.
    pub fn session(&self, index: usize) -> ScriptedSessionControl {
        let sessions = self.shared.sessions.lock().unwrap();
        let session = sessions.get(index).cloned().unwrap_or_else(|| {
            panic!(
                "no scripted session {} was started ({} started)",
                index + 1,
                sessions.len()
            )
        });
        ScriptedSessionControl { session }
    }

    /// The newest session the adapter started.
    pub fn last(&self) -> ScriptedSessionControl {
        self.session(self.count().saturating_sub(1))
    }

    /// How many times the application asked this adapter to retry retained audio.
    pub fn retry_attempts(&self) -> usize {
        self.shared.retries.load(Ordering::SeqCst)
    }

    /// Publish the cumulative summary `usage()` reports.
    pub fn set_usage(&self, usage: UsageSummary) {
        *self.shared.usage.lock().unwrap() = Some(usage);
    }

    /// Publish one segment directory's usage, keyed the way the recognition seam
    /// names a segment (its directory name).
    pub fn set_segment_usage(&self, directory: &Path, usage: SegmentUsage) {
        self.shared.segment_usage.lock().unwrap().insert(
            directory
                .file_name()
                .expect("a segment directory has a name")
                .to_string_lossy()
                .into_owned(),
            usage,
        );
    }
}

/// One started session. Cloning it only shares the same session.
#[derive(Clone)]
pub struct ScriptedSessionControl {
    session: Arc<Session>,
}

impl ScriptedSessionControl {
    /// The id the application assigned to this session.
    pub fn id(&self) -> SessionId {
        self.session.id
    }

    /// The directory the application asked this session to record into.
    pub fn directory(&self) -> PathBuf {
        self.session.directory.clone()
    }

    /// A sender for arbitrary events, including an event that carries another
    /// session's id: the adapter only queues it, the application decides.
    pub fn sender(&self) -> Sender<SessionEvent> {
        self.session.events.clone()
    }

    /// Queue one update for this session's own id.
    pub fn send(&self, update: SessionUpdate) {
        self.send_event(SessionEvent {
            id: self.session.id,
            update,
        });
    }

    /// Queue one event exactly as the test built it.
    pub fn send_event(&self, event: SessionEvent) {
        self.session
            .events
            .send(event)
            .expect("the scripted session is still open");
    }

    /// Queue a complete result for this session's own id.
    pub fn finish(&self, text: &str) {
        self.send(SessionUpdate::Finished(SessionResult {
            text: text.into(),
            description: "scripted playback".into(),
            timing: Default::default(),
            stopped_at: None,
            recovered: false,
        }));
    }

    /// Queue a complete result with an adapter-provided measurement.
    pub fn finish_result(&self, result: SessionResult) {
        self.send(SessionUpdate::Finished(result));
    }

    /// Queue a stable sentence: recognition progress, never a deliverable result.
    pub fn stable(&self, text: &str) {
        self.send(SessionUpdate::Stable(text.into()));
    }

    /// Queue a failure with no retained audio.
    pub fn fail(&self, error: &str) {
        self.send(SessionUpdate::Failed(SessionFailure {
            error: error.into(),
            retry_audio: None,
            retry_scope: None,
            timing: Default::default(),
        }));
    }

    /// Report the given state until `stop`, a queued terminal event or another
    /// `set_state` replaces it.
    pub fn set_state(&self, state: SessionState) {
        *self.session.state.lock().unwrap() = state;
    }

    /// How many stop requests this session received. The application must not
    /// pretend a capture is still recording after it stopped it.
    pub fn stop_count(&self) -> usize {
        self.session.stopped.load(Ordering::SeqCst)
    }
}

struct Shared {
    sessions: Mutex<Vec<Arc<Session>>>,
    backend: Mutex<String>,
    /// Which path this scripted adapter claims to implement. It is a declaration,
    /// not a parsed prefix: a test that wants the cloud branches says so here and
    /// can still show any `backend` wording it likes.
    mode: Mutex<RecognitionMode>,
    usage: Mutex<Option<UsageSummary>>,
    segment_usage: Mutex<HashMap<String, SegmentUsage>>,
    retries: AtomicUsize,
}

struct Session {
    id: SessionId,
    directory: PathBuf,
    events: Sender<SessionEvent>,
    stopped: AtomicUsize,
    state: Mutex<SessionState>,
}

struct ScriptedSession {
    session: Arc<Session>,
    events: Receiver<SessionEvent>,
}

impl RecognitionSession for ScriptedSession {
    fn stop(&mut self) {
        self.session.stopped.fetch_add(1, Ordering::SeqCst);
        *self.session.state.lock().unwrap() = SessionState::AwaitingFinal;
    }

    fn status(&self) -> SessionStatus {
        SessionStatus {
            state: *self.session.state.lock().unwrap(),
            progress: CaptureProgress::default(),
            endpoint: None,
            detail: "scripted recognition session".into(),
        }
    }

    fn poll(&mut self) -> Option<SessionEvent> {
        // Queued events are delivered exactly as scripted. The session's own
        // reported state only changes through `stop` and `set_state`: the
        // application decides whether an event applies to this session, so an
        // event carrying another session's id must not change this one's status.
        self.events.try_recv().ok()
    }
}

//! The recognition seam's optional capabilities and its shared scripted adapter.
//!
//! A recognition adapter that only implements `start` must be enough to drive one
//! complete capture: `preparation` defaults to Ready (a session that is not ready
//! still fails closed in `start`) and `retry` defaults to "this engine does not
//! support retrying". The scripted adapter in `listen::testing` is the one all the
//! other integration tests reuse; it hands back its queued outcomes in order and
//! records how many times the application stopped a session or asked for a retry.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, Preparation, Target,
    TargetSelection, Timing,
    glossary::Glossary,
    recognition::{
        RecognitionSession, RecognitionSessions, SessionEvent, SessionFailure, SessionId,
        SessionRequest, SessionResult, SessionState, SessionStatus, SessionUpdate,
    },
    testing::ScriptedSessions,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-recognition-testing-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Editor(Mutex<String>);
struct Selection(Arc<Editor>);
struct BoundEditor(Arc<Editor>);
impl TargetSelection for Selection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(BoundEditor(self.0.clone()))))
    }
}
impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.0.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut draft = self.0.0.lock().unwrap();
        if *draft != expected {
            return Err("draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

/// The smallest useful adapter: only `start` is implemented, so every optional
/// capability has to come from the trait's defaults.
struct MinimalSessions {
    control: MinimalControl,
}
#[derive(Clone)]
struct MinimalControl {
    starts: Arc<AtomicUsize>,
    directories: Arc<Mutex<Vec<PathBuf>>>,
    updates: Arc<Mutex<Option<mpsc::Sender<SessionUpdate>>>>,
}
impl MinimalControl {
    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
    fn directory(&self) -> PathBuf {
        self.directories
            .lock()
            .unwrap()
            .last()
            .expect("a session was started")
            .clone()
    }
    fn send(&self, update: SessionUpdate) {
        self.updates
            .lock()
            .unwrap()
            .as_ref()
            .expect("a session is running")
            .send(update)
            .unwrap();
    }
}
impl RecognitionSessions for MinimalSessions {
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir_all(&request.directory).map_err(|error| error.to_string())?;
        fs::write(request.directory.join("audio.wav"), b"minimal capture")
            .map_err(|error| error.to_string())?;
        self.control.starts.fetch_add(1, Ordering::SeqCst);
        self.control
            .directories
            .lock()
            .unwrap()
            .push(request.directory.clone());
        let (tx, rx) = mpsc::channel();
        *self.control.updates.lock().unwrap() = Some(tx);
        Ok(Box::new(MinimalSession {
            id: request.id,
            events: rx,
            stopped: false,
        }))
    }
}
struct MinimalSession {
    id: SessionId,
    events: mpsc::Receiver<SessionUpdate>,
    stopped: bool,
}
impl RecognitionSession for MinimalSession {
    fn stop(&mut self) {
        self.stopped = true;
    }
    fn status(&self) -> SessionStatus {
        SessionStatus {
            state: if self.stopped {
                SessionState::AwaitingFinal
            } else {
                SessionState::Capturing
            },
            progress: Default::default(),
            endpoint: None,
            detail: "minimal recognition session".into(),
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        let update = self.events.try_recv().ok()?;
        Some(SessionEvent {
            id: self.id,
            update,
        })
    }
}

fn minimal() -> (MinimalSessions, MinimalControl) {
    let control = MinimalControl {
        starts: Arc::new(AtomicUsize::new(0)),
        directories: Arc::new(Mutex::new(Vec::new())),
        updates: Arc::new(Mutex::new(None)),
    };
    (
        MinimalSessions {
            control: control.clone(),
        },
        control,
    )
}

fn wait_for(app: &mut Application, phase: Phase) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let snapshot = app.snapshot();
        if snapshot.phase == phase {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{snapshot:?}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn complete(text: &str) -> SessionUpdate {
    SessionUpdate::Finished(SessionResult {
        text: text.into(),
        description: "minimal playback".into(),
        timing: Timing::default(),
        stopped_at: None,
        recovered: false,
    })
}

fn update_name(update: &SessionUpdate) -> &'static str {
    match update {
        SessionUpdate::Stable(_) => "a stable sentence",
        SessionUpdate::Finished(_) => "a complete result",
        SessionUpdate::Failed(_) => "a failure",
    }
}

#[test]
fn an_adapter_with_only_start_is_ready_and_completes_a_capture() {
    let runtime = Runtime::new();
    let editor = Arc::new(Editor(Mutex::new("draft".into())));
    let (sessions, control) = minimal();
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));

    // `preparation` has no implementation in this adapter: the seam must answer
    // Ready, and `usage`/`segment_usage` must answer nothing rather than invent a
    // number.
    let initial = app.snapshot();
    assert_eq!(initial.phase, Phase::Ready, "{initial:?}");
    assert_eq!(initial.preparation, Preparation::Ready, "{initial:?}");
    assert_eq!(initial.usage, None, "no adapter code promised a cloud cost");
    app.poll();
    assert_eq!(app.snapshot().usage, None, "{:?}", app.snapshot());

    app.dispatch(Control::Start);
    {
        let capturing = app.snapshot();
        assert_eq!(capturing.phase, Phase::Capturing, "{capturing:?}");
    }
    app.dispatch(Control::Stop);
    control.send(complete("only start was implemented"));
    let result = wait_for(&mut app, Phase::Result);
    assert_eq!(result.text, "only start was implemented");
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert_eq!(
        *editor.0.lock().unwrap(),
        "draft\nonly start was implemented"
    );
    assert_eq!(control.starts(), 1, "exactly one session was started");
}

#[test]
fn an_engine_that_does_not_implement_retry_is_answered_without_a_retry_session() {
    let runtime = Runtime::new();
    let editor = Arc::new(Editor(Mutex::new("protected draft".into())));
    let (sessions, control) = minimal();
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let directory = control.directory();
    control.send(SessionUpdate::Failed(SessionFailure {
        error: "the capture was interrupted".into(),
        retry_audio: Some(CapturedAudio {
            path: directory.join("audio.wav"),
            description: "minimal playback".into(),
        }),
        retry_scope: None,
        timing: Timing::default(),
    }));
    let failed = wait_for(&mut app, Phase::Failure);
    assert_eq!(failed.recognition, Some(SessionState::Failed), "{failed:?}");
    assert!(failed.retry_available, "{failed:?}");

    // The adapter never wrote a retry method. The seam's default must answer for
    // it: no new session is spawned and the failure says why.
    app.dispatch(Control::Retry);
    let refused = app.snapshot();
    assert_eq!(
        control.starts(),
        1,
        "no retry session was started: {refused:?}"
    );
    assert!(
        refused.detail.contains("does not support retrying"),
        "the default retry answer is visible: {refused:?}"
    );
    assert_eq!(
        refused.recognition,
        Some(SessionState::Failed),
        "{refused:?}"
    );
    assert_eq!(*editor.0.lock().unwrap(), "protected draft");
}

#[test]
fn scripted_sessions_hands_back_queued_outcomes_in_order_and_records_stops() {
    let runtime = Runtime::new();
    let mut sessions = ScriptedSessions::new();
    let script = sessions.control();
    let request = SessionRequest {
        id: SessionId(7),
        directory: runtime.0.join("scripted"),
        glossary: Glossary::default(),
    };
    let mut session = sessions
        .start(request)
        .expect("the scripted adapter starts");
    let handle = script.session(0);
    assert_eq!(handle.id(), SessionId(7));
    assert_eq!(handle.stop_count(), 0);

    handle.stable("first sentence");
    handle.fail("second event is the failure");
    handle.finish("third event is the complete result");

    let first = session.poll().expect("the first queued outcome");
    assert_eq!(first.id, SessionId(7));
    match first.update {
        SessionUpdate::Stable(text) => assert_eq!(text, "first sentence"),
        other => panic!(
            "expected the stable sentence first, got {}",
            update_name(&other)
        ),
    }
    let second = session.poll().expect("the second queued outcome");
    match second.update {
        SessionUpdate::Failed(failure) => {
            assert_eq!(failure.error, "second event is the failure");
        }
        other => panic!("expected the failure second, got {}", update_name(&other)),
    }
    let third = session.poll().expect("the third queued outcome");
    match third.update {
        SessionUpdate::Finished(result) => {
            assert_eq!(result.text, "third event is the complete result");
        }
        other => panic!(
            "expected the complete result third, got {}",
            update_name(&other)
        ),
    }
    assert!(session.poll().is_none(), "nothing is left in the script");

    session.stop();
    session.stop();
    assert_eq!(handle.stop_count(), 2, "every stop request is recorded");
    assert_eq!(
        script.retry_attempts(),
        0,
        "the scripted adapter was never asked to retry"
    );
}

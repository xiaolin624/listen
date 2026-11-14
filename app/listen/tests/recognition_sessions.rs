//! User workflows driven through the same application interface as the hotkey.
//! The recognition adapter controls confirmation timing, never target delivery.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, Preparation, Target,
    TargetSelection, Timing,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SessionEvent,
        SessionFailure, SessionId, SessionRequest, SessionResult, SessionState, SessionStatus,
        SessionUpdate,
    },
};
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-session-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

struct ControlledSessions {
    inputs: VecDeque<mpsc::Receiver<SessionEvent>>,
    started: mpsc::Sender<SessionId>,
}
struct ControlledSession {
    events: mpsc::Receiver<SessionEvent>,
    stopped: bool,
}
impl RecognitionSessions for ControlledSessions {
    fn preparation(&mut self) -> RecognitionReadiness {
        RecognitionReadiness {
            state: Preparation::Ready,
            backend: Some("controlled recognition".into()),
            elapsed_ms: None,
            error: None,
        }
    }
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir(&request.directory).map_err(|error| error.to_string())?;
        fs::write(request.directory.join("audio.wav"), b"retained capture")
            .map_err(|error| error.to_string())?;
        self.started.send(request.id).unwrap();
        Ok(Box::new(ControlledSession {
            events: self
                .inputs
                .pop_front()
                .ok_or("missing controlled session")?,
            stopped: false,
        }))
    }
    fn retry(
        &mut self,
        _request: SessionRequest,
        _audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        Err("no retained capture in this scenario".into())
    }
}
impl RecognitionSession for ControlledSession {
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
            detail: "controlled recognition session".into(),
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        self.events.try_recv().ok()
    }
}

fn wait_for_result(app: &mut Application) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if state.phase == Phase::Result {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "result did not arrive: {state:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn complete(id: SessionId, text: &str) -> SessionEvent {
    SessionEvent {
        id,
        update: SessionUpdate::Finished(SessionResult {
            text: text.into(),
            description: "controlled playback".into(),
            timing: Timing::default(),
            stopped_at: None,
        }),
    }
}

#[test]
fn stable_sentences_wait_for_stop_and_complete_confirmation_before_appending() {
    let runtime = Runtime::new();
    fs::write(
        runtime.0.join("current-result.txt"),
        "previous complete result",
    )
    .unwrap();
    let editor = Arc::new(Editor(Mutex::new("initial draft".into())));
    let (events, input) = mpsc::channel();
    let (started, ids) = mpsc::channel();
    let sessions = ControlledSessions {
        inputs: VecDeque::from([input]),
        started,
    };
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Start);
    let id = ids.recv().unwrap();
    events
        .send(SessionEvent {
            id,
            update: SessionUpdate::Stable("stable sentence".into()),
        })
        .unwrap();
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    assert_eq!(app.snapshot().recognition, Some(SessionState::Stable));
    assert_eq!(app.snapshot().text, "previous complete result");
    assert_eq!(*editor.0.lock().unwrap(), "initial draft");

    *editor.0.lock().unwrap() = "draft edited during capture".into();
    app.dispatch(Control::Stop);
    app.poll();
    let waiting = app.snapshot();
    assert_eq!(waiting.phase, Phase::Processing);
    assert_eq!(waiting.recognition, Some(SessionState::AwaitingFinal));
    assert_eq!(*editor.0.lock().unwrap(), "draft edited during capture");

    events
        .send(complete(id, "complete confirmed transcript"))
        .unwrap();
    let result = wait_for_result(&mut app);
    assert_eq!(result.text, "complete confirmed transcript");
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        *editor.0.lock().unwrap(),
        "draft edited during capture\ncomplete confirmed transcript"
    );
}

#[test]
fn a_save_failure_keeps_recognition_failed_audio_recoverable_and_the_target_unchanged() {
    let runtime = Runtime::new();
    // A directory at the result path forces a real storage failure.
    fs::create_dir(runtime.0.join("current-result.txt")).unwrap();
    let editor = Arc::new(Editor(Mutex::new("protected draft".into())));
    let (events, input) = mpsc::channel();
    let (started, ids) = mpsc::channel();
    let mut app = Application::with_recognition_sessions(
        Box::new(ControlledSessions {
            inputs: VecDeque::from([input]),
            started,
        }),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Selection(editor.clone())));
    app.dispatch(Control::Start);
    let id = ids.recv().unwrap();
    app.dispatch(Control::Stop);
    events
        .send(complete(id, "complete result kept for recovery"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.snapshot().phase != Phase::Failure {
        app.poll();
        assert!(Instant::now() < deadline, "{:?}", app.snapshot());
        thread::sleep(Duration::from_millis(2));
    }
    let failure = app.snapshot();
    assert_eq!(failure.recognition, Some(SessionState::Failed));
    assert_eq!(failure.text, "complete result kept for recovery");
    assert!(!failure.retained_audio.is_empty());
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    assert_eq!(*editor.0.lock().unwrap(), "protected draft");
}

#[test]
fn repeated_confirmation_and_other_sessions_late_events_cannot_deliver_twice_or_change_a_new_capture()
 {
    let runtime = Runtime::new();
    let editor = Arc::new(Editor(Mutex::new("draft".into())));
    let (first_events, first_input) = mpsc::channel();
    let (second_events, second_input) = mpsc::channel();
    let (started, ids) = mpsc::channel();
    let mut app = Application::with_recognition_sessions(
        Box::new(ControlledSessions {
            inputs: VecDeque::from([first_input, second_input]),
            started,
        }),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Start);
    let first_id = ids.recv().unwrap();
    // Even a session's own complete-looking text cannot bypass Stop.
    first_events
        .send(complete(first_id, "premature confirmation"))
        .unwrap();
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    assert_eq!(*editor.0.lock().unwrap(), "draft");
    app.dispatch(Control::Stop);
    app.dispatch(Control::Stop);
    first_events
        .send(complete(first_id, "first confirmed result"))
        .unwrap();
    let first = wait_for_result(&mut app);
    assert_eq!(first.text, "first confirmed result");
    first_events
        .send(complete(first_id, "duplicate must not replace the result"))
        .unwrap();
    app.poll();
    assert_eq!(app.snapshot().text, "first confirmed result");
    assert_eq!(*editor.0.lock().unwrap(), "draft\nfirst confirmed result");

    app.dispatch(Control::Start);
    let second_id = ids.recv().unwrap();
    for update in [
        SessionUpdate::Failed(SessionFailure {
            error: "late failure from the old session".into(),
            retry_audio: None,
            retry_scope: None,
            timing: Timing::default(),
        }),
        SessionUpdate::Stable("late old sentence".into()),
        complete(first_id, "late old result").update,
    ] {
        second_events
            .send(SessionEvent {
                id: first_id,
                update,
            })
            .unwrap();
        app.poll();
        let capturing = app.snapshot();
        assert_eq!(capturing.phase, Phase::Capturing);
        assert_eq!(capturing.recognition, Some(SessionState::Capturing));
        assert_eq!(capturing.text, "first confirmed result");
        assert_eq!(*editor.0.lock().unwrap(), "draft\nfirst confirmed result");
    }
    app.dispatch(Control::Stop);
    second_events
        .send(complete(first_id, "old confirmation after the new stop"))
        .unwrap();
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Processing);
    second_events
        .send(complete(second_id, "second confirmed result"))
        .unwrap();
    let second = wait_for_result(&mut app);
    assert_eq!(second.text, "second confirmed result");
    second_events
        .send(complete(second_id, "duplicate second result"))
        .unwrap();
    app.poll();
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().text, "second confirmed result");
    assert_eq!(
        *editor.0.lock().unwrap(),
        "draft\nfirst confirmed result\nsecond confirmed result"
    );
}

//! User workflows driven through the same application interface as the hotkey.
//! The recognition adapter controls confirmation timing, never target delivery.
use listen::{
    AppResult, Application, Control, Delivery, Phase, Target, TargetSelection, Timing,
    recognition::{
        SessionEvent, SessionFailure, SessionId, SessionResult, SessionState, SessionUpdate,
    },
    testing::ScriptedSessions,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
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
            recovered: false,
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
    let adapter = ScriptedSessions::new();
    let script = adapter.control();
    let mut app =
        Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Start);
    let id = script.last().id();
    script.session(0).send_event(SessionEvent {
        id,
        update: SessionUpdate::Stable("stable sentence".into()),
    });
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

    script
        .session(0)
        .send_event(complete(id, "complete confirmed transcript"));
    let result = wait_for_result(&mut app);
    assert_eq!(result.text, "complete confirmed transcript");
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        *editor.0.lock().unwrap(),
        "draft edited during capture\ncomplete confirmed transcript"
    );
    assert_eq!(
        script.retry_attempts(),
        0,
        "a completed capture is never retried"
    );
}

#[test]
fn a_save_failure_keeps_recognition_failed_audio_recoverable_and_the_target_unchanged() {
    let runtime = Runtime::new();
    // A directory at the result path forces a real storage failure.
    fs::create_dir(runtime.0.join("current-result.txt")).unwrap();
    let editor = Arc::new(Editor(Mutex::new("protected draft".into())));
    let adapter = ScriptedSessions::new();
    let script = adapter.control();
    let mut app =
        Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));
    app.dispatch(Control::Start);
    let id = script.last().id();
    app.dispatch(Control::Stop);
    script
        .session(0)
        .send_event(complete(id, "complete result kept for recovery"));
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
    let adapter = ScriptedSessions::new();
    let script = adapter.control();
    let mut app =
        Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));

    app.dispatch(Control::Start);
    let first_id = script.last().id();
    // Even a session's own complete-looking text cannot bypass Stop.
    script
        .session(0)
        .send_event(complete(first_id, "premature confirmation"));
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    assert_eq!(*editor.0.lock().unwrap(), "draft");
    app.dispatch(Control::Stop);
    app.dispatch(Control::Stop);
    script
        .session(0)
        .send_event(complete(first_id, "first confirmed result"));
    let first = wait_for_result(&mut app);
    assert_eq!(first.text, "first confirmed result");
    script
        .session(0)
        .send_event(complete(first_id, "duplicate must not replace the result"));
    app.poll();
    assert_eq!(app.snapshot().text, "first confirmed result");
    assert_eq!(*editor.0.lock().unwrap(), "draft\nfirst confirmed result");

    app.dispatch(Control::Start);
    let second_id = script.last().id();
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
        script.session(1).send_event(SessionEvent {
            id: first_id,
            update,
        });
        app.poll();
        let capturing = app.snapshot();
        assert_eq!(capturing.phase, Phase::Capturing);
        assert_eq!(capturing.recognition, Some(SessionState::Capturing));
        assert_eq!(capturing.text, "first confirmed result");
        assert_eq!(*editor.0.lock().unwrap(), "draft\nfirst confirmed result");
    }
    app.dispatch(Control::Stop);
    script
        .session(1)
        .send_event(complete(first_id, "old confirmation after the new stop"));
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Processing);
    script
        .session(1)
        .send_event(complete(second_id, "second confirmed result"));
    let second = wait_for_result(&mut app);
    assert_eq!(second.text, "second confirmed result");
    script
        .session(1)
        .send_event(complete(second_id, "duplicate second result"));
    app.poll();
    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().text, "second confirmed result");
    assert_eq!(
        *editor.0.lock().unwrap(),
        "draft\nfirst confirmed result\nsecond confirmed result"
    );
    assert_eq!(
        script.retry_attempts(),
        0,
        "no late or duplicate event may ask the adapter to retry"
    );
}

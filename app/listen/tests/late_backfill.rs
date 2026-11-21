//! Late-result backfill through the same application interface as the global
//! hotkey: a result that waited for the network is appended only while the
//! original target still holds the draft recorded when that segment stopped, only
//! once, and never to whatever happens to be in the foreground later.
//!
//! Only the playback device, the recognition sessions and the editors are
//! substituted. Every assertion is a user-visible state, complete text, target
//! draft or durable record observed through `Application::dispatch`, `poll` and
//! `snapshot`; no paid network is contacted.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, Refine, RefineText, Snapshot,
    Target, TargetSelection, Timing,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SessionEvent,
        SessionFailure, SessionId, SessionRequest, SessionResult, SessionState, SessionStatus,
        SessionUpdate,
    },
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
            "listen-late-backfill-{}-{}-{}",
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

/// One controllable editor. Every way a real target can stop being provably safe
/// is a flag here: the window can disappear, the control can become read-only, a
/// write can be applied but its response lost, or the control can hold only a
/// prefix of what was written.
#[derive(Default)]
struct Editor {
    draft: Mutex<String>,
    /// The window/control disappeared: both reading and writing fail.
    available: AtomicBool,
    /// Reading works but writing is refused, like a control that became read-only.
    read_only: AtomicBool,
    /// The write is applied but the adapter reports a lost response.
    lose_response: AtomicBool,
    /// The write stores only this many characters and reports a truncation.
    limit: Mutex<Option<usize>>,
    /// The write blocks until the test releases it, so the persisted started state
    /// can be observed while the target holds the write.
    block_replace: AtomicBool,
    replace_entered: AtomicBool,
    replace_release: AtomicBool,
    replace_calls: AtomicUsize,
    content_calls: AtomicUsize,
}
impl Editor {
    fn new(draft: &str) -> Arc<Self> {
        Arc::new(Self {
            draft: Mutex::new(draft.into()),
            available: AtomicBool::new(true),
            ..Self::default()
        })
    }
    fn draft(&self) -> String {
        self.draft.lock().unwrap().clone()
    }
    fn set_draft(&self, draft: &str) {
        *self.draft.lock().unwrap() = draft.into();
    }
    fn replace_calls(&self) -> usize {
        self.replace_calls.load(Ordering::SeqCst)
    }
}

struct Bound(Arc<Editor>);
struct FocusedEditors {
    editors: Vec<Arc<Editor>>,
    /// `None` models a foreground that is not a supported editable target.
    focus: Mutex<Option<usize>>,
}
impl TargetSelection for FocusedEditors {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let Some(index) = *self.focus.lock().unwrap() else {
            return Ok(None);
        };
        Ok(Some(Box::new(Bound(self.editors[index].clone()))))
    }
}
impl Target for Bound {
    fn content(&mut self) -> AppResult<String> {
        self.0.content_calls.fetch_add(1, Ordering::SeqCst);
        if !self.0.available.load(Ordering::SeqCst) {
            return Err("The original target was destroyed".into());
        }
        Ok(self.0.draft())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        self.0.replace_calls.fetch_add(1, Ordering::SeqCst);
        self.0.replace_entered.store(true, Ordering::SeqCst);
        while self.0.block_replace.load(Ordering::SeqCst)
            && !self.0.replace_release.load(Ordering::SeqCst)
        {
            thread::sleep(Duration::from_millis(1));
        }
        if !self.0.available.load(Ordering::SeqCst) {
            return Err("The original target was destroyed".into());
        }
        if self.0.read_only.load(Ordering::SeqCst) {
            return Err("The original target is read-only".into());
        }
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        let limit = *self.0.limit.lock().unwrap();
        *draft = match limit {
            Some(limit) => replacement.chars().take(limit).collect(),
            None => replacement.into(),
        };
        if limit.is_some() || self.0.lose_response.load(Ordering::SeqCst) {
            return Err("The complete target output could not be confirmed".into());
        }
        Ok(())
    }
}

/// The per-session controls: whether it was stopped and which state it reports
/// while it is alive.
struct Controls {
    stopped: AtomicBool,
    state: Mutex<SessionState>,
}
impl Controls {
    fn set(&self, state: SessionState) {
        *self.state.lock().unwrap() = state;
    }
}

struct Started {
    id: SessionId,
    directory: PathBuf,
    events: mpsc::Sender<SessionEvent>,
    controls: Arc<Controls>,
}

#[derive(Default)]
struct Registry {
    entries: Mutex<Vec<Started>>,
}
impl Registry {
    fn entry(&self, index: usize) -> (SessionId, PathBuf, mpsc::Sender<SessionEvent>) {
        let entries = self.entries.lock().unwrap();
        let entry = entries
            .get(index)
            .unwrap_or_else(|| panic!("no segment {} was started", index + 1));
        (entry.id, entry.directory.clone(), entry.events.clone())
    }
    fn controls(&self, index: usize) -> Arc<Controls> {
        self.entries.lock().unwrap()[index].controls.clone()
    }
    fn count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

struct Sessions {
    registry: Arc<Registry>,
}
impl Sessions {
    fn spawn(&self, request: &SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir(&request.directory).map_err(|error| error.to_string())?;
        fs::write(request.directory.join("audio.wav"), b"captured segment")
            .map_err(|error| error.to_string())?;
        let (events, receiver) = mpsc::channel();
        let controls = Arc::new(Controls {
            stopped: AtomicBool::new(false),
            state: Mutex::new(SessionState::Capturing),
        });
        self.registry.entries.lock().unwrap().push(Started {
            id: request.id,
            directory: request.directory.clone(),
            events,
            controls: controls.clone(),
        });
        Ok(Box::new(ControlledSession {
            events: receiver,
            controls,
        }))
    }
}
impl RecognitionSessions for Sessions {
    fn preparation(&mut self) -> RecognitionReadiness {
        RecognitionReadiness {
            state: listen::Preparation::Ready,
            backend: Some("controlled late-backfill recognition".into()),
            elapsed_ms: None,
            error: None,
        }
    }
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        self.spawn(&request)
    }
    fn retry(
        &mut self,
        request: SessionRequest,
        _audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        self.spawn(&request)
    }
}

struct ControlledSession {
    events: mpsc::Receiver<SessionEvent>,
    controls: Arc<Controls>,
}
impl RecognitionSession for ControlledSession {
    fn stop(&mut self) {
        self.controls.stopped.store(true, Ordering::SeqCst);
    }
    fn status(&self) -> SessionStatus {
        let state = if self.controls.stopped.load(Ordering::SeqCst) {
            SessionState::AwaitingFinal
        } else {
            *self.controls.state.lock().unwrap()
        };
        SessionStatus {
            state,
            progress: Default::default(),
            endpoint: None,
            detail: match state {
                SessionState::WaitingForNetwork => {
                    "Waiting for the network. The received playback is saved on this machine; Listen retries the cloud connection automatically.".into()
                }
                SessionState::AwaitingFinal => {
                    "Waiting for whole-task cloud confirmation; unconfirmed sentences are not delivered."
                        .into()
                }
                _ => "Capturing continuous playback to local PCM before upload.".into(),
            },
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        self.events.try_recv().ok()
    }
}

/// A controllable 精修 port: it records exactly which transcript it was given and
/// returns a recognisable correction without any network.
#[derive(Default)]
struct Refiner {
    texts: Mutex<Vec<String>>,
}
impl Refiner {
    fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap().clone()
    }
}
impl RefineText for Refiner {
    fn availability(&self) -> Result<(), String> {
        Ok(())
    }
    fn refine(&self, text: &str) -> AppResult<String> {
        self.texts.lock().unwrap().push(text.into());
        Ok(format!("精修：{text}"))
    }
}

struct Harness {
    runtime: Runtime,
    app: Application,
    registry: Arc<Registry>,
    editors: Vec<Arc<Editor>>,
    focus: Arc<FocusedEditors>,
    refiner: Arc<Refiner>,
}
impl Harness {
    fn new(drafts: &[&str]) -> Self {
        Self::with_runtime(Runtime::new(), drafts)
    }
    /// Build the application over a prepared runtime directory, so a test can
    /// place an unreadable durable record before the application opens it.
    fn with_runtime(runtime: Runtime, drafts: &[&str]) -> Self {
        let editors: Vec<Arc<Editor>> = drafts.iter().map(|draft| Editor::new(draft)).collect();
        let focus = Arc::new(FocusedEditors {
            editors: editors.clone(),
            focus: Mutex::new(Some(0)),
        });
        let registry = Arc::new(Registry::default());
        let refiner = Arc::new(Refiner::default());
        let app = Application::with_recognition_sessions(
            Box::new(Sessions {
                registry: registry.clone(),
            }),
            runtime.0.clone(),
            Ok(()),
        )
        .with_targets(focus.clone())
        .with_refine(refiner.clone());
        Self {
            runtime,
            app,
            registry,
            editors,
            focus,
            refiner,
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.app.snapshot()
    }
    fn start(&mut self, editor: usize) -> usize {
        *self.focus.focus.lock().unwrap() = Some(editor);
        self.app.dispatch(Control::Start);
        assert_eq!(
            self.snapshot().phase,
            Phase::Capturing,
            "{:?}",
            self.snapshot()
        );
        self.registry.count() - 1
    }
    /// Start a capture while the foreground is not a supported editable target.
    fn start_without_target(&mut self) -> usize {
        *self.focus.focus.lock().unwrap() = None;
        self.app.dispatch(Control::Start);
        assert_eq!(
            self.snapshot().phase,
            Phase::Capturing,
            "{:?}",
            self.snapshot()
        );
        self.registry.count() - 1
    }
    fn focus(&self, editor: Option<usize>) {
        *self.focus.focus.lock().unwrap() = editor;
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
    }
    fn entry(&self, index: usize) -> (SessionId, PathBuf, mpsc::Sender<SessionEvent>) {
        self.registry.entry(index)
    }
    fn controls(&self, index: usize) -> Arc<Controls> {
        self.registry.controls(index)
    }
    fn waiting_for_network(&self, index: usize) {
        self.controls(index).set(SessionState::WaitingForNetwork);
    }
    fn draft(&self, editor: usize) -> String {
        self.editors[editor].draft()
    }
    fn poll_until(&mut self, what: &str, matches: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.app.poll();
            let snapshot = self.app.snapshot();
            if matches(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    /// Wait until the application persisted the draft read when this segment
    /// stopped (or why it could not be read). The test never edits the draft before
    /// that reference exists, so "edited after the stop" is deterministic instead
    /// of racing the reader.
    fn wait_for_stop_draft(&mut self, sequence: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.app.poll();
            if let Some(record) = self.delivery_record(sequence) {
                if record["baseline"].is_string() || record["baseline_problem"].is_string() {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for segment {sequence}'s stop-time draft"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    /// One durable delivery record from `delivery.json`, if the file exists.
    fn delivery_record(&self, sequence: u64) -> Option<serde_json::Value> {
        let bytes = fs::read(self.runtime.0.join("delivery.json")).ok()?;
        let index: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        index["segments"]
            .as_array()?
            .iter()
            .find(|record| record["sequence"] == sequence)
            .cloned()
    }
    fn finish(&self, index: usize, text: &str, recovered: bool) {
        let (id, _, events) = self.entry(index);
        events
            .send(SessionEvent {
                id,
                update: SessionUpdate::Finished(SessionResult {
                    text: text.into(),
                    description: "controlled playback".into(),
                    timing: Timing::default(),
                    stopped_at: None,
                    recovered,
                }),
            })
            .unwrap();
    }
    fn fail(&self, index: usize, message: &str) {
        let (id, directory, events) = self.entry(index);
        events
            .send(SessionEvent {
                id,
                update: SessionUpdate::Failed(SessionFailure {
                    error: message.into(),
                    retry_audio: Some(CapturedAudio {
                        path: directory.join("audio.wav"),
                        description: "controlled playback".into(),
                    }),
                    retry_scope: None,
                    timing: Timing::default(),
                }),
            })
            .unwrap();
    }
    fn runtime(&self) -> &Path {
        &self.runtime.0
    }
}

#[test]
fn a_late_result_appends_to_the_unchanged_draft_of_the_target_bound_at_start() {
    let mut h = Harness::new(&["会议草稿", "另一个输入框"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    // The foreground changes after the stop. The late result must still reach the
    // editor that was bound at this segment's start, never the new focus.
    h.focus(Some(1));
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.text, "迟到但完整的转写。");
    assert_eq!(
        settled.delivery,
        Some(Delivery::Appended),
        "a late result with an intact original draft is appended: {settled:?}"
    );
    assert_eq!(h.draft(0), "会议草稿\n迟到但完整的转写。");
    assert_eq!(h.draft(1), "另一个输入框");
    assert_eq!(
        settled.segments[0].delivery,
        Some(Delivery::Appended),
        "{settled:?}"
    );
    assert_eq!(
        fs::read_to_string(h.runtime().join("current-result.txt")).unwrap(),
        "迟到但完整的转写。"
    );
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "appended", "{record}");
    assert_eq!(record["before"], "会议草稿", "{record}");
    assert_eq!(record["after"], "会议草稿\n迟到但完整的转写。", "{record}");
}

/// How many times one exact text appears in a draft.
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn a_late_result_appends_after_edits_made_during_the_capture() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    // The user keeps writing in the original draft while the capture is running.
    h.editors[0].set_draft("会议草稿\n采集期间写下的内容");
    h.waiting_for_network(first);
    h.stop();
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    assert_eq!(
        h.draft(0),
        "会议草稿\n采集期间写下的内容\n迟到但完整的转写。",
        "edits made before the stop are kept and the result goes after them"
    );
}

#[test]
fn a_late_result_never_appends_after_the_user_rewrote_the_original_draft() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    // The user rewrote the draft after this segment stopped, before its complete
    // result existed. That later text is never touched.
    h.editors[0].set_draft("会议草稿\n用户后来补充的内容");
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(
        h.draft(0),
        "会议草稿\n用户后来补充的内容",
        "a user rewrite after the stop is never appended to: {settled:?}"
    );
    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(
        settled.text, "迟到但完整的转写。",
        "the complete text stays available"
    );
    assert!(
        settled
            .detail
            .contains("changed after this segment stopped"),
        "{settled:?}"
    );
    assert_eq!(
        fs::read_to_string(h.runtime().join("segment-00000001.txt")).unwrap(),
        "迟到但完整的转写。"
    );
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "fallback", "{record}");
    assert_eq!(record["baseline"], "会议草稿", "{record}");
}

#[test]
fn the_tools_own_confirmed_append_is_not_mistaken_for_a_user_edit() {
    let mut h = Harness::new(&["会议草稿"]);
    // Segment 1 keeps waiting after its stop. While it waits, segment 2 captures
    // and completes normally on the very same target, appending its own result.
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    let second = h.start(0);
    h.stop();
    h.finish(second, "第二段完整结果。", false);
    let second_done = h.poll_until("segment 2's result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(
        second_done.delivery,
        Some(Delivery::Appended),
        "{second_done:?}"
    );
    assert_eq!(h.draft(0), "会议草稿\n第二段完整结果。");

    // Segment 1's late result arrives afterwards. The target changed since its own
    // stop, but only because Listen appended segment 2's confirmed result: that is
    // not a user edit, so the older result is appended after it exactly once.
    h.finish(first, "第一段完整结果。", true);
    let settled = h.poll_until("segment 1's late result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && !segment.text.is_empty())
    });
    assert_eq!(
        h.draft(0),
        "会议草稿\n第二段完整结果。\n第一段完整结果。",
        "the tool's own appended result is not a user rewrite: {settled:?}"
    );
    assert_eq!(
        settled
            .segments
            .iter()
            .find(|segment| segment.sequence == 1)
            .and_then(|segment| segment.delivery),
        Some(Delivery::Appended),
        "{settled:?}"
    );
    assert_eq!(
        settled
            .segments
            .iter()
            .find(|segment| segment.sequence == 2)
            .and_then(|segment| segment.delivery),
        Some(Delivery::Appended),
        "{settled:?}"
    );
}

#[test]
fn a_user_edit_between_the_tools_own_appends_keeps_the_late_result_pending() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    // The user edits the draft first; a later normal append then sits on top of
    // that edit. The older late result can no longer prove its draft is intact.
    h.editors[0].set_draft("会议草稿\n用户补充");
    let second = h.start(0);
    h.stop();
    h.finish(second, "第二段完整结果。", false);
    h.poll_until("segment 2's result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    h.finish(first, "第一段完整结果。", true);
    let settled = h.poll_until("segment 1's pending result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && !segment.text.is_empty())
    });

    assert_eq!(
        h.draft(0),
        "会议草稿\n用户补充\n第二段完整结果。",
        "the user's text and the later confirmed append are both preserved: {settled:?}"
    );
    assert_eq!(
        settled
            .segments
            .iter()
            .find(|segment| segment.sequence == 1)
            .and_then(|segment| segment.delivery),
        Some(Delivery::Fallback),
        "{settled:?}"
    );
}

#[test]
fn a_destroyed_original_target_keeps_the_complete_late_result_pending() {
    let mut h = Harness::new(&["会议草稿", "另一个输入框"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    h.editors[0].available.store(false, Ordering::SeqCst);
    h.focus(Some(1));
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(settled.text, "迟到但完整的转写。");
    assert!(settled.detail.contains("destroyed"), "{settled:?}");
    assert_eq!(
        h.draft(1),
        "另一个输入框",
        "never retargeted to the new focus"
    );
    assert_eq!(
        settled.segments[0].delivery,
        Some(Delivery::Fallback),
        "{settled:?}"
    );
    assert_eq!(
        fs::read_to_string(h.runtime().join("segment-00000001.txt")).unwrap(),
        "迟到但完整的转写。"
    );
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "fallback", "{record}");
}

#[test]
fn a_target_that_became_read_only_keeps_the_complete_late_result_pending() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    h.editors[0].read_only.store(true, Ordering::SeqCst);
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(h.draft(0), "会议草稿");
    assert!(settled.detail.contains("read-only"), "{settled:?}");
    assert_eq!(
        h.editors[0].replace_calls(),
        1,
        "the refused write is never retried; a repeated event must not write again"
    );
    // A repeated stop and further polls change nothing.
    h.finish(first, "迟到但完整的转写。", true);
    h.stop();
    for _ in 0..20 {
        h.app.poll();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.editors[0].replace_calls(), 1);
    assert_eq!(h.draft(0), "会议草稿");
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "fallback", "{record}");
}

#[test]
fn a_stop_time_draft_that_cannot_be_read_keeps_the_complete_late_result_pending() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.editors[0].available.store(false, Ordering::SeqCst);
    h.stop();
    h.wait_for_stop_draft(1);
    // The control is readable again, but the stop-time reference was never read:
    // that can no longer be proven, so nothing is written.
    h.editors[0].available.store(true, Ordering::SeqCst);
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(h.draft(0), "会议草稿");
    assert_eq!(
        h.editors[0].replace_calls(),
        0,
        "an unprovable draft never leads to a write"
    );
    assert!(settled.detail.contains("destroyed"), "{settled:?}");
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "fallback", "{record}");
    assert_eq!(record["baseline"], serde_json::Value::Null, "{record}");
}

#[test]
fn a_lost_write_response_is_recorded_uncertain_and_never_appended_twice() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    // The editor applies the complete write but its response is lost.
    h.editors[0].lose_response.store(true, Ordering::SeqCst);
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    // The target actually holds the complete append, but Listen cannot claim a
    // confirmed delivery, so the result stays available as a fallback.
    assert_eq!(settled.delivery, Some(Delivery::Fallback), "{settled:?}");
    assert_eq!(h.draft(0), "会议草稿\n迟到但完整的转写。");
    assert_eq!(
        occurrences(&h.draft(0), "迟到但完整的转写。"),
        1,
        "a lost response is not written again"
    );
    assert!(
        settled
            .detail
            .contains("now contains the complete appended text"),
        "{settled:?}"
    );
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "uncertain", "{record}");

    // A repeated cloud event, a repeated stop and repeated polls cannot append it
    // again; the complete text is still in the panel.
    h.editors[0].lose_response.store(false, Ordering::SeqCst);
    h.finish(first, "迟到但完整的转写。", true);
    h.stop();
    for _ in 0..20 {
        h.app.poll();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.editors[0].replace_calls(), 1);
    assert_eq!(occurrences(&h.draft(0), "迟到但完整的转写。"), 1);
    assert_eq!(h.snapshot().text, "迟到但完整的转写。");
}

#[test]
fn a_truncated_late_write_is_not_a_complete_delivery_and_keeps_the_full_text() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    *h.editors[0].limit.lock().unwrap() = Some(6);
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    let draft = h.draft(0);
    assert_eq!(settled.delivery, Some(Delivery::Fallback), "{settled:?}");
    assert!(draft.starts_with("会议草稿\n迟"), "{draft:?}");
    assert!(
        draft.chars().count() < "会议草稿\n迟到但完整的转写。".chars().count(),
        "the target holds only a prefix: {draft:?}"
    );
    assert!(settled.detail.contains("truncated"), "{settled:?}");
    assert_eq!(
        settled.text, "迟到但完整的转写。",
        "the complete text is never truncated in the panel"
    );
    assert_eq!(
        fs::read_to_string(h.runtime().join("segment-00000001.txt")).unwrap(),
        "迟到但完整的转写。"
    );
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "uncertain", "{record}");
}

#[test]
fn a_late_result_with_no_original_target_stays_pending_and_never_uses_a_later_focus() {
    let mut h = Harness::new(&["另一个输入框"]);
    let first = h.start_without_target();
    h.stop();
    // A supported editor becomes the foreground later; it is not this segment's
    // target and must never receive its text.
    h.focus(Some(0));
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(h.draft(0), "另一个输入框");
    assert_eq!(
        h.editors[0].replace_calls(),
        0,
        "no target was bound, so no control may be written"
    );
    assert_eq!(settled.text, "迟到但完整的转写。");
    assert!(
        settled.detail.contains("no supported editable target"),
        "{settled:?}"
    );
}

#[test]
fn duplicate_late_events_stops_and_polls_never_append_the_same_result_twice() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.finish(first, "一次性结果。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    assert_eq!(h.draft(0), "会议草稿\n一次性结果。");
    assert_eq!(h.editors[0].replace_calls(), 1);

    // A duplicate confirmation, a late failure and repeated controls after the
    // result must not append it again.
    h.finish(first, "一次性结果。", true);
    h.fail(first, "A late failure from the already delivered segment");
    h.stop();
    h.app.dispatch(Control::CleanupAudio);
    for _ in 0..30 {
        h.app.poll();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        occurrences(&h.draft(0), "一次性结果。"),
        1,
        "one segment is appended at most once"
    );
    assert_eq!(h.editors[0].replace_calls(), 1);
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "appended", "{record}");
}

#[test]
fn the_delivery_start_is_persisted_before_the_target_write_and_a_slow_editor_never_blocks_poll() {
    let mut h = Harness::new(&["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.wait_for_stop_draft(1);
    h.editors[0].block_replace.store(true, Ordering::SeqCst);
    h.finish(first, "迟到但完整的转写。", true);

    // While the editor holds the write, the durable record already says a write
    // started — that is what makes a lost response undecidable instead of a second
    // append. The application thread keeps answering polls meanwhile.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.app.poll();
        if h.delivery_record(1)
            .is_some_and(|record| record["state"] == "started")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the delivery start was never recorded"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(h.draft(0), "会议草稿", "the blocked write is not complete");
    assert_eq!(
        h.snapshot().phase,
        Phase::Processing,
        "a slow editor never blocks the application poll"
    );

    h.editors[0].replace_release.store(true, Ordering::SeqCst);
    let settled = h.poll_until("the completed late delivery", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    assert_eq!(h.draft(0), "会议草稿\n迟到但完整的转写。");
    let record = h.delivery_record(1).expect("a durable delivery record");
    assert_eq!(record["state"], "appended", "{record}");
}

#[test]
fn an_unreadable_delivery_record_keeps_the_complete_late_result_pending() {
    let runtime = Runtime::new();
    let path = runtime.0.clone();
    // A record from a future version is never silently rewritten; without it the
    // safety checks cannot prove anything, so a late result must stay pending.
    fs::write(
        path.join("delivery.json"),
        r#"{"version":99,"segments":[]}"#,
    )
    .unwrap();
    let mut h = Harness::with_runtime(runtime, &["会议草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    h.finish(first, "迟到但完整的转写。", true);
    let settled = h.poll_until("the late result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    assert_eq!(settled.delivery, Some(Delivery::Fallback), "{settled:?}");
    assert_eq!(h.draft(0), "会议草稿");
    assert_eq!(h.editors[0].replace_calls(), 0);
    assert!(
        settled.detail.contains("unsupported version"),
        "{settled:?}"
    );
    assert_eq!(
        settled.text, "迟到但完整的转写。",
        "the complete text is still available"
    );
    assert_eq!(
        fs::read(path.join("delivery.json")).unwrap(),
        br#"{"version":99,"segments":[]}"#,
        "a future record is never downgraded"
    );
}

#[test]
fn a_late_result_of_an_older_segment_never_borrows_the_current_results_refine_or_target_memory() {
    let mut h = Harness::new(&["草稿甲", "草稿乙"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.stop();
    let second = h.start(1);
    h.stop();
    h.finish(second, "第二段完整结果。", false);
    h.poll_until("segment 2's result", |snapshot| {
        snapshot.phase == Phase::Result
    });

    // The older late result is appended to its own target while segment 2 is the
    // current result. It must not take over the current result or its delivery
    // memory, which refine needs.
    h.finish(first, "第一段完整结果。", true);
    let after_late = h.poll_until("segment 1's late result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.delivery.is_some())
    });
    assert_eq!(h.draft(0), "草稿甲\n第一段完整结果。");
    assert_eq!(
        after_late.text, "第二段完整结果。",
        "the older late delivery never becomes the current result: {after_late:?}"
    );
    assert!(
        matches!(after_late.refine, Refine::Available),
        "{after_late:?}"
    );

    h.app.dispatch(Control::Refine);
    let settled = h.poll_until("the refine", |snapshot| {
        matches!(
            snapshot.refine,
            Refine::Applied | Refine::NotApplied(_) | Refine::Failed(_)
        )
    });
    assert_eq!(
        h.refiner.texts(),
        vec!["第二段完整结果。".to_string()],
        "refine receives exactly the current segment's transcript"
    );
    assert!(matches!(settled.refine, Refine::Applied), "{settled:?}");
    assert_eq!(
        h.draft(1),
        "草稿乙\n精修：第二段完整结果。",
        "the current result's target is the one refine writes"
    );
    assert_eq!(
        h.draft(0),
        "草稿甲\n第一段完整结果。",
        "the older segment's target is never touched by the refine"
    );
}

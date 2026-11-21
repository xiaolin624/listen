//! Multi-segment capture workflows through the same application interface as the
//! global hotkey: every start/stop is an independent task, a new capture is never
//! refused because an older segment still waits, and each segment's text, target
//! and pending result stay its own.
//!
//! Only the playback device, the recognition sessions and the editors are
//! substituted. Every assertion is a user-visible state, complete text, target
//! content or on-disk record observed through `Application::dispatch`, `poll` and
//! `snapshot`; no paid network is contacted and no thread count is asserted.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, SegmentState, Snapshot,
    Target, TargetSelection, Timing,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SegmentUsage, SessionEvent,
        SessionFailure, SessionId, SessionRequest, SessionResult, SessionState, SessionStatus,
        SessionUpdate,
    },
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-multi-segment-{}-{}-{}",
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

struct Editor {
    draft: Mutex<String>,
}
struct Bound(Arc<Editor>);
struct FocusedEditors {
    editors: Vec<Arc<Editor>>,
    focus: Mutex<usize>,
}
impl TargetSelection for FocusedEditors {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let index = *self.focus.lock().unwrap();
        Ok(Some(Box::new(Bound(self.editors[index].clone()))))
    }
}
impl Target for Bound {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.draft.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

/// Per-session controls the test drives: whether the capture was stopped and which
/// state the recognition adapter reports while it is alive.
struct Controls {
    stopped: AtomicBool,
    state: Mutex<SessionState>,
}
impl Controls {
    fn new(state: SessionState) -> Arc<Self> {
        Arc::new(Self {
            stopped: AtomicBool::new(false),
            state: Mutex::new(state),
        })
    }
    fn set(&self, state: SessionState) {
        *self.state.lock().unwrap() = state;
    }
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
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
    usage: Mutex<HashMap<String, SegmentUsage>>,
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
    fn set_usage(&self, directory: &Path, usage: SegmentUsage) {
        self.usage.lock().unwrap().insert(
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            usage,
        );
    }
}

struct Sessions {
    registry: Arc<Registry>,
}
impl Sessions {
    fn spawn(
        &self,
        request: &SessionRequest,
        state: SessionState,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir(&request.directory).map_err(|error| error.to_string())?;
        fs::write(request.directory.join("audio.wav"), b"captured segment")
            .map_err(|error| error.to_string())?;
        let (events, receiver) = mpsc::channel();
        let controls = Controls::new(state);
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
            backend: Some("controlled multi-segment recognition".into()),
            elapsed_ms: None,
            error: None,
        }
    }
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        self.spawn(&request, SessionState::Capturing)
    }
    fn retry(
        &mut self,
        request: SessionRequest,
        _audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        self.spawn(&request, SessionState::AwaitingFinal)
    }
    fn segment_usage(&self, directory: &Path) -> Option<SegmentUsage> {
        self.registry
            .usage
            .lock()
            .unwrap()
            .get(&directory.file_name()?.to_string_lossy().into_owned())
            .cloned()
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
            self.controls.state()
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

struct Harness {
    runtime: Runtime,
    app: Application,
    registry: Arc<Registry>,
    editors: Vec<Arc<Editor>>,
    focus: Arc<FocusedEditors>,
}
impl Harness {
    fn new(drafts: &[&str]) -> Self {
        let runtime = Runtime::new();
        let editors: Vec<Arc<Editor>> = drafts
            .iter()
            .map(|draft| {
                Arc::new(Editor {
                    draft: Mutex::new((*draft).into()),
                })
            })
            .collect();
        let focus = Arc::new(FocusedEditors {
            editors: editors.clone(),
            focus: Mutex::new(0),
        });
        let registry = Arc::new(Registry::default());
        let app = Application::with_recognition_sessions(
            Box::new(Sessions {
                registry: registry.clone(),
            }),
            runtime.0.clone(),
            Ok(()),
        )
        .with_targets(focus.clone());
        Self {
            runtime,
            app,
            registry,
            editors,
            focus,
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.app.snapshot()
    }
    /// Focus one editor and press Start. Returns the index of the segment just
    /// created, so the test can address its events and its persisted directory.
    fn start(&mut self, editor: usize) -> usize {
        *self.focus.focus.lock().unwrap() = editor;
        self.app.dispatch(Control::Start);
        assert_eq!(
            self.snapshot().phase,
            Phase::Capturing,
            "{:?}",
            self.snapshot()
        );
        self.registry.count() - 1
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
    fn draft(&self, editor: usize) -> String {
        self.editors[editor].draft.lock().unwrap().clone()
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
    fn poll_settled(&mut self) -> Snapshot {
        // A few polls let every in-flight completion reach the snapshot; the test
        // still asserts only observable state, never how many polls it took.
        for _ in 0..12 {
            self.app.poll();
            thread::sleep(Duration::from_millis(2));
        }
        self.app.snapshot()
    }
    /// Exactly one application poll, for asserting how much work one poll accepts.
    fn poll_once(&mut self) -> Snapshot {
        self.app.poll();
        self.app.snapshot()
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
        self.fail_with_audio(index, message, None);
    }
    /// Fail one segment, optionally keeping its captured audio for an explicit retry.
    fn fail_with_audio(&self, index: usize, message: &str, retry_audio: Option<bool>) {
        let (id, directory, events) = self.entry(index);
        let retry_audio = retry_audio.map(|_| CapturedAudio {
            path: directory.join("audio.wav"),
            description: "controlled playback".into(),
        });
        events
            .send(SessionEvent {
                id,
                update: SessionUpdate::Failed(SessionFailure {
                    error: message.into(),
                    retry_audio,
                    retry_scope: None,
                    timing: Timing::default(),
                }),
            })
            .unwrap();
    }
    fn waiting_for_network(&self, index: usize) {
        self.controls(index).set(SessionState::WaitingForNetwork);
    }
    fn runtime(&self) -> &Path {
        &self.runtime.0
    }
}

#[test]
fn a_new_capture_starts_while_the_previous_segment_waits_and_each_segment_keeps_its_own_target_and_text()
 {
    let mut h = Harness::new(&["第一段草稿", "第二段草稿"]);
    let first = h.start(0);
    h.waiting_for_network(first);
    h.poll_until("the waiting segment", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.state == SegmentState::WaitingForNetwork)
    });
    h.stop();
    // The first segment has not produced a complete result yet. A new capture must
    // still be allowed: one waiting segment cannot block the next meeting sentence.
    assert_eq!(h.snapshot().phase, Phase::Processing);
    let second = h.start(1);
    assert_eq!(h.snapshot().phase, Phase::Capturing);

    // The first segment's late result arrives while the second is capturing. It is
    // a recovered (late) result, so it must never be written to its target, and it
    // must not change the live capture's phase. It is the newest complete result, so
    // it is shown as the current result until the live segment finishes.
    h.finish(first, "第一段完整结果。", true);
    let waiting = h.poll_until("the first segment's pending result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.text == "第一段完整结果。")
    });
    assert_eq!(waiting.phase, Phase::Capturing, "{waiting:?}");
    assert_eq!(h.draft(0), "第一段草稿", "a late result is never appended");
    assert_eq!(waiting.text, "第一段完整结果。");
    assert_eq!(waiting.delivery, Some(Delivery::Fallback), "{waiting:?}");
    assert!(
        waiting
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.delivery == Some(Delivery::Fallback)),
        "{waiting:?}"
    );

    // The new capture completes normally: it is appended to the target bound at its
    // own start, and it becomes the current result without deleting the older one.
    h.stop();
    h.finish(second, "第二段完整结果。", false);
    let result = h.poll_until("the second segment's result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(result.text, "第二段完整结果。");
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert_eq!(
        h.draft(0),
        "第一段草稿",
        "no segment writes another's target"
    );
    assert_eq!(h.draft(1), "第二段草稿\n第二段完整结果。");
    assert_eq!(result.segments.len(), 2, "{result:?}");
    assert_eq!(result.segments[0].sequence, 1);
    assert_eq!(
        result.segments[0].text, "第一段完整结果。",
        "a later result must not erase the earlier pending result: {result:?}"
    );
    assert_eq!(result.segments[1].sequence, 2);
    assert_eq!(result.segments[1].text, "第二段完整结果。");
}

#[test]
fn three_interleaved_captures_keep_their_own_state_and_the_newest_result_is_current() {
    let mut h = Harness::new(&["一段草稿", "二段草稿", "三段草稿"]);
    // Segment 1 waits for the network and is stopped while it does.
    let first = h.start(0);
    h.waiting_for_network(first);
    h.poll_until("segment 1 waiting", |snapshot| {
        snapshot.segments.iter().any(|segment| {
            segment.sequence == 1 && segment.state == SegmentState::WaitingForNetwork
        })
    });
    h.stop();
    assert_eq!(h.snapshot().phase, Phase::Processing);

    // Segment 2 starts while segment 1 is still processing, and is stopped too.
    let second = h.start(1);
    h.waiting_for_network(second);
    h.poll_until("segment 2 waiting", |snapshot| {
        snapshot.segments.iter().any(|segment| {
            segment.sequence == 2 && segment.state == SegmentState::WaitingForNetwork
        })
    });
    h.stop();
    assert_eq!(h.snapshot().phase, Phase::Processing);

    // Segment 3 starts while the two older segments still wait for the network.
    let third = h.start(2);
    let captured = h.snapshot();
    assert_eq!(captured.phase, Phase::Capturing, "{captured:?}");
    assert_eq!(captured.segments.len(), 3, "{captured:?}");
    assert_eq!(
        captured
            .segments
            .iter()
            .map(|segment| segment.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the panel keeps capture order: {captured:?}"
    );
    assert_eq!(captured.segments[0].state, SegmentState::Processing);
    assert_eq!(captured.segments[1].state, SegmentState::Processing);
    assert_eq!(captured.segments[2].state, SegmentState::Capturing);
    assert!(
        captured
            .segments
            .iter()
            .all(|segment| segment.text.is_empty()),
        "no unfinished segment claims text: {captured:?}"
    );

    // The backlog recovers out of order after the newest capture is stopped. Every
    // complete result is kept, and only the newest capture order is current.
    h.stop();
    h.finish(second, "第二段完整结果。", true);
    h.finish(first, "第一段完整结果。", true);
    h.poll_until("both old results", |snapshot| {
        snapshot
            .segments
            .iter()
            .filter(|segment| !segment.text.is_empty())
            .count()
            == 2
    });
    h.finish(third, "第三段完整结果。", true);
    let settled = h.poll_until("all three results", |snapshot| {
        snapshot
            .segments
            .iter()
            .all(|segment| !segment.text.is_empty())
    });
    assert_eq!(settled.segments.len(), 3, "{settled:?}");
    assert_eq!(settled.segments[0].text, "第一段完整结果。");
    assert_eq!(settled.segments[1].text, "第二段完整结果。");
    assert_eq!(settled.segments[2].text, "第三段完整结果。");
    assert_eq!(
        settled.text, "第三段完整结果。",
        "the newest capture order is the current result: {settled:?}"
    );
    assert_eq!(settled.delivery, Some(Delivery::Fallback), "{settled:?}");
    // Every recovered result stays conservative: no target is written at all.
    assert_eq!(h.draft(0), "一段草稿");
    assert_eq!(h.draft(1), "二段草稿");
    assert_eq!(h.draft(2), "三段草稿");
}

#[test]
fn a_failure_in_an_old_segment_does_not_end_the_new_capture_or_clear_other_results() {
    let mut h = Harness::new(&["一段草稿", "二段草稿", "三段草稿"]);
    // Segment 1 produces a pending (recovered) result that must survive everything.
    let first = h.start(0);
    h.stop();
    h.finish(first, "第一段待取结果。", true);
    h.poll_until("segment 1 result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && !segment.text.is_empty())
    });

    // Segment 2 is stopped and then fails while segment 3 captures.
    let second = h.start(1);
    h.stop();
    h.poll_until("segment 2 processing", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    let third = h.start(2);
    h.fail_with_audio(
        second,
        "The old segment lost the network for good",
        Some(true),
    );
    let isolated = h.poll_until("segment 2 failure", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 2 && segment.state == SegmentState::Failed)
    });
    assert_eq!(
        isolated.phase,
        Phase::Capturing,
        "an old failure must not end the new capture: {isolated:?}"
    );
    assert_eq!(
        isolated.recognition,
        Some(SessionState::Capturing),
        "{isolated:?}"
    );
    assert_eq!(
        isolated.segments[0].text, "第一段待取结果。",
        "another segment's pending result must not be cleared: {isolated:?}"
    );
    assert!(isolated.retry_available, "{isolated:?}");
    assert!(
        isolated.retained_audio.iter().all(|path| path.is_file()),
        "the failed segment's audio is retained: {isolated:?}"
    );

    // Segment 3 completes normally; the pending result and the failed audio stay.
    h.stop();
    h.finish(third, "第三段完整结果。", false);
    let settled = h.poll_until("segment 3 result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(settled.text, "第三段完整结果。");
    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    assert_eq!(settled.segments[0].text, "第一段待取结果。");
    assert_eq!(settled.segments[1].state, SegmentState::Failed);
    assert!(
        settled.segments[1].text.is_empty(),
        "a failed segment never claims a complete result: {settled:?}"
    );
    assert!(
        settled.retained_audio.iter().all(|path| path.is_file()),
        "the old failure's audio must survive the new result: {settled:?}"
    );
    assert_eq!(h.draft(0), "一段草稿");
    assert_eq!(h.draft(1), "二段草稿");
    assert_eq!(h.draft(2), "三段草稿\n第三段完整结果。");
}

#[test]
fn every_segment_saves_its_own_audio_text_state_and_cost_without_overwriting_an_earlier_one() {
    let mut h = Harness::new(&["草稿一", "草稿二", "草稿三", "草稿四"]);
    let first = h.start(0);
    let (_, first_directory, _) = h.entry(first);
    h.registry.set_usage(
        &first_directory,
        SegmentUsage {
            provider: "Alibaba Model Studio".into(),
            model: "controlled".into(),
            region: "cn-beijing".into(),
            requests: 2,
            estimated_nanoyuan: None,
            unknown: true,
        },
    );
    h.stop();
    let second = h.start(1);
    let (_, second_directory, _) = h.entry(second);
    h.registry.set_usage(
        &second_directory,
        SegmentUsage {
            provider: "Alibaba Model Studio".into(),
            model: "controlled".into(),
            region: "cn-beijing".into(),
            requests: 1,
            estimated_nanoyuan: Some(1_500),
            unknown: false,
        },
    );
    h.stop();
    let third = h.start(2);
    h.stop();

    h.finish(second, "第二段待取。", true);
    h.finish(first, "第一段待取。", true);
    h.finish(third, "第三段已交付。", false);
    let settled = h.poll_until("all three results", |snapshot| {
        snapshot.phase == Phase::Result
            && snapshot
                .segments
                .iter()
                .filter(|segment| !segment.text.is_empty())
                .count()
                == 3
    });
    assert_eq!(settled.text, "第三段已交付。", "{settled:?}");

    // Each pending result has its own durable text file; the delivered one was
    // drained after its complete text was saved to current-result.txt.
    let runtime = h.runtime().to_path_buf();
    assert_eq!(
        fs::read_to_string(runtime.join("segment-00000001.txt")).unwrap(),
        "第一段待取。"
    );
    assert_eq!(
        fs::read_to_string(runtime.join("segment-00000002.txt")).unwrap(),
        "第二段待取。"
    );
    assert_eq!(
        fs::read_to_string(runtime.join("current-result.txt")).unwrap(),
        "第三段已交付。"
    );
    assert!(
        !runtime.join("segment-00000003.txt").exists(),
        "an appended result is delivered, so its pending copy is drained"
    );

    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(runtime.join("segments.json")).unwrap()).unwrap();
    let records = index["segments"].as_array().expect("segment records");
    assert_eq!(records.len(), 2, "{index}");
    let texts: Vec<String> = records
        .iter()
        .map(|record| {
            fs::read_to_string(runtime.join(record["text_file"].as_str().unwrap())).unwrap()
        })
        .collect();
    assert!(texts.iter().any(|text| text == "第一段待取。"), "{texts:?}");
    assert!(texts.iter().any(|text| text == "第二段待取。"), "{texts:?}");
    assert!(
        records
            .iter()
            .all(|record| record["delivery"] == "fallback"),
        "{index}"
    );
    let directories: std::collections::HashSet<&str> = records
        .iter()
        .map(|record| record["directory"].as_str().unwrap())
        .collect();
    assert_eq!(
        directories.len(),
        records.len(),
        "each segment keeps its own capture directory: {index}"
    );
    // Cost is persisted per segment and never invented as zero for an unknown part.
    let first_usage = settled
        .segments
        .iter()
        .find(|segment| segment.sequence == 1)
        .and_then(|segment| segment.usage.as_ref())
        .expect("segment 1 usage");
    assert_eq!(first_usage.requests, 2);
    assert!(first_usage.unknown);
    assert_eq!(first_usage.estimated_nanoyuan, None);
    let second_usage = settled
        .segments
        .iter()
        .find(|segment| segment.sequence == 2)
        .and_then(|segment| segment.usage.as_ref())
        .expect("segment 2 usage");
    assert_eq!(second_usage.requests, 1);
    assert!(!second_usage.unknown);
    assert_eq!(second_usage.estimated_nanoyuan, Some(1_500));
    let first_record = records
        .iter()
        .find(|record| record["sequence"] == 1)
        .expect("segment 1 record");
    assert_eq!(first_record["usage"]["requests"], 2, "{index}");
    assert_eq!(first_record["usage"]["unknown"], true, "{index}");
    assert_eq!(
        first_record["usage"]["estimated_nanoyuan"],
        serde_json::Value::Null,
        "{index}"
    );

    // A fourth segment continues the capture order without touching older records.
    let fourth = h.start(3);
    let (_, _, _) = h.entry(fourth);
    let after = h.snapshot();
    assert_eq!(
        after
            .segments
            .iter()
            .map(|segment| segment.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(runtime.join("segments.json")).unwrap()).unwrap();
    assert_eq!(
        index["next_sequence"], 5,
        "the capture order is persisted: {index}"
    );
    assert!(
        index["segments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["sequence"] == 1),
        "an older pending result must not be overwritten by a later segment: {index}"
    );
}

#[test]
fn a_finished_backlog_is_recovered_one_segment_at_a_time_in_capture_order() {
    let mut h = Harness::new(&["草稿一", "草稿二", "草稿三"]);
    let first = h.start(0);
    h.stop();
    let second = h.start(1);
    h.stop();
    let third = h.start(2);
    h.stop();
    assert_eq!(h.snapshot().phase, Phase::Processing);

    // All three segments become complete at once, delivered newest first. The
    // application must not start three recoveries at the same time: the oldest
    // capture is handled first, then the next, so the backlog cannot occupy the
    // workers or the target delivery that the current capture needs.
    h.finish(third, "第三段完整结果。", true);
    h.finish(second, "第二段完整结果。", true);
    h.finish(first, "第一段完整结果。", true);
    let runtime = h.runtime().to_path_buf();
    h.poll_once();
    assert!(
        runtime.join("segment-00000001.txt").is_file(),
        "the oldest complete segment is saved first"
    );
    assert!(
        !runtime.join("segment-00000002.txt").exists(),
        "a later segment must not overtake the older backlog"
    );
    h.poll_once();
    assert!(
        runtime.join("segment-00000002.txt").is_file(),
        "the second segment follows in capture order"
    );
    assert!(
        !runtime.join("segment-00000003.txt").exists(),
        "still one at a time"
    );

    let settled = h.poll_until("the whole backlog", |snapshot| {
        snapshot
            .segments
            .iter()
            .all(|segment| !segment.text.is_empty())
    });
    assert_eq!(settled.segments[0].text, "第一段完整结果。");
    assert_eq!(settled.segments[1].text, "第二段完整结果。");
    assert_eq!(settled.segments[2].text, "第三段完整结果。");
    assert_eq!(
        settled.text, "第三段完整结果。",
        "the newest capture order is still the current result"
    );
}

#[test]
fn duplicate_and_foreign_events_cannot_change_another_segments_result() {
    let mut h = Harness::new(&["草稿甲", "草稿乙"]);
    let first = h.start(0);
    h.stop();
    let second = h.start(1);
    h.stop();
    assert_eq!(h.snapshot().phase, Phase::Processing);

    // A duplicate confirmation and a late failure from segment 1 must not deliver
    // or change anything while segment 2 is still processing.
    h.finish(first, "第一段完整结果。", true);
    h.finish(first, "第一段重复结果。", true);
    h.fail(first, "A late failure from the finished segment");
    let after_old_events = h.poll_until("segment 1 result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && !segment.text.is_empty())
    });
    assert_eq!(after_old_events.segments[0].text, "第一段完整结果。");
    assert_eq!(
        after_old_events.segments[0].state,
        SegmentState::Result,
        "a late failure of a completed segment is ignored: {after_old_events:?}"
    );
    assert_eq!(after_old_events.phase, Phase::Processing);

    // An event carrying segment 1's identity through segment 2's channel is foreign
    // and must be ignored; then segment 2's own confirmation applies once.
    let (first_id, _, _) = h.entry(first);
    let (_, _, second_events) = h.entry(second);
    second_events
        .send(SessionEvent {
            id: first_id,
            update: SessionUpdate::Finished(SessionResult {
                text: "串到第二段的文字".into(),
                description: "controlled playback".into(),
                timing: Timing::default(),
                stopped_at: None,
                recovered: false,
            }),
        })
        .unwrap();
    let still_processing = h.poll_settled();
    assert_eq!(
        still_processing.segments[1].text, "",
        "a foreign event must not fill another segment: {still_processing:?}"
    );
    assert_eq!(still_processing.phase, Phase::Processing);

    h.finish(second, "第二段完整结果。", false);
    let settled = h.poll_until("segment 2 result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(settled.text, "第二段完整结果。");
    assert_eq!(settled.segments[1].text, "第二段完整结果。");
    assert_eq!(settled.segments[0].text, "第一段完整结果。");
    assert_eq!(h.draft(0), "草稿甲", "the recovered result wrote no target");
    assert_eq!(h.draft(1), "草稿乙\n第二段完整结果。");
    // The duplicate text never appeared anywhere.
    assert_ne!(settled.text, "第一段重复结果。");
}

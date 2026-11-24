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
        SegmentUsage, SessionEvent, SessionFailure, SessionId, SessionResult, SessionState,
        SessionUpdate,
    },
    testing::{ScriptedControl, ScriptedSessions},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering, mpsc},
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
    /// When set, a target write waits until the test releases it. Only the backlog
    /// ordering test uses this, so a save can be observed before its delivery
    /// removes the durable record.
    blocked: Mutex<bool>,
    released: std::sync::Condvar,
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
        self.0.wait_for_test_release();
        Ok(self.0.draft.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        self.0.wait_for_test_release();
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}
impl Editor {
    fn new(draft: &str) -> Self {
        Self {
            draft: Mutex::new(draft.into()),
            blocked: Mutex::new(false),
            released: std::sync::Condvar::new(),
        }
    }
    fn set_writes_blocked(&self, blocked: bool) {
        *self.blocked.lock().unwrap() = blocked;
        if !blocked {
            self.released.notify_all();
        }
    }
    fn wait_for_test_release(&self) {
        let mut blocked = self.blocked.lock().unwrap();
        while *blocked {
            blocked = self.released.wait(blocked).unwrap();
        }
    }
}

struct Harness {
    runtime: Runtime,
    app: Application,
    script: ScriptedControl,
    editors: Vec<Arc<Editor>>,
    focus: Arc<FocusedEditors>,
}
impl Harness {
    fn new(drafts: &[&str]) -> Self {
        let runtime = Runtime::new();
        let editors: Vec<Arc<Editor>> = drafts
            .iter()
            .map(|draft| Arc::new(Editor::new(draft)))
            .collect();
        let focus = Arc::new(FocusedEditors {
            editors: editors.clone(),
            focus: Mutex::new(0),
        });
        let adapter = ScriptedSessions::new().with_backend("controlled multi-segment recognition");
        let script = adapter.control();
        let app =
            Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
                .with_targets(focus.clone());
        Self {
            runtime,
            app,
            script,
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
        self.script.count() - 1
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
    }
    fn entry(&self, index: usize) -> (SessionId, PathBuf, mpsc::Sender<SessionEvent>) {
        let session = self.script.session(index);
        (session.id(), session.directory(), session.sender())
    }
    fn draft(&self, editor: usize) -> String {
        self.editors[editor].draft.lock().unwrap().clone()
    }
    /// Hold every test target's read/write at the delivery boundary until released.
    fn set_writes_blocked(&self, blocked: bool) {
        for editor in &self.editors {
            editor.set_writes_blocked(blocked);
        }
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
        self.script
            .session(index)
            .set_state(SessionState::WaitingForNetwork);
    }
    fn runtime(&self) -> &Path {
        &self.runtime.0
    }
    /// Wait until the draft read taken when this segment stopped is recorded, so a
    /// test can rewrite the draft afterwards without racing that reader.
    fn wait_for_stop_draft(&mut self, sequence: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.app.poll();
            let recorded = fs::read(self.runtime.0.join("delivery.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|index| {
                    index["segments"].as_array().and_then(|records| {
                        records
                            .iter()
                            .find(|record| record["sequence"] == sequence)
                            .cloned()
                    })
                })
                .is_some_and(|record| {
                    record["baseline"].is_string() || record["baseline_problem"].is_string()
                });
            if recorded {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for segment {sequence}'s stop-time draft"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn set_draft(&self, editor: usize, draft: &str) {
        *self.editors[editor].draft.lock().unwrap() = draft.into();
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

    // The first segment's late result arrives while the second is capturing. Its
    // own target still holds the draft recorded at its stop, so it is appended once
    // to that target and must not change the live capture's phase. It is the newest
    // complete result, so it is shown as the current result until the live segment
    // finishes.
    h.finish(first, "第一段完整结果。", true);
    let waiting = h.poll_until("the first segment's late result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.text == "第一段完整结果。")
    });
    assert_eq!(waiting.phase, Phase::Capturing, "{waiting:?}");
    assert_eq!(
        h.draft(0),
        "第一段草稿\n第一段完整结果。",
        "a late result with an intact draft is appended to its own target"
    );
    assert_eq!(waiting.text, "第一段完整结果。");
    assert_eq!(waiting.delivery, Some(Delivery::Appended), "{waiting:?}");
    assert!(
        waiting
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.delivery == Some(Delivery::Appended)),
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
        "第一段草稿\n第一段完整结果。",
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
    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    // Every recovered result had its own intact stop-time draft, so each was
    // appended once to its own target — never to another segment's.
    assert_eq!(h.draft(0), "一段草稿\n第一段完整结果。");
    assert_eq!(h.draft(1), "二段草稿\n第二段完整结果。");
    assert_eq!(h.draft(2), "三段草稿\n第三段完整结果。");
}

#[test]
fn a_failure_in_an_old_segment_does_not_end_the_new_capture_or_clear_other_results() {
    let mut h = Harness::new(&["一段草稿", "二段草稿", "三段草稿"]);
    // Segment 1 produces a recovered result that must survive everything.
    let first = h.start(0);
    h.stop();
    h.finish(first, "第一段结果。", true);
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
        isolated.segments[0].text, "第一段结果。",
        "another segment's delivered result must not be cleared: {isolated:?}"
    );
    assert!(isolated.retry_available, "{isolated:?}");
    assert!(
        isolated.retained_audio.iter().all(|path| path.is_file()),
        "the failed segment's audio is retained: {isolated:?}"
    );

    // Segment 3 completes normally; the earlier result and the failed audio stay.
    h.stop();
    h.finish(third, "第三段完整结果。", false);
    let settled = h.poll_until("segment 3 result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(settled.text, "第三段完整结果。");
    assert_eq!(settled.delivery, Some(Delivery::Appended), "{settled:?}");
    assert_eq!(settled.segments[0].text, "第一段结果。");
    assert_eq!(settled.segments[1].state, SegmentState::Failed);
    assert!(
        settled.segments[1].text.is_empty(),
        "a failed segment never claims a complete result: {settled:?}"
    );
    assert!(
        settled.retained_audio.iter().all(|path| path.is_file()),
        "the old failure's audio must survive the new result: {settled:?}"
    );
    assert_eq!(h.draft(0), "一段草稿\n第一段结果。");
    assert_eq!(h.draft(1), "二段草稿");
    assert_eq!(h.draft(2), "三段草稿\n第三段完整结果。");
}

#[test]
fn every_segment_saves_its_own_audio_text_state_and_cost_without_overwriting_an_earlier_one() {
    let mut h = Harness::new(&["草稿一", "草稿二", "草稿三", "草稿四"]);
    let first = h.start(0);
    let (_, first_directory, _) = h.entry(first);
    h.script.set_segment_usage(
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
    h.wait_for_stop_draft(1);
    // Both older drafts are rewritten after their own stop, so their late results
    // cannot be proven safe and must stay independent pending results.
    h.set_draft(0, "草稿一\n用户改写");
    let second = h.start(1);
    let (_, second_directory, _) = h.entry(second);
    h.script.set_segment_usage(
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
    h.wait_for_stop_draft(2);
    h.set_draft(1, "草稿二\n用户改写");
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

/// The capture orders whose complete text is durably saved, read from the versioned
/// index. A confirmed target append deletes both the text file and the index entry,
/// so this is only stable while delivery is deliberately blocked; it is not a file
/// existence check.
fn saved_texts(runtime: &Path) -> Vec<u64> {
    let Ok(bytes) = fs::read(runtime.join("segments.json")) else {
        return Vec::new();
    };
    let Ok(index) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    index["segments"]
        .as_array()
        .map(|records| {
            records
                .iter()
                .filter(|record| record["text_file"].is_string())
                .filter_map(|record| record["sequence"].as_u64())
                .collect()
        })
        .unwrap_or_default()
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
    // application must not start three recoveries at the same time: the current
    // segment is handled first, then one older backlog segment per poll, oldest
    // first. Delivery is held at the target boundary so the ordering is observed on
    // the durable save (the text file a confirmed append deletes is not an oracle).
    h.set_writes_blocked(true);
    h.finish(third, "第三段完整结果。", true);
    h.finish(second, "第二段完整结果。", true);
    h.finish(first, "第一段完整结果。", true);
    let runtime = h.runtime().to_path_buf();
    h.poll_once();
    let saved = saved_texts(&runtime);
    assert!(
        saved.contains(&3),
        "the current segment is saved first: {saved:?}"
    );
    assert!(
        saved.contains(&1),
        "the oldest backlog segment follows the current one: {saved:?}"
    );
    assert!(
        !saved.contains(&2),
        "a later segment must not overtake the older backlog: {saved:?}"
    );
    h.poll_once();
    let saved = saved_texts(&runtime);
    assert!(
        saved.contains(&2),
        "the second backlog segment follows in capture order: {saved:?}"
    );
    assert!(
        saved.iter().all(|sequence| (1..=3).contains(sequence)),
        "still one backlog segment at a time: {saved:?}"
    );

    h.set_writes_blocked(false);
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
    assert_eq!(
        h.draft(0),
        "草稿一\n第一段完整结果。",
        "the oldest result is appended exactly once"
    );
    assert_eq!(h.draft(1), "草稿二\n第二段完整结果。");
    assert_eq!(h.draft(2), "草稿三\n第三段完整结果。");
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
    assert_eq!(
        after_old_events
            .segments
            .iter()
            .filter(|segment| segment.sequence == 1)
            .count(),
        1,
        "{after_old_events:?}"
    );
    assert_eq!(
        h.draft(0),
        "草稿甲\n第一段完整结果。",
        "a duplicate confirmation never appends the same segment twice"
    );

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
    assert_eq!(
        h.draft(0),
        "草稿甲\n第一段完整结果。",
        "the recovered result is appended once, and its duplicate is never appended"
    );
    assert_eq!(h.draft(1), "草稿乙\n第二段完整结果。");
    // The duplicate text never appeared anywhere.
    assert_ne!(settled.text, "第一段重复结果。");
}

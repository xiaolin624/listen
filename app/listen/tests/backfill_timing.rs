//! One stop-to-confirmed-backfill measurement at the application seam (#38).
//!
//! The recognition adapter injects its own confirmation duration and the target
//! injects its draft read and write duration, so the measured stages need no real
//! network, audio device or desktop editor. Everything is driven through the same
//! `Application::dispatch` / `poll` / `snapshot` interface the hotkey and the GUI use.
#![cfg(windows)]

use listen::{
    AppResult, Application, Control, Delivery, Phase, SegmentState, Target, TargetSelection,
    Timing,
    recognition::{SessionEvent, SessionId, SessionResult, SessionState, SessionUpdate},
    testing::{ScriptedControl, ScriptedSessions},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-backfill-timing-{}-{}-{}",
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

/// A bound editor whose draft read and write each take an injected duration, so a
/// measured delivery stage is a real elapsed time rather than a constant.
struct Editor {
    draft: Mutex<String>,
    read: Duration,
    write: Duration,
    fail_write: Mutex<Option<String>>,
    reads: AtomicUsize,
    writes: AtomicUsize,
}
struct Selection(Arc<Editor>);
struct Bound(Arc<Editor>);
impl TargetSelection for Selection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(Bound(self.0.clone()))))
    }
}
impl Target for Bound {
    fn content(&mut self) -> AppResult<String> {
        thread::sleep(self.0.read);
        self.0.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.draft.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        thread::sleep(self.0.write);
        self.0.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.0.fail_write.lock().unwrap().clone() {
            return Err(error);
        }
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed during output; no replacement".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

struct Harness {
    runtime: Runtime,
    editor: Arc<Editor>,
    app: Application,
    script: ScriptedControl,
}

impl Harness {
    fn new(draft: &str, read_ms: u64, write_ms: u64) -> Self {
        let runtime = Runtime::new();
        let editor = Arc::new(Editor {
            draft: Mutex::new(draft.into()),
            read: Duration::from_millis(read_ms),
            write: Duration::from_millis(write_ms),
            fail_write: Mutex::new(None),
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
        });
        let adapter = ScriptedSessions::new();
        let script = adapter.control();
        let app =
            Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
                .with_targets(Arc::new(Selection(editor.clone())));
        Self {
            runtime,
            editor,
            app,
            script,
        }
    }

    fn start(&mut self) -> SessionId {
        self.app.dispatch(Control::Start);
        assert_eq!(
            self.app.snapshot().phase,
            Phase::Capturing,
            "start must capture: {:?}",
            self.app.snapshot()
        );
        self.script.last().id()
    }

    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
    }

    fn finish(&self, index: usize, id: SessionId, text: &str, timing: Timing) {
        self.script.session(index).send_event(SessionEvent {
            id,
            update: SessionUpdate::Finished(SessionResult {
                text: text.into(),
                description: "scripted playback".into(),
                timing,
                stopped_at: None,
                recovered: false,
            }),
        });
    }

    fn draft(&self) -> String {
        self.editor.draft.lock().unwrap().clone()
    }

    fn wait_for_phase(&mut self, phase: Phase) -> listen::Snapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.app.poll();
            let state = self.app.snapshot();
            if state.phase == phase {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "expected {phase:?}, observed {state:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn option(timing: &Timing, field: fn(&Timing) -> Option<u64>, name: &str) -> u64 {
    field(timing).unwrap_or_else(|| panic!("{name} must be measured: {timing:?}"))
}

#[test]
fn one_confirmed_backfill_reports_stop_to_confirmed_and_every_stage() {
    let mut harness = Harness::new("原有草稿", 5, 25);
    let id = harness.start();
    harness.stop();
    harness.finish(0, id, "第一行：Rust 和 WASAPI。", Timing::default());

    let result = harness.wait_for_phase(Phase::Result);
    assert_eq!(result.delivery, Some(Delivery::Appended), "{result:?}");
    assert_eq!(
        harness.draft(),
        "原有草稿\n第一行：Rust 和 WASAPI。",
        "the complete result is appended once"
    );
    // One read records the stop-time draft for a possible late result, one reads
    // the latest draft immediately before the write. Both are bounded and measured.
    assert_eq!(harness.editor.reads.load(Ordering::SeqCst), 2);
    assert_eq!(harness.editor.writes.load(Ordering::SeqCst), 1);

    let timing = &result.timing;
    let confirmed = option(
        timing,
        |timing| timing.stop_to_confirmed_ms,
        "stop_to_confirmed_ms",
    );
    let result_ms = option(
        timing,
        |timing| timing.stop_to_result_ms,
        "stop_to_result_ms",
    );
    assert!(
        confirmed <= result_ms,
        "the target read-back happens before the application applies the outcome: {timing:?}"
    );

    // Durable save, then the injected target read and write, are each visible.
    let save = option(timing, |timing| timing.save_ms, "save_ms");
    let read = option(timing, |timing| timing.delivery_read_ms, "delivery_read_ms");
    let write = option(
        timing,
        |timing| timing.delivery_write_ms,
        "delivery_write_ms",
    );
    let delivery = option(timing, |timing| timing.delivery_ms, "delivery_ms");
    assert!(read >= 5, "the injected draft read is measured: {timing:?}");
    assert!(
        write >= 25,
        "the injected target write and read-back are measured: {timing:?}"
    );
    assert!(
        delivery >= read + write,
        "the delivery stage covers the target read and write: {timing:?}"
    );

    // Application scheduling is visible, not hidden inside the total.
    let event_wait = option(timing, |timing| timing.event_wait_ms, "event_wait_ms");
    let queue = option(
        timing,
        |timing| timing.delivery_queue_ms,
        "delivery_queue_ms",
    );
    let apply_wait = option(timing, |timing| timing.apply_wait_ms, "apply_wait_ms");
    let schedule = option(timing, |timing| timing.schedule_ms, "schedule_ms");
    assert_eq!(
        schedule,
        event_wait + queue + apply_wait,
        "application scheduling is the sum of its measured gaps: {timing:?}"
    );
    assert!(
        timing.delivery_lock_ms.is_some() && timing.delivery_save_ms.is_some(),
        "the writer lock and the fail-closed delivery record are measured: {timing:?}"
    );

    // The stages partition the same stop-to-result interval; every millisecond is
    // accounted for once, within the truncation of each stage.
    let stages = timing.finalize_ms.unwrap_or(0)
        + timing.cloud_drain_ms.unwrap_or(0)
        + timing.cloud_final_ms.unwrap_or(0)
        + event_wait
        + save
        + queue
        + delivery
        + apply_wait;
    assert!(
        (stages as i64 - result_ms as i64).abs() <= 8,
        "the stage sum {stages} ms must match stop_to_result {result_ms} ms: {timing:?}"
    );
    assert!(
        ((stages - apply_wait) as i64 - confirmed as i64).abs() <= 8,
        "stop_to_confirmed {confirmed} ms ends at the target read-back: {timing:?}"
    );

    // The same breakdown is persisted for later review.
    let record = fs::read_to_string(harness.runtime.0.join("latency.jsonl")).unwrap();
    let value: serde_json::Value = serde_json::from_str(record.lines().next().unwrap()).unwrap();
    assert_eq!(value["stop_to_confirmed_ms"], confirmed);
    assert_eq!(value["schedule_ms"], schedule);
    assert_eq!(value["delivery_read_ms"], read);
    assert_eq!(value["delivery_write_ms"], write);
    println!(
        "measured stages: stop_to_confirmed={confirmed} stop_to_result={result_ms} \
         stop_finalize={:?} cloud_final={:?} save={save} event_wait={event_wait} \
         delivery_queue={queue} delivery={delivery} read={read} write={write} \
         apply_wait={apply_wait}",
        timing.finalize_ms, timing.cloud_final_ms
    );
}

#[test]
fn a_cloud_result_keeps_its_own_whole_task_confirmation_stages() {
    let mut harness = Harness::new("草稿", 0, 0);
    let id = harness.start();
    harness.stop();
    let adapter = Timing {
        finalize_ms: Some(3),
        cloud_connect_ms: Some(40),
        first_task_started_ms: Some(120),
        cloud_drain_ms: Some(2),
        cloud_final_ms: Some(7),
        recognition_ms: Some(900),
        ..Timing::default()
    };
    harness.finish(0, id, "云端最终文本。", adapter);
    let result = harness.wait_for_phase(Phase::Result);

    let timing = &result.timing;
    assert_eq!(timing.finalize_ms, Some(3), "{timing:?}");
    assert_eq!(timing.cloud_connect_ms, Some(40), "{timing:?}");
    assert_eq!(timing.first_task_started_ms, Some(120), "{timing:?}");
    assert_eq!(timing.cloud_drain_ms, Some(2), "{timing:?}");
    assert_eq!(timing.cloud_final_ms, Some(7), "{timing:?}");
    assert!(
        timing.stop_to_confirmed_ms.is_some(),
        "a confirmed append has a stop-to-confirmed number: {timing:?}"
    );
    assert!(
        timing.stop_to_confirmed_ms <= timing.stop_to_result_ms,
        "{timing:?}"
    );
}

#[test]
fn a_fallback_never_reports_a_confirmed_target_read_back() {
    let mut harness = Harness::new("草稿", 5, 5);
    *harness.editor.fail_write.lock().unwrap() = Some("The original target is read-only".into());
    let id = harness.start();
    harness.stop();
    harness.finish(0, id, "完整但未写入的转写。", Timing::default());
    let result = harness.wait_for_phase(Phase::Result);

    assert_eq!(result.delivery, Some(Delivery::Fallback), "{result:?}");
    let timing = &result.timing;
    assert!(
        timing.stop_to_confirmed_ms.is_none(),
        "a fallback has no target read-back to claim: {timing:?}"
    );
    assert!(
        timing.stop_to_result_ms.is_some() && timing.apply_wait_ms.is_some(),
        "the outcome is still measured: {timing:?}"
    );
    assert!(
        timing.delivery_write_ms.is_some(),
        "the refused write is still measured: {timing:?}"
    );
    assert_eq!(harness.draft(), "草稿", "the target was never changed");
    assert_eq!(result.text, "完整但未写入的转写。");
}

#[test]
fn two_results_that_confirm_together_are_appended_in_capture_order() {
    // Both segments target the same editor and confirm close together. The newest
    // segment is still processed and saved first (#38), but the target append order
    // is capture order: the older sentence must never land after the newer one.
    let mut harness = Harness::new("合并草稿", 0, 20);
    let first = harness.start();
    harness.stop();
    let second = harness.start();
    harness.stop();
    harness.finish(1, second, "第二段完整结果。", Timing::default());
    harness.finish(0, first, "第一段完整结果。", Timing::default());

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.app.poll();
        let draft = harness.draft();
        if draft.matches("第一段完整结果。").count() == 1
            && draft.matches("第二段完整结果。").count() == 1
        {
            assert_eq!(
                draft, "合并草稿\n第一段完整结果。\n第二段完整结果。",
                "target appends must preserve capture order, oldest first"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both results were not appended: {draft:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let settled = loop {
        harness.app.poll();
        let state = harness.app.snapshot();
        if state
            .segments
            .iter()
            .all(|segment| segment.state == SegmentState::Result)
        {
            break state;
        }
        assert!(Instant::now() < deadline, "{state:?}");
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(settled.segments.len(), 2, "{settled:?}");
    assert_eq!(
        harness.draft(),
        "合并草稿\n第一段完整结果。\n第二段完整结果。",
        "target appends must preserve capture order, oldest first"
    );
}

#[test]
fn an_older_segment_never_delays_a_newer_segments_save_or_delivery() {
    // A 50 ms write keeps both deliveries in flight while the first poll applies the
    // two session events, so the saves can be observed before the deliveries.
    let mut harness = Harness::new("合并草稿", 0, 50);
    let first = harness.start();
    harness.stop();
    // A new capture is admitted while the older segment is still processing.
    let second = harness.start();
    harness.stop();

    // Both segments confirm between two polls.
    harness.finish(0, first, "第一段完整结果。", Timing::default());
    harness.finish(1, second, "第二段完整结果。", Timing::default());
    harness.app.poll();
    let after_first_poll = harness.app.snapshot();
    // The primary segment's whole-task event was applied in the same poll; its save
    // happened before either delivery could finish (the target write takes 50 ms).
    assert_eq!(
        after_first_poll.recognition,
        Some(SessionState::Complete),
        "the newer segment's event was applied in the same poll: {after_first_poll:?}"
    );
    assert!(
        harness.runtime.0.join("segment-00000001.txt").is_file(),
        "the older segment's complete text was saved in the same poll: {after_first_poll:?}"
    );
    assert!(
        harness.runtime.0.join("segment-00000002.txt").is_file(),
        "the newer segment's save never waits for another poll: {after_first_poll:?}"
    );

    // Wait, without polling the application, until both deliveries are durably
    // confirmed. Then one poll must apply the newest segment's delivery; the older
    // one stays pending for a later poll, so a backlog can never hold it back.
    let appended = |harness: &Harness| -> usize {
        let path = harness.runtime.0.join("delivery.json");
        let Ok(bytes) = fs::read(path) else {
            return 0;
        };
        let Ok(stored) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return 0;
        };
        stored["segments"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .filter(|record| record["state"] == "appended")
                    .count()
            })
            .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while appended(&harness) < 2 {
        assert!(
            Instant::now() < deadline,
            "both deliveries were not confirmed in time"
        );
        thread::sleep(Duration::from_millis(2));
    }
    harness.app.poll();
    let newest_first = harness.app.snapshot();
    assert_eq!(
        newest_first.segments[1].state,
        SegmentState::Result,
        "the newer delivery is applied before the older backlog: {newest_first:?}"
    );
    assert_eq!(
        newest_first.phase,
        Phase::Result,
        "the newer segment's result is not held behind the older one: {newest_first:?}"
    );
    assert_eq!(
        newest_first.segments[0].state,
        SegmentState::Processing,
        "the older backlog delivery keeps its own turn: {newest_first:?}"
    );

    let settled = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.app.poll();
            let state = harness.app.snapshot();
            if state
                .segments
                .iter()
                .all(|segment| segment.state == SegmentState::Result)
            {
                break state;
            }
            assert!(
                Instant::now() < deadline,
                "the backlog did not settle: {state:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    };
    assert_eq!(settled.segments.len(), 2, "{settled:?}");
    let draft = harness.draft();
    assert_eq!(
        draft.matches("第一段完整结果。").count(),
        1,
        "the older result is appended exactly once: {draft:?}"
    );
    assert_eq!(
        draft.matches("第二段完整结果。").count(),
        1,
        "the newer result is appended exactly once: {draft:?}"
    );
    assert!(draft.starts_with("合并草稿"), "{draft:?}");
}

#[test]
fn the_delivery_history_is_bounded_while_confirmed_appends_keep_working() {
    let segments = 60;
    let mut harness = Harness::new("草稿", 0, 0);
    let mut first_save_ms = None;
    let mut last_save_ms = None;
    for index in 0..segments {
        let id = harness.start();
        harness.stop();
        let text = format!("第{index}段完整转写：Rust 和 WASAPI 与云端最终确认。");
        harness.finish(index, id, &text, Timing::default());
        let result = harness.wait_for_phase(Phase::Result);
        assert_eq!(
            result.delivery,
            Some(Delivery::Appended),
            "segment {index}: {result:?}"
        );
        let saved = result
            .timing
            .delivery_save_ms
            .expect("delivery record save");
        first_save_ms.get_or_insert(saved);
        last_save_ms = Some(saved);
    }

    let path = harness.runtime.0.join("delivery.json");
    let bytes = fs::metadata(&path).unwrap().len();
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let records = stored["segments"].as_array().unwrap().len();
    println!(
        "delivery.json records={records} bytes={bytes} first_save_ms={first_save_ms:?} last_save_ms={last_save_ms:?}"
    );
    assert!(
        bytes <= 192 * 1024,
        "the durable delivery record must stay bounded, got {bytes} bytes"
    );
    assert!(
        records <= 64,
        "resolved delivery history is bounded, got {records} records"
    );
    let newest = stored["segments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["sequence"] == segments)
        .expect("the newest append stays recorded");
    assert_eq!(newest["state"], "appended");
    assert_eq!(
        harness.draft().matches("完整转写").count(),
        segments,
        "every complete result is appended exactly once"
    );
}

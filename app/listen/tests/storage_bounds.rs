//! Storage bounds and explicit cleanup (#36) through the same application
//! interface the hotkey, tray and `--control` use.
//!
//! Decisions only: how much pending audio exists across every segment, when that
//! crosses the warning line, when the budget or the free-disk floor stops a capture,
//! how a failed save is told apart from a network wait, and what an explicit
//! per-segment delete does to the other unfinished work and to late events.
//!
//! Storage conditions are injected (a small budget, a controlled free-space probe
//! and the runtime directory itself); no test creates real disk pressure and no
//! paid network is contacted.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Notice, Phase, SegmentState,
    Snapshot, Target, TargetSelection, Timing,
    recognition::{
        SessionEvent, SessionFailure, SessionId, SessionResult, SessionState, SessionUpdate,
    },
    storage::{FreeSpace, StorageLevel, StoragePolicy},
    testing::{ScriptedControl, ScriptedSessionControl, ScriptedSessions},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
            "listen-storage-bounds-{}-{}-{}",
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

/// A controlled free-space probe. Production reads the runtime volume; tests set a
/// value, so disk pressure is a state, not a machine accident.
struct ControlledFreeSpace(Arc<AtomicU64>);
impl FreeSpace for ControlledFreeSpace {
    fn free_bytes(&self, _directory: &Path) -> AppResult<u64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

struct Editor {
    draft: Mutex<String>,
    /// When set, reading the draft waits until the gate is opened. It lets a test
    /// hold one target delivery in the "being written" state deterministically.
    gate: Mutex<Option<Arc<AtomicBool>>>,
}
impl Editor {
    fn wait_gate(&self) {
        loop {
            let gate = self.gate.lock().unwrap().clone();
            match gate {
                Some(gate) if !gate.load(Ordering::SeqCst) => {
                    thread::sleep(Duration::from_millis(1))
                }
                _ => return,
            }
        }
    }
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
        self.0.wait_gate();
        Ok(self.0.draft.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        self.0.wait_gate();
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

struct Harness {
    runtime: Runtime,
    app: Application,
    script: ScriptedControl,
    editors: Vec<Arc<Editor>>,
    focus: Arc<FocusedEditors>,
    free: Arc<AtomicU64>,
}

/// A small pending-audio budget with room for a couple of test segments. The free
/// disk is generous and the floor is zero unless a test sets it, so a budget test
/// never contradicts the machine it runs on.
fn budget_policy(bytes: u64) -> StoragePolicy {
    StoragePolicy {
        pending_audio_budget_bytes: bytes,
        free_disk_floor_bytes: 0,
        ..StoragePolicy::default()
    }
}

impl Harness {
    fn new(drafts: &[&str], policy: StoragePolicy, free_bytes: u64) -> Self {
        Self::with_runtime(Runtime::new(), drafts, policy, free_bytes)
    }
    fn with_runtime(
        runtime: Runtime,
        drafts: &[&str],
        policy: StoragePolicy,
        free_bytes: u64,
    ) -> Self {
        let editors: Vec<Arc<Editor>> = drafts
            .iter()
            .map(|draft| {
                Arc::new(Editor {
                    draft: Mutex::new((*draft).into()),
                    gate: Mutex::new(None),
                })
            })
            .collect();
        let focus = Arc::new(FocusedEditors {
            editors: editors.clone(),
            focus: Mutex::new(0),
        });
        let adapter = ScriptedSessions::new().with_backend("controlled storage recognition");
        let script = adapter.control();
        let free = Arc::new(AtomicU64::new(free_bytes));
        let app =
            Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
                .with_targets(focus.clone())
                .with_storage_policy(policy)
                .with_free_space(Arc::new(ControlledFreeSpace(free.clone())));
        Self {
            runtime,
            app,
            script,
            editors,
            focus,
            free,
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.app.snapshot()
    }
    fn storage(&self) -> listen::storage::StorageStatus {
        self.snapshot()
            .storage
            .expect("storage status is observable")
    }
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
    fn segment_dir(&self, index: usize) -> PathBuf {
        self.script.session(index).directory()
    }
    fn controls(&self, index: usize) -> ScriptedSessionControl {
        self.script.session(index)
    }
    fn stopped(&self, index: usize) -> bool {
        self.script.session(index).stop_count() > 0
    }
    fn draft(&self, editor: usize) -> String {
        self.editors[editor].draft.lock().unwrap().clone()
    }
    fn set_draft(&self, editor: usize, draft: &str) {
        *self.editors[editor].draft.lock().unwrap() = draft.into();
    }
    /// Wait until the draft read taken when this segment stopped is recorded, so a
    /// test can rewrite the draft afterwards without racing that reader (#34).
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
    /// Hold the next target read for this editor until the returned gate opens, so
    /// the "delivery is being written" state can be observed without a race.
    fn gate_editor(&self, editor: usize) -> Arc<AtomicBool> {
        let gate = Arc::new(AtomicBool::new(false));
        *self.editors[editor].gate.lock().unwrap() = Some(gate.clone());
        gate
    }
    fn runtime(&self) -> &Path {
        &self.runtime.0
    }
    fn set_free(&self, bytes: u64) {
        self.free.store(bytes, Ordering::SeqCst);
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
        for _ in 0..12 {
            self.app.poll();
            thread::sleep(Duration::from_millis(2));
        }
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
    /// Fail one segment, optionally keeping its captured audio for an explicit retry.
    fn fail_with_audio(&self, index: usize, message: &str, keep_audio: bool) {
        let (id, directory, events) = self.entry(index);
        let retry_audio = keep_audio.then(|| CapturedAudio {
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
    fn delete(&mut self, sequence: u64) {
        self.app.dispatch(Control::DeleteSegment(sequence));
    }
}

fn write_bytes(path: &Path, length: usize) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, vec![7u8; length]).unwrap();
}

#[test]
fn pending_audio_is_counted_across_segments_and_a_full_budget_stops_the_capture_and_the_next_start()
{
    let mut h = Harness::new(&["第一段草稿", "第二段草稿"], budget_policy(1_000), 1 << 30);
    let first = h.start(0);
    let first_directory = h.segment_dir(first);
    write_bytes(&first_directory.join("audio.wav"), 600);
    h.stop();
    // A later segment is still admitted while the pending audio fits the budget.
    let second = h.start(1);
    let second_directory = h.segment_dir(second);
    // The live capture itself crosses the budget. Nothing is deleted or overwritten.
    write_bytes(&second_directory.join("playback.pcm"), 500);
    // The desktop caller measures storage on its own low-frequency tick; `poll()`
    // never walks the runtime directory (#49). Ask for the measurement explicitly.
    h.app.measure_storage();

    let stopped = h.poll_until("the storage stop", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    assert!(
        h.stopped(second),
        "the capture must be stopped instead of pretending to keep recording: {stopped:?}"
    );
    let status = stopped.storage.clone().expect("storage status");
    assert!(status.over_budget(), "{status:?}");
    assert_eq!(status.level(), StorageLevel::Full);
    assert!(
        status.pending_audio_bytes >= 1_100,
        "both segments' audio is counted: {status:?}"
    );
    assert!(
        stopped.detail.to_lowercase().contains("storage"),
        "the stop must say it was a storage limit: {stopped:?}"
    );
    assert!(
        stopped.detail.to_lowercase().contains("retained")
            || stopped.detail.to_lowercase().contains("kept"),
        "the received audio must be described as kept: {stopped:?}"
    );
    assert!(first_directory.join("audio.wav").is_file());
    assert!(second_directory.join("playback.pcm").is_file());

    // While the budget is still exceeded, a new capture is refused, not started into
    // an overwrite, and both existing segments keep their own audio and record.
    h.app.dispatch(Control::Start);
    let refused = h.snapshot();
    assert_eq!(refused.phase, Phase::Failure, "{refused:?}");
    assert!(
        refused.detail.to_lowercase().contains("storage"),
        "{refused:?}"
    );
    assert!(
        refused.detail.contains("No new capture was started"),
        "the refusal must say no capture began: {refused:?}"
    );
    assert_eq!(refused.segments.len(), 2, "{refused:?}");
    assert!(first_directory.join("audio.wav").is_file());
    assert!(second_directory.join("playback.pcm").is_file());
}

#[test]
fn temporary_uploads_duplicate_fragments_and_repair_files_all_count_not_only_the_final_wav() {
    let runtime = Runtime::new();
    // An earlier failed segment: a small published WAV, a large temporary upload and
    // a large repair copy. Only the sum can decide whether there is room.
    let directory = runtime.0.join("session-77-1");
    write_bytes(&directory.join("playback.wav"), 100);
    write_bytes(&directory.join("upload-0001.part"), 600);
    write_bytes(&directory.join("repair/merged-retry.wav"), 700);
    let mut h = Harness::with_runtime(runtime, &["草稿"], budget_policy(1_000), 1 << 30);
    assert!(
        h.storage().pending_audio_bytes >= 1_400,
        "every temporary file counts: {:?}",
        h.storage()
    );
    h.app.dispatch(Control::Start);
    let refused = h.snapshot();
    assert_eq!(refused.phase, Phase::Failure, "{refused:?}");
    assert!(refused.detail.to_lowercase().contains("storage"));
    assert!(
        directory.join("upload-0001.part").is_file()
            && directory.join("repair/merged-retry.wav").is_file(),
        "refusing a capture never deletes the files it counted"
    );
}

#[test]
fn approaching_the_budget_warns_before_it_refuses_anything() {
    let runtime = Runtime::new();
    // 85% of a 1,000-byte budget: a warning, not a stop.
    write_bytes(&runtime.0.join("session-9-1/playback.pcm"), 850);
    let mut h = Harness::with_runtime(runtime, &["草稿"], budget_policy(1_000), 1 << 30);
    let status = h.storage();
    assert!(status.near_limit(), "{status:?}");
    assert!(!status.blocking(), "{status:?}");
    assert_eq!(status.level(), StorageLevel::Warning);
    assert_eq!(
        h.snapshot().notice,
        Some(Notice::StorageWarning),
        "the approaching limit must be visible before anything is refused"
    );
    assert!(h.snapshot().notice.unwrap().text().contains("缓存上限"));

    // A warning is a reminder only: capture still starts while there is room.
    h.start(0);
    assert_eq!(h.snapshot().phase, Phase::Capturing);
    assert_eq!(h.snapshot().segments[0].sequence, 1);
}

#[test]
fn a_disk_below_the_floor_refuses_the_next_capture_and_keeps_existing_recordings() {
    let runtime = Runtime::new();
    write_bytes(&runtime.0.join("session-77-2/audio.wav"), 10);
    let policy = StoragePolicy {
        pending_audio_budget_bytes: 1 << 20,
        free_disk_floor_bytes: 1_000_000,
        ..StoragePolicy::default()
    };
    let mut h = Harness::with_runtime(runtime, &["草稿"], policy, 999_999);
    let status = h.storage();
    assert!(status.below_floor(), "{status:?}");
    assert!(status.blocking(), "{status:?}");
    assert_eq!(status.level(), StorageLevel::DiskLow);

    h.app.dispatch(Control::Start);
    let refused = h.snapshot();
    assert_eq!(refused.phase, Phase::Failure, "{refused:?}");
    assert!(
        refused.detail.to_lowercase().contains("disk")
            || refused.detail.to_lowercase().contains("free space"),
        "the refusal must name the free-disk floor: {refused:?}"
    );
    assert!(h.runtime().join("session-77-2/audio.wav").is_file());
}

#[test]
fn a_save_failure_is_a_storage_fault_with_real_retained_audio_and_never_a_network_wait() {
    let mut h = Harness::new(&["受保护草稿"], budget_policy(1 << 20), 1 << 30);
    let first = h.start(0);
    h.stop();
    // A directory at the durable text path forces a real save failure without any
    // real disk pressure.
    fs::create_dir(h.runtime().join("segment-00000001.txt")).unwrap();
    h.finish(first, "完整结果仍然保留在面板", false);
    let failed = h.poll_until("the storage failure", |snapshot| {
        snapshot.phase == Phase::Failure
    });
    assert!(
        failed.detail.starts_with("Storage fault:"),
        "a save failure must read as a real storage fault: {failed:?}"
    );
    assert!(
        failed.detail.contains("not a network wait")
            && failed.detail.contains("not a playback-device interruption"),
        "it must be distinguished from both other failure kinds: {failed:?}"
    );
    let storage = failed.storage.clone().expect("storage status");
    assert!(
        storage.fault.is_some(),
        "the fault stays observable as itself: {storage:?}"
    );
    assert_eq!(
        h.draft(0),
        "受保护草稿",
        "nothing was written to the target"
    );
    assert_eq!(
        failed.text, "完整结果仍然保留在面板",
        "the complete text stays readable: {failed:?}"
    );
    assert!(!failed.retained_audio.is_empty(), "{failed:?}");
    assert!(failed.retained_audio.iter().all(|path| path.is_file()));
}

#[test]
fn audio_over_the_budget_never_blocks_saving_the_complete_text_to_its_own_bound() {
    let mut h = Harness::new(&["草稿"], budget_policy(1_000), 1 << 30);
    let first = h.start(0);
    let directory = h.segment_dir(first);
    write_bytes(&directory.join("playback.pcm"), 2_000);
    // `poll()` no longer measures (#49); the desktop tick does. Measure explicitly
    // so the budget stop is decided here instead of at some later tick.
    h.app.measure_storage();
    let stopped = h.poll_until("the budget stop", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    assert!(
        stopped.storage.as_ref().is_some_and(|s| s.over_budget()),
        "{stopped:?}"
    );
    // The audio budget is already exceeded, but the complete text has its own bound
    // and must still be saved: the two limits are not interchangeable. The draft is
    // rewritten after the storage stop, so the recovered result stays a pending
    // result whose durable text file proves the save (#34).
    h.wait_for_stop_draft(1);
    h.set_draft(0, "草稿（改写）");
    h.finish(first, "这段完整文字仍要保存。", true);
    let saved = h.poll_until("the saved complete text", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.state == SegmentState::Result)
    });
    assert_eq!(saved.text, "这段完整文字仍要保存。");
    assert_eq!(h.draft(0), "草稿（改写）");
    let text_file = h.runtime().join("segment-00000001.txt");
    assert!(
        text_file.is_file(),
        "the complete text was saved despite the full audio budget"
    );
    assert!(
        !directory.exists(),
        "the audio is released only after the complete text is durably saved"
    );
}

#[test]
fn saved_text_has_its_own_bound_and_overflowing_it_is_a_storage_fault() {
    let policy = StoragePolicy {
        pending_audio_budget_bytes: 1 << 20,
        free_disk_floor_bytes: 0,
        text_budget_bytes: 60,
        ..StoragePolicy::default()
    };
    let runtime = Runtime::new();
    // Text from an earlier pending result counts toward the text bound only. It is
    // on disk before the application opens, exactly like an earlier run.
    fs::write(runtime.0.join("segment-00000001.txt"), vec![b'x'; 40]).unwrap();
    let mut h = Harness::with_runtime(runtime, &["草稿"], policy, 1 << 30);
    let status = h.storage();
    assert_eq!(status.pending_audio_bytes, 0, "{status:?}");
    assert_eq!(status.text_bytes, 40, "{status:?}");
    assert!(!status.over_budget(), "saved text is not pending audio");

    let first = h.start(0);
    h.stop();
    h.finish(
        first,
        "一段超过剩余文字预算的完整结果，必须保留录音。",
        false,
    );
    let failed = h.poll_until("the text-bound failure", |snapshot| {
        snapshot.phase == Phase::Failure
    });
    assert!(
        failed.detail.contains("Storage fault (text)"),
        "the text bound reports its own storage fault: {failed:?}"
    );
    assert!(!failed.retained_audio.is_empty(), "{failed:?}");
    assert!(failed.retained_audio.iter().all(|path| path.is_file()));
    assert_eq!(h.draft(0), "草稿");
}

#[test]
fn the_free_disk_floor_stops_a_running_capture_and_keeps_the_received_audio() {
    let policy = StoragePolicy {
        pending_audio_budget_bytes: 1 << 20,
        free_disk_floor_bytes: 1_000_000,
        ..StoragePolicy::default()
    };
    let mut h = Harness::new(&["草稿"], policy, 2_000_000);
    let first = h.start(0);
    let directory = h.segment_dir(first);
    write_bytes(&directory.join("playback.pcm"), 300);
    h.set_free(999_999);
    // The free-space probe is read by the measurement, and `poll()` no longer
    // measures (#49); the desktop tick does.
    h.app.measure_storage();
    let stopped = h.poll_until("the floor stop", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    assert!(
        h.stopped(first),
        "capture stops when the disk floor is reached: {stopped:?}"
    );
    assert!(directory.join("playback.pcm").is_file());
    let storage = stopped.storage.clone().expect("storage status");
    assert!(storage.below_floor(), "{storage:?}");
    assert!(
        stopped.detail.contains("free-disk floor"),
        "the stop names the disk floor: {stopped:?}"
    );
    assert!(
        stopped.detail.contains("not a network wait"),
        "the stop is a storage condition: {stopped:?}"
    );
}

#[test]
fn deleting_a_selected_pending_result_removes_only_it_and_a_late_event_cannot_resurrect_it() {
    let mut h = Harness::new(&["一段草稿", "二段草稿"], budget_policy(1 << 20), 1 << 30);
    let first = h.start(0);
    h.stop();
    let second = h.start(1);
    h.stop();
    // Both drafts are rewritten after their own stop, so each recovered result stays
    // its own pending result — exactly the state this test deletes explicitly (#34).
    h.wait_for_stop_draft(1);
    h.wait_for_stop_draft(2);
    h.set_draft(0, "一段草稿\n用户改写");
    h.set_draft(1, "二段草稿\n用户改写");
    h.finish(first, "第一段待取结果。", true);
    h.finish(second, "第二段待取结果。", true);
    let settled = h.poll_until("both pending results", |snapshot| {
        snapshot
            .segments
            .iter()
            .filter(|segment| !segment.text.is_empty())
            .count()
            == 2
    });
    assert_eq!(settled.text, "第二段待取结果。");
    let first_text = h.runtime().join("segment-00000001.txt");
    assert!(first_text.is_file(), "{settled:?}");
    let (_, _, first_events) = h.entry(first);

    h.delete(1);
    let deleted = h.snapshot();
    assert_eq!(deleted.segments.len(), 1, "{deleted:?}");
    assert_eq!(deleted.segments[0].sequence, 2);
    assert_eq!(deleted.segments[0].text, "第二段待取结果。");
    assert_eq!(
        deleted.text, "第二段待取结果。",
        "another pending result is still the current result: {deleted:?}"
    );
    assert!(!first_text.exists(), "the deleted pending result is gone");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(h.runtime().join("segments.json")).unwrap()).unwrap();
    let records = index["segments"].as_array().expect("segment records");
    assert_eq!(records.len(), 1, "{index}");
    assert_eq!(records[0]["sequence"], 2, "{index}");

    // The cleaned segment's session channel is closed: a late whole-task
    // confirmation cannot even reach the application, so it can never recreate the
    // segment or write the target.
    assert!(
        first_events
            .send(SessionEvent {
                id: SessionId(1),
                update: SessionUpdate::Finished(SessionResult {
                    text: "迟到结果".into(),
                    description: "controlled playback".into(),
                    timing: Timing::default(),
                    stopped_at: None,
                    recovered: false,
                }),
            })
            .is_err(),
        "the deleted segment's session handle was dropped"
    );
    h.poll_settled();
    let after = h.snapshot();
    assert_eq!(after.segments.len(), 1);
    assert!(after.segments.iter().all(|segment| segment.sequence != 1));
    assert_eq!(
        h.draft(0),
        "一段草稿\n用户改写",
        "the deleted pending result never wrote its target"
    );

    // Deleting the remaining (current) pending result also clears its durable
    // current copy and record; it never expires by date and nothing else is touched.
    h.delete(2);
    let empty = h.snapshot();
    assert!(empty.segments.is_empty(), "{empty:?}");
    assert!(empty.text.is_empty(), "{empty:?}");
    assert!(!h.runtime().join("segment-00000002.txt").exists());
    assert_eq!(
        fs::read_to_string(h.runtime().join("current-result.txt")).unwrap(),
        "",
        "a deleted current result must not come back after a restart"
    );
    assert_eq!(
        h.draft(1),
        "二段草稿\n用户改写",
        "the deleted pending result never wrote its target"
    );
    assert_eq!(h.snapshot().phase, Phase::Ready);
}

#[test]
fn deleting_a_recovering_segment_stops_its_later_processing_and_keeps_the_new_capture() {
    let mut h = Harness::new(&["一段草稿", "二段草稿"], budget_policy(1 << 20), 1 << 30);
    let first = h.start(0);
    h.controls(first).set_state(SessionState::WaitingForNetwork);
    h.poll_until("segment 1 waiting for the network", |snapshot| {
        snapshot.segments.iter().any(|segment| {
            segment.sequence == 1 && segment.state == SegmentState::WaitingForNetwork
        })
    });
    h.stop();
    let second = h.start(1);
    assert_eq!(h.snapshot().phase, Phase::Capturing);
    let first_directory = h.segment_dir(first);
    assert!(first_directory.is_dir());
    let (_, _, first_events) = h.entry(first);

    h.delete(1);
    let after = h.snapshot();
    assert_eq!(
        after.phase,
        Phase::Capturing,
        "deleting an older recovering segment must not stop the live capture: {after:?}"
    );
    assert!(after.segments.iter().all(|segment| segment.sequence != 1));
    assert!(!first_directory.exists());
    assert!(
        first_events
            .send(SessionEvent {
                id: SessionId(1),
                update: SessionUpdate::Finished(SessionResult {
                    text: "恢复后的迟到结果".into(),
                    description: "controlled playback".into(),
                    timing: Timing::default(),
                    stopped_at: None,
                    recovered: true,
                }),
            })
            .is_err()
    );
    h.poll_settled();
    assert!(
        h.snapshot()
            .segments
            .iter()
            .all(|segment| segment.sequence != 1)
    );

    // The new capture still stops, completes and is delivered normally.
    h.stop();
    h.finish(second, "第二段完整结果。", false);
    let result = h.poll_until("the second result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(result.text, "第二段完整结果。");
    assert_eq!(h.draft(1), "二段草稿\n第二段完整结果。");
    assert_eq!(h.draft(0), "一段草稿");
}

#[test]
fn deleting_a_failed_recording_keeps_other_unfinished_work_and_remaining_tasks_usable() {
    let mut h = Harness::new(
        &["草稿一", "草稿二", "草稿三"],
        budget_policy(1 << 20),
        1 << 30,
    );
    let first = h.start(0);
    h.stop();
    h.finish(first, "第一段待取。", true);
    h.poll_until("the first pending result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| !segment.text.is_empty())
    });
    let second = h.start(1);
    h.stop();
    h.poll_until("segment 2 processing", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    h.fail_with_audio(
        second,
        "Playback capture was interrupted; received audio is retained and this capture is incomplete",
        true,
    );
    let failed = h.poll_until("segment 2 failed", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 2 && segment.state == SegmentState::Failed)
    });
    assert!(failed.retry_available, "{failed:?}");
    let second_directory = h.segment_dir(second);
    assert!(second_directory.join("audio.wav").is_file());

    h.delete(2);
    let after = h.snapshot();
    assert!(after.segments.iter().all(|segment| segment.sequence != 2));
    assert!(!second_directory.exists());
    assert!(
        !after.retry_available,
        "the retained audio went with the deleted segment: {after:?}"
    );
    assert_eq!(after.segments.len(), 1, "{after:?}");
    assert_eq!(after.segments[0].text, "第一段待取。");
    assert_eq!(
        after.segments[0].delivery,
        Some(Delivery::Appended),
        "the earlier result was safely delivered to its own target: {after:?}"
    );
    assert_eq!(h.draft(0), "草稿一\n第一段待取。");

    // The untouched earlier result is still available, and a new capture still runs
    // to a complete result and is delivered to its own target.
    let third = h.start(2);
    h.stop();
    h.finish(third, "第三段结果。", false);
    let result = h.poll_until("the third result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(result.text, "第三段结果。");
    assert_eq!(h.draft(2), "草稿三\n第三段结果。");
    assert_eq!(
        result
            .segments
            .iter()
            .filter(|segment| !segment.text.is_empty())
            .count(),
        2,
        "{result:?}"
    );
}

#[test]
fn the_live_capture_cannot_be_deleted_and_is_left_recording() {
    let mut h = Harness::new(&["草稿"], budget_policy(1 << 20), 1 << 30);
    let first = h.start(0);
    let directory = h.segment_dir(first);
    h.delete(1);
    let after = h.snapshot();
    assert_eq!(after.phase, Phase::Capturing, "{after:?}");
    assert!(after.detail.contains("still being captured"), "{after:?}");
    assert_eq!(after.segments.len(), 1);
    assert!(directory.is_dir());
    assert!(
        !h.stopped(first),
        "a refused delete must not stop the capture"
    );
}

#[test]
fn a_result_being_written_to_its_target_cannot_be_deleted_until_the_write_finishes() {
    let mut h = Harness::new(&["原始草稿"], budget_policy(1 << 20), 1 << 30);
    let gate = h.gate_editor(0);
    let first = h.start(0);
    h.stop();
    h.finish(first, "完整结果", false);
    // One poll applies the confirmation and starts the target write, which the gate
    // holds. Deleting now would race the write.
    h.app.poll();
    h.delete(1);
    let refused = h.snapshot();
    assert!(
        refused.detail.contains("being written to its target"),
        "{refused:?}"
    );
    assert_eq!(refused.segments.len(), 1);
    assert!(h.runtime().join("segment-00000001.txt").is_file());

    gate.store(true, Ordering::SeqCst);
    let result = h.poll_until("the delivered result", |snapshot| {
        snapshot.phase == Phase::Result
    });
    assert_eq!(result.text, "完整结果");
    assert_eq!(h.draft(0), "原始草稿\n完整结果");

    // Once delivered there is nothing pending left: an explicit delete refuses and
    // never pretends to undo text that is already in the editor.
    h.delete(1);
    let after = h.snapshot();
    assert!(
        after.detail.contains("already delivered"),
        "nothing pending means nothing is deleted: {after:?}"
    );
    assert_eq!(after.segments.len(), 1);
    assert_eq!(after.text, "完整结果");
    assert_eq!(h.draft(0), "原始草稿\n完整结果");
}

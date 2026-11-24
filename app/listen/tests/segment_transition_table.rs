//! The segment state transition table (#46, W4).
//!
//! One row per transition: what happens to a segment (a session event, a
//! delivery outcome, a storage stop, a failure, a retry) and what the user is then
//! shown — `phase`, `recognition`, the category of the state sentence, and whether
//! the segment still has a durable record.
//!
//! This table was written before the single application point existed, against the
//! behaviour the five scattered writers already produced. It is the guardrail for
//! moving those writers into one place: the table must stay green without a single
//! behaviour change.
//!
//! The scenario is driven through the same `Application` seam the hotkey and the
//! tray use, with two injected adapters only (a scripted recognition adapter and a
//! controlled editor). No audio device, no real transcription and no paid network
//! is involved.
#![cfg(windows)]

use listen::{
    Application, Control, Delivery, Phase, SegmentState, Snapshot, Target, TargetSelection,
    recognition::SessionState,
    storage::{FreeSpace, StoragePolicy},
    testing::{ScriptedControl, ScriptedSessions},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ADAPTER_DETAIL: &str = "scripted recognition session";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-transitions-{}-{}-{}",
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

struct ControlledFreeSpace(Arc<AtomicU64>);
impl FreeSpace for ControlledFreeSpace {
    fn free_bytes(&self, _directory: &Path) -> listen::AppResult<u64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

struct Editor {
    draft: Mutex<String>,
}
struct Bound(Arc<Editor>);
struct FocusedEditors {
    editors: Vec<Arc<Editor>>,
}
impl TargetSelection for FocusedEditors {
    fn focused_target(&self) -> listen::AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(Bound(self.editors[0].clone()))))
    }
}
impl Target for Bound {
    fn content(&mut self) -> listen::AppResult<String> {
        Ok(self.0.draft.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> listen::AppResult<()> {
        let mut draft = self.0.draft.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

fn policy(pending_audio_budget_bytes: u64) -> StoragePolicy {
    StoragePolicy {
        pending_audio_budget_bytes,
        free_disk_floor_bytes: 0,
        ..StoragePolicy::default()
    }
}

struct Harness {
    runtime: Runtime,
    app: Application,
    script: ScriptedControl,
    editors: Vec<Arc<Editor>>,
}

impl Harness {
    /// A harness with one controlled editor target and a generous pending-audio
    /// budget.
    fn new() -> Self {
        Self::with(policy(u64::MAX / 4), true, Ok(()))
    }
    fn with(policy: StoragePolicy, with_target: bool, hotkey: Result<(), String>) -> Self {
        let runtime = Runtime::new();
        let editors = vec![Arc::new(Editor {
            draft: Mutex::new("第一段草稿".into()),
        })];
        let adapter = ScriptedSessions::new().with_backend("scripted recognition");
        let script = adapter.control();
        let mut app =
            Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), hotkey)
                .with_storage_policy(policy)
                .with_free_space(Arc::new(ControlledFreeSpace(Arc::new(AtomicU64::new(
                    1 << 40,
                )))));
        if with_target {
            app = app.with_targets(Arc::new(FocusedEditors {
                editors: editors.clone(),
            }));
        }
        Self {
            runtime,
            app,
            script,
            editors,
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.app.snapshot()
    }
    fn start(&mut self) -> usize {
        self.app.dispatch(Control::Start);
        assert_eq!(
            self.snapshot().phase,
            Phase::Capturing,
            "a scripted capture starts: {:?}",
            self.snapshot()
        );
        self.script.count() - 1
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
    }
    fn directory(&self, index: usize) -> PathBuf {
        self.script.session(index).directory()
    }
    fn finish(&self, index: usize, text: &str) {
        self.script.session(index).finish(text);
    }
    fn fail(&self, index: usize, error: &str) {
        self.script.session(index).fail(error);
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
    /// The durable state this segment has in `segments.json`, or `None` when its
    /// record is gone.
    fn record(&self, sequence: u64) -> Option<String> {
        let bytes = fs::read(self.runtime.0.join("segments.json")).ok()?;
        let index: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        index["segments"]
            .as_array()
            .and_then(|records| records.iter().find(|record| record["sequence"] == sequence))
            .and_then(|record| record["state"].as_str())
            .map(str::to_owned)
    }
    fn draft(&self) -> String {
        self.editors[0].draft.lock().unwrap().clone()
    }
}

/// Which class of user-visible sentence the transition is expected to leave. The
/// class — not the exact wording — is what the table pins; the wording itself is
/// asserted only where it names the consequence the user acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Detail {
    /// The recognition adapter's own progress sentence.
    Adapter,
    /// A complete result and where it went, including the playback description.
    Result,
    /// A complete result that was not written to any target.
    NoTarget,
    /// Silence: no result and no delivery.
    NoSpeech,
    /// A failure with its reason.
    Failure,
    /// Why the storage bounds stopped the capture.
    StorageStop,
    /// Why a start was refused.
    StartRefusal,
    /// Why the hotkey could not be registered.
    HotkeyRefusal,
    /// Why a retry did nothing.
    RetryRefusal,
}

impl Detail {
    fn holds(self, detail: &str) {
        match self {
            Self::Adapter => assert_eq!(detail, ADAPTER_DETAIL, "adapter progress sentence"),
            Self::Result => {
                assert!(
                    detail.contains("The complete result was appended to the original target"),
                    "a confirmed append says so: {detail}"
                );
                assert!(
                    detail.contains("\nPlayback: scripted playback"),
                    "the result names the playback device: {detail}"
                );
            }
            Self::NoTarget => assert!(
                detail.contains("No supported editable target was selected at capture start"),
                "the fallback names its reason: {detail}"
            ),
            Self::NoSpeech => assert!(
                detail.contains("No valid speech detected; nothing was delivered")
                    && detail.contains("The previous current result is unchanged"),
                "silence keeps the previous result: {detail}"
            ),
            Self::Failure => assert!(
                detail.contains("the recognition engine lost the connection"),
                "the failure keeps its reason: {detail}"
            ),
            Self::StorageStop => assert!(
                detail.contains("storage budget was reached while this segment was being captured"),
                "the storage stop names the limit: {detail}"
            ),
            Self::StartRefusal => assert!(
                detail.contains("Storage is full") && detail.contains("No new capture was started"),
                "a refused start says nothing was overwritten: {detail}"
            ),
            Self::HotkeyRefusal => assert!(
                detail.contains("Global hotkey unavailable"),
                "the hotkey failure names itself: {detail}"
            ),
            Self::RetryRefusal => assert!(
                detail.contains("No retained audio is available for a retry"),
                "a retry with nothing to retry says so: {detail}"
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct Expected {
    phase: Phase,
    recognition: Option<SessionState>,
    detail: Detail,
    delivery: Option<Delivery>,
    /// The durable state in `segments.json`; `None` when the record was removed
    /// because nothing about the segment is pending.
    record: Option<SegmentState>,
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Start,
    StableSentence,
    NetworkWait,
    Stop,
    Delivered,
    NoTarget,
    SessionFailed,
    NoSpeech,
    StorageStop,
    StartRefusedByStorage,
    HotkeyUnavailable,
    RetryWithoutAudio,
}

impl Scenario {
    /// Build the state just before the transition under test, apply it, and return
    /// the snapshot and the segment whose durable record the row names.
    fn run(self) -> (Snapshot, Option<String>) {
        match self {
            Self::Start => {
                let mut h = Harness::new();
                let _ = h.start();
                (h.snapshot(), h.record(1))
            }
            Self::StableSentence => {
                let mut h = Harness::new();
                let index = h.start();
                h.script.session(index).stable("第一行：Rust");
                let snapshot = h.poll_until("a stable sentence", |snapshot| {
                    snapshot.recognition == Some(SessionState::Stable)
                });
                (snapshot, h.record(1))
            }
            Self::NetworkWait => {
                let mut h = Harness::new();
                let index = h.start();
                h.script
                    .session(index)
                    .set_state(SessionState::WaitingForNetwork);
                let snapshot = h.poll_until("a network wait", |snapshot| {
                    snapshot.recognition == Some(SessionState::WaitingForNetwork)
                });
                (snapshot, h.record(1))
            }
            Self::Stop => {
                let mut h = Harness::new();
                let _ = h.start();
                h.stop();
                let snapshot =
                    h.poll_until("the stop", |snapshot| snapshot.phase == Phase::Processing);
                (snapshot, h.record(1))
            }
            Self::Delivered => {
                let mut h = Harness::new();
                let index = h.start();
                h.stop();
                h.finish(index, "第一行：Rust 和 WASAPI。");
                let snapshot =
                    h.poll_until("the result", |snapshot| snapshot.phase == Phase::Result);
                assert!(
                    h.draft().contains("第一行：Rust 和 WASAPI。"),
                    "the result reached the editor: {:?}",
                    h.draft()
                );
                (snapshot, h.record(1))
            }
            Self::NoTarget => {
                let mut h = Harness::with(policy(u64::MAX / 4), false, Ok(()));
                let index = h.start();
                h.stop();
                h.finish(index, "第一行：Rust 和 WASAPI。");
                let snapshot =
                    h.poll_until("the result", |snapshot| snapshot.phase == Phase::Result);
                (snapshot, h.record(1))
            }
            Self::SessionFailed => {
                let mut h = Harness::new();
                let index = h.start();
                h.stop();
                h.fail(index, "the recognition engine lost the connection");
                let snapshot =
                    h.poll_until("the failure", |snapshot| snapshot.phase == Phase::Failure);
                (snapshot, h.record(1))
            }
            Self::NoSpeech => {
                let mut h = Harness::new();
                let index = h.start();
                h.stop();
                h.finish(index, "   ");
                let snapshot = h.poll_until("the no-speech result", |snapshot| {
                    snapshot.phase == Phase::NoSpeech
                });
                (snapshot, h.record(1))
            }
            Self::StorageStop => {
                let mut h = Harness::with(policy(1_000), true, Ok(()));
                let index = h.start();
                // Cross the pending-audio budget while the capture is still live.
                fs::write(h.directory(index).join("playback.pcm"), vec![7u8; 2_000]).unwrap();
                // `poll()` only advances state (#49); the desktop caller measures
                // storage on its own low-frequency tick.
                h.app.measure_storage();
                let snapshot = h.poll_until("the storage stop", |snapshot| {
                    snapshot.phase == Phase::Processing
                        && snapshot.detail.contains("storage budget was reached")
                });
                (snapshot, h.record(1))
            }
            Self::StartRefusedByStorage => {
                let mut h = Harness::with(policy(1_000), true, Ok(()));
                let index = h.start();
                fs::write(h.directory(index).join("playback.pcm"), vec![7u8; 2_000]).unwrap();
                h.stop();
                // The pending recording now exceeds the budget. Once it failed there
                // is no live capture or processing segment left to block the start
                // command, so the storage measurement itself must refuse it.
                h.fail(index, "the recognition engine lost the connection");
                h.poll_until("the failure", |snapshot| snapshot.phase == Phase::Failure);
                let before = h.script.count();
                h.app.dispatch(Control::Start);
                assert_eq!(
                    h.script.count(),
                    before,
                    "no second session was started for a refused capture"
                );
                (h.snapshot(), h.record(1))
            }
            Self::HotkeyUnavailable => {
                let mut h = Harness::with(
                    policy(u64::MAX / 4),
                    true,
                    Err("RegisterHotKey failed".into()),
                );
                h.app.dispatch(Control::Start);
                (h.snapshot(), h.record(1))
            }
            Self::RetryWithoutAudio => {
                let mut h = Harness::new();
                let index = h.start();
                h.stop();
                h.fail(index, "the recognition engine lost the connection");
                h.poll_until("the failure", |snapshot| snapshot.phase == Phase::Failure);
                h.app.dispatch(Control::Retry);
                (h.snapshot(), h.record(1))
            }
        }
    }

    fn expected(self) -> Expected {
        use SessionState::*;
        match self {
            Self::Start => Expected {
                phase: Phase::Capturing,
                recognition: Some(Capturing),
                detail: Detail::Adapter,
                delivery: None,
                record: Some(SegmentState::Capturing),
            },
            Self::StableSentence => Expected {
                phase: Phase::Capturing,
                recognition: Some(Stable),
                detail: Detail::Adapter,
                delivery: None,
                record: Some(SegmentState::Capturing),
            },
            Self::NetworkWait => Expected {
                phase: Phase::Capturing,
                recognition: Some(WaitingForNetwork),
                detail: Detail::Adapter,
                delivery: None,
                record: Some(SegmentState::WaitingForNetwork),
            },
            Self::Stop => Expected {
                phase: Phase::Processing,
                recognition: Some(AwaitingFinal),
                detail: Detail::Adapter,
                delivery: None,
                record: Some(SegmentState::Processing),
            },
            Self::Delivered => Expected {
                phase: Phase::Result,
                recognition: Some(Complete),
                detail: Detail::Result,
                delivery: Some(Delivery::Appended),
                record: None,
            },
            Self::NoTarget => Expected {
                phase: Phase::Result,
                recognition: Some(Complete),
                detail: Detail::NoTarget,
                delivery: Some(Delivery::Fallback),
                record: Some(SegmentState::Result),
            },
            Self::SessionFailed => Expected {
                phase: Phase::Failure,
                recognition: Some(Failed),
                detail: Detail::Failure,
                delivery: None,
                record: Some(SegmentState::Failed),
            },
            Self::NoSpeech => Expected {
                phase: Phase::NoSpeech,
                recognition: Some(Complete),
                detail: Detail::NoSpeech,
                delivery: None,
                record: None,
            },
            Self::StorageStop => Expected {
                phase: Phase::Processing,
                recognition: Some(AwaitingFinal),
                detail: Detail::StorageStop,
                delivery: None,
                record: Some(SegmentState::Processing),
            },
            Self::StartRefusedByStorage => Expected {
                phase: Phase::Failure,
                recognition: Some(Failed),
                detail: Detail::StartRefusal,
                delivery: None,
                record: Some(SegmentState::Failed),
            },
            Self::HotkeyUnavailable => Expected {
                phase: Phase::Failure,
                recognition: Some(Failed),
                detail: Detail::HotkeyRefusal,
                delivery: None,
                record: None,
            },
            Self::RetryWithoutAudio => Expected {
                // The failure is still the newest fact; a refused retry only explains
                // itself, it does not replace the failure with something else.
                phase: Phase::Failure,
                recognition: Some(Failed),
                detail: Detail::RetryRefusal,
                delivery: None,
                record: Some(SegmentState::Failed),
            },
        }
    }
}

#[test]
fn every_segment_transition_leaves_the_documented_phase_recognition_sentence_and_record() {
    let rows = [
        Scenario::Start,
        Scenario::StableSentence,
        Scenario::NetworkWait,
        Scenario::Stop,
        Scenario::Delivered,
        Scenario::NoTarget,
        Scenario::SessionFailed,
        Scenario::NoSpeech,
        Scenario::StorageStop,
        Scenario::StartRefusedByStorage,
        Scenario::HotkeyUnavailable,
        Scenario::RetryWithoutAudio,
    ];
    for scenario in rows {
        let expected = scenario.expected();
        let (snapshot, record) = scenario.run();
        assert_eq!(
            snapshot.phase, expected.phase,
            "{scenario:?}: phase; snapshot {snapshot:?}"
        );
        assert_eq!(
            snapshot.recognition, expected.recognition,
            "{scenario:?}: recognition; snapshot {snapshot:?}"
        );
        expected.detail.holds(&snapshot.detail);
        assert_eq!(
            snapshot.delivery, expected.delivery,
            "{scenario:?}: delivery; snapshot {snapshot:?}"
        );
        assert_eq!(
            record.as_deref(),
            expected.record.map(SegmentState::name),
            "{scenario:?}: durable record state; snapshot {snapshot:?}"
        );
    }
}

/// The storage stop's reason is not a progress tick: later status refreshes must not
/// turn it back into a normal capture sentence. This is the "stopped keeps its line"
/// rule, exercised through the one path that keeps a note beside the adapter's own.
#[test]
fn a_storage_stop_reason_survives_later_status_refreshes() {
    let mut h = Harness::with(policy(1_000), true, Ok(()));
    let index = h.start();
    fs::write(h.directory(index).join("playback.pcm"), vec![7u8; 2_000]).unwrap();
    // `poll()` only advances state (#49); measure explicitly, like the desktop tick.
    h.app.measure_storage();
    h.poll_until("the storage stop", |snapshot| {
        snapshot.phase == Phase::Processing && snapshot.detail.contains("storage budget")
    });
    // The adapter keeps reporting its own progress for several ticks afterwards.
    for _ in 0..5 {
        h.app.poll();
        thread::sleep(Duration::from_millis(2));
    }
    let snapshot = h.snapshot();
    assert!(
        snapshot.detail.contains("storage budget was reached"),
        "the reason capture stopped must survive a status refresh: {snapshot:?}"
    );
}

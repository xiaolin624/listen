//! #49: `poll()` advances state only; the disk measurement is a separate,
//! caller-driven action.
//!
//! The measurement is injected through [`MeasureBytes`], so "a poll never measures"
//! is observable as a call count instead of being inferred from timing, and no test
//! depends on this machine's real disk or real free space.
//!
//! What stays frozen: the published values are still the ones the panel,
//! `status.txt` and the storage bounds read; they now come from the last
//! measurement, which is exactly what the ticket allows ("data may come from the
//! last measurement").
use listen::{
    AppResult, Application, Control, Notice, Phase, SegmentState, Snapshot,
    storage::{FreeSpace, MeasureBytes, MeasuredBytes, StorageLevel, StoragePolicy},
    testing::{ScriptedControl, ScriptedSessions},
};
use std::{
    fs,
    path::{Path, PathBuf},
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
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-storage-measurement-{}-{}-{}",
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

/// A counted fake walk: it records every measurement and answers the bytes the test
/// currently wants. It never touches the filesystem.
struct FakeMeasure {
    calls: AtomicUsize,
    bytes: Mutex<MeasuredBytes>,
}

impl FakeMeasure {
    fn new(pending_audio_bytes: u64, text_bytes: u64) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            bytes: Mutex::new(MeasuredBytes {
                pending_audio_bytes,
                text_bytes,
            }),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn set(&self, pending_audio_bytes: u64, text_bytes: u64) {
        *self.bytes.lock().unwrap() = MeasuredBytes {
            pending_audio_bytes,
            text_bytes,
        };
    }
}

impl MeasureBytes for FakeMeasure {
    fn measure(&self, _runtime: &Path) -> MeasuredBytes {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.bytes.lock().unwrap()
    }
}

/// A generous free-space probe. The free-disk floor is zero in these tests, so the
/// reported free space never decides anything.
struct GenerousFreeSpace;
impl FreeSpace for GenerousFreeSpace {
    fn free_bytes(&self, _directory: &Path) -> AppResult<u64> {
        Ok(1 << 40)
    }
}

struct Harness {
    /// Kept only so the temporary runtime directory lives exactly as long as the
    /// application; the fake measurement never reads it.
    _runtime: Runtime,
    app: Application,
    script: ScriptedControl,
    measure: Arc<FakeMeasure>,
}

/// A small pending-audio budget (the warning line is four fifths of it) and no
/// free-disk floor: the measurement under test decides everything by itself.
fn budget_policy() -> StoragePolicy {
    StoragePolicy {
        pending_audio_budget_bytes: 1_000,
        free_disk_floor_bytes: 0,
        ..StoragePolicy::default()
    }
}

impl Harness {
    fn new(pending_audio_bytes: u64, text_bytes: u64) -> Self {
        let runtime = Runtime::new();
        let adapter = ScriptedSessions::new().with_backend("controlled storage recognition");
        let script = adapter.control();
        let measure = Arc::new(FakeMeasure::new(pending_audio_bytes, text_bytes));
        let app =
            Application::with_recognition_sessions(Box::new(adapter), runtime.0.clone(), Ok(()))
                .with_storage_policy(budget_policy())
                .with_free_space(Arc::new(GenerousFreeSpace))
                .with_storage_measure(measure.clone());
        Self {
            _runtime: runtime,
            app,
            script,
            measure,
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.app.snapshot()
    }
    fn storage(&self) -> listen::storage::StorageStatus {
        self.snapshot()
            .storage
            .expect("a measurement is published at construction")
    }
    fn start(&mut self) -> usize {
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
    fn poll(&mut self, times: usize) {
        for _ in 0..times {
            self.app.poll();
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
}

/// The count a poll must never change, and the count one explicit measurement adds.
/// The live-capture loop is covered as well, because `Phase::Capturing`/`Processing`
/// is where the desktop poll runs at 10 ms.
#[test]
fn consecutive_polls_never_measure_the_runtime_directory() {
    let mut h = Harness::new(100, 0);
    let constructed = h.measure.calls();
    assert!(
        constructed >= 1,
        "the first measurement is published while the application is constructed"
    );

    h.poll(50);
    assert_eq!(
        h.measure.calls(),
        constructed,
        "50 idle polls must not measure the runtime directory"
    );

    // A live capture makes the desktop timer switch to its 10 ms tick; polling must
    // still not measure. `start` itself is an explicit action and may measure.
    h.start();
    let after_start = h.measure.calls();
    h.poll(50);
    assert_eq!(
        h.measure.calls(),
        after_start,
        "50 polls of a live capture must not measure the runtime directory"
    );

    h.app.measure_storage();
    assert_eq!(
        h.measure.calls(),
        after_start + 1,
        "one explicit measurement is exactly one walk"
    );
}

/// Polling publishes nothing new; only the explicit measurement does. The values
/// stay readable in between, which is what the panel and `status.txt` read.
#[test]
fn only_an_explicit_measurement_changes_the_published_storage_status() {
    let mut h = Harness::new(100, 5);
    assert_eq!(h.storage().pending_audio_bytes, 100);
    assert_eq!(h.storage().text_bytes, 5);
    assert_eq!(h.snapshot().notice, None);

    h.measure.set(900, 7);
    h.poll(25);
    let stale = h.storage();
    assert_eq!(
        stale.pending_audio_bytes, 100,
        "a poll keeps publishing the last measurement: {stale:?}"
    );
    assert_eq!(stale.text_bytes, 5, "{stale:?}");
    assert_eq!(
        h.snapshot().notice,
        None,
        "a poll must not re-run the storage judgement either"
    );

    h.app.measure_storage();
    let fresh = h.storage();
    assert_eq!(
        fresh.pending_audio_bytes, 900,
        "an explicit measurement publishes the new value: {fresh:?}"
    );
    assert_eq!(fresh.text_bytes, 7, "{fresh:?}");
    assert_eq!(
        h.snapshot().notice,
        Some(Notice::StorageWarning),
        "the approaching-limit warning is published by the measurement"
    );
}

/// The user-visible behavior that must survive #49: after a measurement refresh the
/// approaching-limit warning is still there and the segment the panel offers for
/// explicit deletion is still there. It is the same snapshot both read.
#[test]
fn a_measurement_refresh_keeps_the_approaching_limit_and_the_deletable_segment() {
    let mut h = Harness::new(100, 0);
    h.start();
    h.stop();
    let stopped = h.poll_until("the stopped segment", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    assert!(
        stopped
            .segments
            .iter()
            .any(|segment| segment.state == SegmentState::Processing),
        "a stopped segment is offered for explicit deletion: {stopped:?}"
    );

    // The runtime directory is not touched by any of this: the measurement is the
    // injected value, exactly like the real walk would report 850 bytes of audio.
    h.measure.set(850, 0);
    h.app.measure_storage();
    let refreshed = h.snapshot();
    assert_eq!(refreshed.notice, Some(Notice::StorageWarning));
    assert_eq!(
        refreshed.storage.as_ref().map(|status| status.level()),
        Some(StorageLevel::Warning)
    );
    assert!(
        refreshed
            .segments
            .iter()
            .any(|segment| { segment.sequence == 1 && segment.state == SegmentState::Processing }),
        "the segment the panel can delete survives the refresh: {refreshed:?}"
    );

    h.poll(10);
    assert_eq!(
        h.snapshot().notice,
        Some(Notice::StorageWarning),
        "the last measurement stays published across polls"
    );
}

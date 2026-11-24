//! 精修 (refine) at the application boundary.
//!
//! Every test here drives `Application` with fake audio, a fake local transcriber,
//! a fake target editor and a fake refine port. The real DeepSeek API is never
//! touched; the port boundary is exactly where "only the transcript text leaves the
//! machine" can be observed, and where the append-then-refine safety rules are
//! proven branch by branch.

use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, LocalTranscriber, Phase,
    PlaybackAudio, Recording, Refine, RefineText, Target, TargetSelection,
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

/// The text the fake local transcriber produces. It contains the kind of error the
/// refine step exists for: an English technical name heard as words.
const TRANSCRIPT: &str = "我们使用 deep sick harnessness 来做测试。";
const REFINED: &str = "我们使用 DeepSeek Harness 来做测试。";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-refine-{}-{}-{}",
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

struct Playback;
struct Playing(PathBuf);
impl PlaybackAudio for Playback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let path = directory.join("audio.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for _ in 0..160 {
            writer.write_sample(1234_i16).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
        Ok(Box::new(Playing(path)))
    }
}
impl Recording for Playing {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        Ok(CapturedAudio {
            path: self.0,
            description: "test render device".into(),
        })
    }
}

struct LocalModel;
impl LocalTranscriber for LocalModel {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        let wav = hound::WavReader::open(&audio.path).map_err(|e| e.to_string())?;
        if wav.duration() == 0 {
            return Err("no playback audio reached the local model".into());
        }
        Ok(TRANSCRIPT.into())
    }
}

/// Returns the transcript on the first capture, then a silent result. Used to prove
/// that `NoSpeech` keeps the previous result but does not leave refine available.
struct SpeechThenSilence(AtomicUsize);
impl LocalTranscriber for SpeechThenSilence {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(TRANSCRIPT.into())
        } else {
            Ok(" \r\n\t".into())
        }
    }
}

fn await_phase(app: &mut Application, phase: Phase) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if state.phase == phase {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "expected {phase:?}, observed {state:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Poll until the refine state satisfies `done`. Keeps the test off the internal
/// worker channel: only the user-visible `Snapshot::refine` is observed.
fn await_refine(
    app: &mut Application,
    done: impl Fn(&Refine) -> bool,
    what: &str,
) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let state = app.snapshot();
        if done(&state.refine) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "expected {what}, observed refine={:?} detail={}",
            state.refine,
            state.detail
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn await_refined(app: &mut Application) -> listen::Snapshot {
    await_refine(
        app,
        |refine| !matches!(refine, Refine::Running),
        "a finished refine",
    )
}

/// Capture once and return the complete result. The fake model is instant, so the
/// worker only needs a few polls.
fn capture_result(app: &mut Application) -> listen::Snapshot {
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    await_phase(app, Phase::Result)
}

/// A fake DeepSeek port. It records exactly what it was asked to refine, so a test
/// can prove that nothing but the transcript text crosses this boundary.
struct FakeRefine {
    answer: Result<String, String>,
    availability: Result<(), String>,
    seen: Mutex<Vec<String>>,
    /// When set, `refine` blocks until the receiver yields, which makes the
    /// `Refine::Running` window observable.
    gate: Mutex<Option<mpsc::Receiver<()>>>,
}

impl FakeRefine {
    fn answering(answer: &str) -> Self {
        Self {
            answer: Ok(answer.into()),
            availability: Ok(()),
            seen: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }
    fn failing(error: &str) -> Self {
        Self {
            answer: Err(error.into()),
            availability: Ok(()),
            seen: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }
    fn unavailable(reason: &str) -> Self {
        Self {
            answer: Ok(REFINED.into()),
            availability: Err(reason.into()),
            seen: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }
    fn blocked(gate: mpsc::Receiver<()>) -> Self {
        Self {
            answer: Ok(REFINED.into()),
            availability: Ok(()),
            seen: Mutex::new(Vec::new()),
            gate: Mutex::new(Some(gate)),
        }
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl RefineText for FakeRefine {
    fn availability(&self) -> Result<(), String> {
        self.availability.clone()
    }
    fn refine(&self, text: &str) -> AppResult<String> {
        self.seen.lock().unwrap().push(text.to_string());
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.recv_timeout(Duration::from_secs(5));
        }
        self.answer.clone()
    }
}

/// Failure injection for one editor. Everything is off by default, so a test that
/// does not care about a lost or slow write behaves exactly as before.
#[derive(Clone, Default)]
struct Gate {
    /// Store the replacement, then wait before reporting the write, so the test can
    /// observe the durable `started` marker in the window between write and
    /// confirmation.
    block_after_write: Arc<AtomicBool>,
    /// The write reached the block (readiness signal for the test).
    entered: Arc<AtomicBool>,
    /// The test releases the blocked write.
    release: Arc<AtomicBool>,
    /// Store the replacement, then report a lost response.
    lose_response: Arc<AtomicBool>,
}

/// Releases a blocked fake write even when the test fails, so a red test cannot keep
/// holding the process-wide target writer and hang the rest of the test binary.
struct ReleaseOnDrop(Gate);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release.store(true, Ordering::SeqCst);
    }
}

struct Editors {
    focused: Option<usize>,
    drafts: [String; 2],
    available: [bool; 2],
    /// Report the draft with CRLF line endings, as some Windows editors do, while
    /// storing what `replace` was asked for. The refine comparison must not depend
    /// on line endings.
    crlf: bool,
    replace_calls: usize,
    gate: Gate,
}

impl Editors {
    fn new(first: &str) -> Self {
        Self::with_gate(first, Gate::default())
    }
    fn with_gate(first: &str, gate: Gate) -> Self {
        Self {
            focused: Some(0),
            drafts: [first.into(), String::new()],
            available: [true; 2],
            crlf: false,
            replace_calls: 0,
            gate,
        }
    }
}

struct Desktop(Arc<Mutex<Editors>>);
struct BoundEditor {
    editors: Arc<Mutex<Editors>>,
    selected: usize,
    gate: Gate,
}

impl TargetSelection for Desktop {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let desktop = self.0.lock().unwrap();
        let Some(selected) = desktop.focused else {
            return Ok(None);
        };
        if !desktop.available[selected] {
            return Err("The original target was destroyed before binding".into());
        }
        Ok(Some(Box::new(BoundEditor {
            editors: self.0.clone(),
            selected,
            gate: desktop.gate.clone(),
        })))
    }
}

impl Target for BoundEditor {
    fn content(&mut self) -> AppResult<String> {
        let desktop = self.editors.lock().unwrap();
        if !desktop.available[self.selected] {
            return Err("The original target was destroyed".into());
        }
        let draft = &desktop.drafts[self.selected];
        Ok(if desktop.crlf {
            draft.replace('\n', "\r\n")
        } else {
            draft.clone()
        })
    }

    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut desktop = self.editors.lock().unwrap();
        desktop.replace_calls += 1;
        if !desktop.available[self.selected] {
            return Err("The original target was destroyed".into());
        }
        let observed = if desktop.crlf {
            desktop.drafts[self.selected].replace('\n', "\r\n")
        } else {
            desktop.drafts[self.selected].clone()
        };
        if observed != expected {
            return Err(
                "The target draft changed during output; no replacement was attempted".into(),
            );
        }
        desktop.drafts[self.selected] = replacement.replace("\r\n", "\n");
        drop(desktop);
        // The write already landed; only the response is delayed or lost.
        if self.gate.block_after_write.load(Ordering::SeqCst) {
            self.gate.entered.store(true, Ordering::SeqCst);
            while !self.gate.release.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
        }
        if self.gate.lose_response.load(Ordering::SeqCst) {
            return Err("The complete target output could not be confirmed".into());
        }
        Ok(())
    }
}

fn refine_application(
    runtime: &Runtime,
    editors: &Arc<Mutex<Editors>>,
    refine: &Arc<FakeRefine>,
) -> Application {
    Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())))
    .with_refine(refine.clone())
}

/// One durable delivery record from `delivery.json`, if the file can be read.
fn delivery_record(runtime: &Runtime, sequence: u64) -> Option<serde_json::Value> {
    let bytes = fs::read(runtime.0.join("delivery.json")).ok()?;
    let index: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    index["segments"]
        .as_array()?
        .iter()
        .find(|record| record["sequence"] == sequence)
        .cloned()
}

#[test]
fn a_successful_refine_replaces_the_appended_transcript_and_reads_it_back() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    let result = capture_result(&mut app);
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(result.refine, Refine::Available);
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        format!("原有草稿\n{TRANSCRIPT}")
    );

    app.dispatch(Control::Refine);
    assert_eq!(app.snapshot().refine, Refine::Running);
    let refined = await_refined(&mut app);

    assert_eq!(refined.refine, Refine::Applied);
    assert_eq!(
        refined.text, REFINED,
        "the current result becomes the refined text"
    );
    assert_eq!(refined.refined_text, None);
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        format!("原有草稿\n{REFINED}"),
        "only the appended transcript is replaced; the user's earlier draft stays"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        REFINED
    );
    assert_eq!(editors.lock().unwrap().replace_calls, 2);
    assert!(refined.detail.contains("read back"), "{}", refined.detail);
}

#[test]
fn the_refine_port_receives_exactly_the_transcript_text_and_nothing_else() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    app.dispatch(Control::Refine);
    await_refined(&mut app);

    let seen = refine.seen();
    assert_eq!(seen, vec![TRANSCRIPT.to_string()]);
    assert!(
        !seen[0].contains(".wav") && !seen[0].contains('\\'),
        "no audio path may cross the refine boundary: {seen:?}"
    );
}

#[test]
fn an_empty_original_draft_is_replaced_with_only_the_refined_text() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(capture_result(&mut app).delivery, Some(Delivery::Appended));
    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert_eq!(refined.refine, Refine::Applied);
    assert_eq!(editors.lock().unwrap().drafts[0], REFINED);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        REFINED
    );
}

#[test]
fn line_endings_reported_by_the_target_do_not_prevent_a_safe_backfill() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    editors.lock().unwrap().crlf = true;
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(capture_result(&mut app).delivery, Some(Delivery::Appended));
    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert_eq!(refined.refine, Refine::Applied);
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        format!("原有草稿\n{REFINED}")
    );
}

#[test]
fn a_target_edited_by_the_user_is_never_overwritten_and_keeps_the_refined_text_in_the_panel() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    let edited = {
        let mut desktop = editors.lock().unwrap();
        desktop.drafts[0].push_str("\n这是我手改的内容");
        desktop.drafts[0].clone()
    };
    let writes_before = editors.lock().unwrap().replace_calls;

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert!(
        matches!(refined.refine, Refine::NotApplied(_)),
        "{:?}",
        refined.refine
    );
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        edited,
        "the user's edit must be preserved byte for byte"
    );
    assert_eq!(
        editors.lock().unwrap().replace_calls,
        writes_before,
        "a changed target must never be written"
    );
    assert_eq!(refined.refined_text.as_deref(), Some(REFINED));
    assert_eq!(
        refined.text, TRANSCRIPT,
        "the local result stays the current result"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    assert!(
        refined.detail.contains("edited"),
        "the state must explain why it was not written back: {}",
        refined.detail
    );
}

#[test]
fn a_target_that_cannot_be_read_after_delivery_is_not_written_and_keeps_the_refined_text() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    let writes_before = editors.lock().unwrap().replace_calls;
    // The editor window went away after the result was delivered.
    editors.lock().unwrap().available[0] = false;

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert!(
        matches!(refined.refine, Refine::NotApplied(_)),
        "{:?}",
        refined.refine
    );
    assert_eq!(editors.lock().unwrap().replace_calls, writes_before);
    assert_eq!(refined.refined_text.as_deref(), Some(REFINED));
    assert_eq!(refined.text, TRANSCRIPT);
    assert!(
        refined.detail.contains("could not be read"),
        "{}",
        refined.detail
    );
}

#[test]
fn a_failed_refine_keeps_the_local_result_and_never_appends_it_again() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::failing(
        "DeepSeek API returned HTTP 401: Authentication Fails",
    ));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    let delivered = editors.lock().unwrap().drafts[0].clone();
    let writes_before = editors.lock().unwrap().replace_calls;

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert!(
        matches!(&refined.refine, Refine::Failed(reason) if reason.contains("HTTP 401")),
        "{:?}",
        refined.refine
    );
    assert_eq!(refined.text, TRANSCRIPT);
    assert_eq!(refined.refined_text, None);
    assert_eq!(refined.delivery, Some(Delivery::Appended));
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        delivered,
        "a failed refine must neither re-append nor restore old text"
    );
    assert_eq!(editors.lock().unwrap().replace_calls, writes_before);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );
    assert!(
        refined.detail.contains("Refine failed"),
        "{}",
        refined.detail
    );
    assert!(refined.detail.contains("unchanged"), "{}", refined.detail);
}

#[test]
fn a_refine_write_persists_its_start_before_the_target_write_and_a_crash_then_stays_uncertain() {
    let runtime = Runtime::new();
    let gate = Gate::default();
    let editors = Arc::new(Mutex::new(Editors::with_gate("原有草稿", gate.clone())));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(capture_result(&mut app).delivery, Some(Delivery::Appended));
    assert_eq!(
        delivery_record(&runtime, 1).expect("a durable delivery record")["state"],
        "appended"
    );

    // The target applies the refined replacement and then waits before reporting the
    // write: the window in which a crash loses only the confirmation. The guard
    // releases the fake write even if an assertion below fails.
    gate.block_after_write.store(true, Ordering::SeqCst);
    let _release = ReleaseOnDrop(gate.clone());
    app.dispatch(Control::Refine);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gate.entered.load(Ordering::SeqCst) {
        app.poll();
        assert!(
            Instant::now() < deadline,
            "the refine write never reached the target"
        );
        thread::sleep(Duration::from_millis(2));
    }
    // The write is now in flight and not confirmed. The durable record must already
    // say that a write started, and it must describe the complete draft it replaces.
    let started = delivery_record(&runtime, 1).expect("a durable delivery record");
    assert_eq!(
        started["state"], "started",
        "the started marker must be persisted before the target write: {started}"
    );
    assert_eq!(
        started["before"],
        format!("原有草稿\n{TRANSCRIPT}"),
        "the started marker describes the complete draft the write replaces: {started}"
    );
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        format!("原有草稿\n{REFINED}"),
        "the write landed, so this is exactly the 'written but not confirmed' window"
    );

    // The process now ends in that window. Nothing can be proven, so the durable
    // record stays `started` (terminal) and a restarted application must neither
    // rewrite the old target nor claim a panel-only fallback.
    //
    // Boundary, stated in docs/issue-44-verification.md: a confirmed append already
    // removed this segment's index record and text file, so the restart cannot attach
    // the `started` record to a segment entry. The transcript is still the current
    // result from current-result.txt and stays in the panel; the write is never
    // replayed and the record is never downgraded.
    let restarted_editors = Arc::new(Mutex::new(Editors::new("新进程中的目标草稿")));
    let restarted_refine = Arc::new(FakeRefine::answering("再次精修的结果"));
    let mut restarted = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(restarted_editors.clone())))
    .with_refine(restarted_refine.clone());
    let recovered = restarted.snapshot();
    assert_eq!(
        recovered.text, TRANSCRIPT,
        "the transcript is still the current result in the panel: {recovered:?}"
    );
    assert!(
        recovered.segments.is_empty(),
        "a confirmed append leaves no segment record; the started record is the only \
         trace of the unconfirmed refine write: {recovered:?}"
    );
    assert_eq!(
        delivery_record(&runtime, 1).expect("the durable record")["state"],
        "started",
        "recovery never rewrites or downgrades the started marker"
    );

    // Even a refine requested after the restart only rewrites the panel: the old
    // target memory is gone, so the target is never written again.
    restarted.dispatch(Control::Refine);
    let refined_again = await_refined(&mut restarted);
    assert_eq!(refined_again.refine, Refine::Applied);
    assert_eq!(
        restarted_editors.lock().unwrap().replace_calls,
        0,
        "a restart never writes the old target again"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        "再次精修的结果",
        "the restarted refine only updates the local result"
    );
}

#[test]
fn a_refine_write_with_a_lost_response_is_reported_and_recorded_as_uncertain() {
    let runtime = Runtime::new();
    let gate = Gate::default();
    let editors = Arc::new(Mutex::new(Editors::with_gate("原有草稿", gate.clone())));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(capture_result(&mut app).delivery, Some(Delivery::Appended));
    let writes_before = editors.lock().unwrap().replace_calls;
    gate.lose_response.store(true, Ordering::SeqCst);

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert_eq!(
        refined.refine.name(),
        "unconfirmed",
        "a write whose response was lost is not claimed and not denied: {:?}",
        refined.refine
    );
    assert!(
        refined.detail.contains("could not be confirmed"),
        "{}",
        refined.detail
    );
    assert_eq!(
        refined.refined_text.as_deref(),
        Some(REFINED),
        "the refined text stays available in the panel"
    );
    assert_eq!(refined.text, TRANSCRIPT, "the local result is unchanged");
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        format!("原有草稿\n{REFINED}"),
        "the write really landed; only its confirmation was lost"
    );
    let record = delivery_record(&runtime, 1).expect("a durable delivery record");
    assert_eq!(record["state"], "uncertain", "{record}");

    // The durable record is terminal for this segment, and the refine memory was
    // consumed, so a second request cannot write the target again.
    app.dispatch(Control::Refine);
    app.poll();
    assert_eq!(
        editors.lock().unwrap().replace_calls,
        writes_before + 1,
        "an unconfirmed refine write is never repeated"
    );
}

#[test]
fn a_refined_result_is_recorded_and_never_written_again_after_a_restart() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(capture_result(&mut app).delivery, Some(Delivery::Appended));
    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);
    assert_eq!(refined.refine, Refine::Applied);

    let record = delivery_record(&runtime, 1).expect("a durable delivery record");
    assert_eq!(record["state"], "appended", "{record}");
    assert_eq!(
        record["before"],
        format!("原有草稿\n{TRANSCRIPT}"),
        "{record}"
    );
    assert_eq!(
        record["after"],
        format!("原有草稿\n{REFINED}"),
        "the confirmed refine is recorded around the write: {record}"
    );

    drop(app);
    let restarted_editors = Arc::new(Mutex::new(Editors::new("新进程中的目标草稿")));
    let restarted_refine = Arc::new(FakeRefine::answering("再次精修的结果"));
    let mut restarted = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(restarted_editors.clone())))
    .with_refine(restarted_refine.clone());
    let recovered = restarted.snapshot();
    assert_eq!(
        recovered.text, REFINED,
        "the refined text is the current result after a restart: {recovered:?}"
    );
    // The confirmed append removed the segment's index record, so on a restart the
    // refined text comes back as a complete result the index did not describe. Its
    // delivery state cannot be proven from the index, so it is reported conservatively
    // as uncertain — never as a panel-only fallback and never as a fresh delivery.
    assert!(
        recovered
            .segments
            .iter()
            .all(|segment| segment.delivery != Some(Delivery::Fallback)),
        "a restart never claims a panel-only fallback for a result a target may hold: {recovered:?}"
    );
    assert_eq!(
        delivery_record(&runtime, 1).expect("the durable record")["after"],
        format!("原有草稿\n{REFINED}"),
        "the confirmed refine survives the restart"
    );

    // A refine requested after the restart only rewrites the panel/current result: the
    // old target memory is gone and the old target is never written again.
    restarted.dispatch(Control::Refine);
    let refined_again = await_refined(&mut restarted);
    assert_eq!(refined_again.refine, Refine::Applied);
    for _ in 0..5 {
        restarted.poll();
    }
    assert_eq!(
        restarted_editors.lock().unwrap().replace_calls,
        0,
        "a restart never writes the old target again"
    );
}

#[test]
fn a_refine_whose_started_marker_cannot_be_saved_does_not_write_the_target() {
    let runtime = Runtime::new();
    // A record from a future version is never rewritten, so the `started` marker
    // cannot be persisted at all.
    fs::write(
        runtime.0.join("delivery.json"),
        r#"{"version":99,"segments":[]}"#,
    )
    .unwrap();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    assert_eq!(
        capture_result(&mut app).delivery,
        Some(Delivery::Appended),
        "a normal append does not require the durable marker"
    );
    let written = editors.lock().unwrap().drafts[0].clone();
    let writes_before = editors.lock().unwrap().replace_calls;

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert!(
        matches!(refined.refine, Refine::NotApplied(_)),
        "{:?}",
        refined.refine
    );
    assert!(
        refined.detail.contains("delivery record"),
        "the refusal names the durable record that could not be written: {}",
        refined.detail
    );
    assert!(
        refined.detail.contains("not written"),
        "the refusal says nothing was written: {}",
        refined.detail
    );
    assert_eq!(
        editors.lock().unwrap().drafts[0],
        written,
        "an unmarked refine write is refused, so the target is untouched"
    );
    assert_eq!(
        editors.lock().unwrap().replace_calls,
        writes_before,
        "no write was attempted"
    );
    assert_eq!(refined.refined_text.as_deref(), Some(REFINED));
    assert_eq!(refined.text, TRANSCRIPT);
    assert_eq!(
        fs::read(runtime.0.join("delivery.json")).unwrap(),
        br#"{"version":99,"segments":[]}"#,
        "the future record is never downgraded"
    );
}

#[test]
fn a_fallback_result_with_no_target_is_refined_into_the_panel_and_the_current_result() {
    let runtime = Runtime::new();
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_refine(refine.clone());

    let result = capture_result(&mut app);
    assert_eq!(result.delivery, Some(Delivery::Fallback));
    assert_eq!(result.refine, Refine::Available);

    app.dispatch(Control::Refine);
    let refined = await_refined(&mut app);

    assert_eq!(refined.refine, Refine::Applied);
    assert_eq!(refined.text, REFINED);
    assert_eq!(refined.refined_text, None);
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        REFINED
    );
    assert!(
        refined.detail.contains("no writable target"),
        "{}",
        refined.detail
    );
}

#[test]
fn no_refine_port_refuses_explicitly_and_leaves_the_result_and_target_untouched() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(LocalModel),
        runtime.0.clone(),
        Ok(()),
    )
    .with_targets(Arc::new(Desktop(editors.clone())));

    let result = capture_result(&mut app);
    let reason = match &result.refine {
        Refine::Unavailable(reason) => reason.clone(),
        other => panic!("expected refine to be unavailable, got {other:?}"),
    };
    assert!(reason.contains("connector"), "{reason}");
    let delivered = editors.lock().unwrap().drafts[0].clone();

    app.dispatch(Control::Refine);
    let refused = app.snapshot();
    assert!(matches!(refused.refine, Refine::Unavailable(_)));
    assert!(
        refused.detail.contains("Refine is unavailable"),
        "{}",
        refused.detail
    );
    assert_eq!(refused.text, TRANSCRIPT);
    assert_eq!(editors.lock().unwrap().drafts[0], delivered);
}

#[test]
fn a_port_whose_availability_fails_refuses_with_its_own_reason() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::unavailable(
        "No DeepSeek API key is configured in DEEPSEEK_API_KEY",
    ));
    let mut app = refine_application(&runtime, &editors, &refine);

    let result = capture_result(&mut app);
    assert!(
        matches!(&result.refine, Refine::Unavailable(reason) if reason.contains("DEEPSEEK_API_KEY")),
        "{:?}",
        result.refine
    );
    let delivered = editors.lock().unwrap().drafts[0].clone();

    app.dispatch(Control::Refine);
    let refused = app.snapshot();
    assert!(
        refused.detail.contains("DEEPSEEK_API_KEY"),
        "{}",
        refused.detail
    );
    assert!(
        refine.seen().is_empty(),
        "an unavailable port is never called"
    );
    assert_eq!(editors.lock().unwrap().drafts[0], delivered);
    assert_eq!(refused.text, TRANSCRIPT);
}

#[test]
fn refine_is_unavailable_while_capturing_or_processing_and_never_calls_the_port() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    app.dispatch(Control::Refine);
    let capturing = app.snapshot();
    assert!(
        matches!(&capturing.refine, Refine::Unavailable(reason) if reason.contains("captur")),
        "{:?}",
        capturing.refine
    );
    assert!(
        capturing.detail.contains("Refine is unavailable"),
        "{}",
        capturing.detail
    );
    assert_eq!(capturing.text, "");

    app.dispatch(Control::Stop);
    assert_eq!(app.snapshot().phase, Phase::Processing);
    app.dispatch(Control::Refine);
    let processing = app.snapshot();
    assert!(
        matches!(&processing.refine, Refine::Unavailable(reason) if reason.contains("processing")),
        "{:?}",
        processing.refine
    );
    assert!(
        processing.detail.contains("Refine is unavailable"),
        "{}",
        processing.detail
    );
    assert!(refine.seen().is_empty());

    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.refine, Refine::Available);
}

#[test]
fn starting_a_capture_is_refused_while_a_refine_is_in_flight() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let (release, gate) = mpsc::channel();
    let refine = Arc::new(FakeRefine::blocked(gate));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    app.dispatch(Control::Refine);
    assert_eq!(app.snapshot().refine, Refine::Running);

    app.dispatch(Control::Start);
    let refused = app.snapshot();
    assert_eq!(
        refused.phase,
        Phase::Result,
        "no capture may start mid-refine"
    );
    assert!(
        refused.detail.contains("Refine is still running"),
        "{}",
        refused.detail
    );
    assert_eq!(refused.text, TRANSCRIPT);

    app.dispatch(Control::Toggle);
    assert_eq!(app.snapshot().phase, Phase::Result);
    assert!(app.snapshot().detail.contains("Refine is still running"));

    release.send(()).unwrap();
    let refined = await_refine(
        &mut app,
        |refine| matches!(refine, Refine::Applied),
        "applied",
    );
    assert_eq!(refined.text, REFINED);

    // A second refine request has nothing to do: the refined text is already applied.
    app.dispatch(Control::Refine);
    assert_eq!(app.snapshot().refine, Refine::Applied);
    assert!(
        app.snapshot().detail.contains("already applied"),
        "{}",
        app.snapshot().detail
    );
    assert_eq!(refine.seen().len(), 1, "the port is asked exactly once");
}

#[test]
fn a_second_refine_request_while_running_is_refused_without_calling_the_port_again() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let (release, gate) = mpsc::channel();
    let refine = Arc::new(FakeRefine::blocked(gate));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    app.dispatch(Control::Refine);
    app.dispatch(Control::Refine);
    assert_eq!(app.snapshot().refine, Refine::Running);
    assert!(
        app.snapshot().detail.contains("already running"),
        "{}",
        app.snapshot().detail
    );

    release.send(()).unwrap();
    await_refine(
        &mut app,
        |refine| matches!(refine, Refine::Applied),
        "applied",
    );
    assert_eq!(refine.seen().len(), 1);
}

#[test]
fn a_new_capture_clears_the_refine_state_and_a_new_result_offers_refine_again() {
    let runtime = Runtime::new();
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = refine_application(&runtime, &editors, &refine);

    capture_result(&mut app);
    app.dispatch(Control::Refine);
    await_refine(
        &mut app,
        |refine| matches!(refine, Refine::Applied),
        "applied",
    );

    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing);
    assert!(matches!(capturing.refine, Refine::Unavailable(_)));
    assert_eq!(capturing.refined_text, None);

    app.dispatch(Control::Stop);
    let result = await_phase(&mut app, Phase::Result);
    assert_eq!(result.refine, Refine::Available);
    assert_eq!(result.refined_text, None);
}

#[test]
fn no_speech_keeps_the_previous_result_but_does_not_leave_refine_available() {
    let runtime = Runtime::new();
    let model = Arc::new(SpeechThenSilence(AtomicUsize::new(0)));
    let editors = Arc::new(Mutex::new(Editors::new("原有草稿")));
    let refine = Arc::new(FakeRefine::answering(REFINED));
    let mut app = Application::new(Arc::new(Playback), model, runtime.0.clone(), Ok(()))
        .with_targets(Arc::new(Desktop(editors.clone())))
        .with_refine(refine.clone());

    assert_eq!(capture_result(&mut app).refine, Refine::Available);
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let silent = await_phase(&mut app, Phase::NoSpeech);

    assert_eq!(silent.text, TRANSCRIPT, "the previous result is kept");
    assert!(
        matches!(&silent.refine, Refine::Unavailable(reason) if reason.contains("no speech")),
        "{:?}",
        silent.refine
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("current-result.txt")).unwrap(),
        TRANSCRIPT
    );

    app.dispatch(Control::Refine);
    assert!(app.snapshot().detail.contains("Refine is unavailable"));
    assert!(refine.seen().is_empty());
}

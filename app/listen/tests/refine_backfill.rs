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
        atomic::{AtomicUsize, Ordering},
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

struct Editors {
    focused: Option<usize>,
    drafts: [String; 2],
    available: [bool; 2],
    /// Report the draft with CRLF line endings, as some Windows editors do, while
    /// storing what `replace` was asked for. The refine comparison must not depend
    /// on line endings.
    crlf: bool,
    replace_calls: usize,
}

impl Editors {
    fn new(first: &str) -> Self {
        Self {
            focused: Some(0),
            drafts: [first.into(), String::new()],
            available: [true; 2],
            crlf: false,
            replace_calls: 0,
        }
    }
}

struct Desktop(Arc<Mutex<Editors>>);
struct BoundEditor {
    editors: Arc<Mutex<Editors>>,
    selected: usize,
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

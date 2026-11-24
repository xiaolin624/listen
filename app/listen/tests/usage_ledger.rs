//! Cumulative cloud usage and cost through the application seam.
//!
//! Every assertion is a user-visible fact observed through `Application::dispatch`,
//! `poll` and `snapshot`: how much this tool spent, how much of it is unverified,
//! which month it belongs to, and whether the monthly reminder changed anything.
//! No paid network is contacted. The cloud adapter tests write the real durable
//! ledger file with an independent JSON fixture so the summary is derived from the
//! same facts a restart would read, not from private helpers.
use listen::{
    AppResult, Application, CapturedAudio, Control, Phase, PlaybackAudio, Recording, Target,
    TargetSelection, UsageSummary,
    cloud::{
        AliSettings, CloudError, CloudSessions, CredentialSource, MessageSocket, MessageTransport,
        PcmSpoolWriter, PcmStream, Secret, TrialLedger, VerifiedBounds,
    },
    testing::{ScriptedControl, ScriptedSessions},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(0);

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "listen-usage-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn app(&self) -> PathBuf {
        self.0.join("app")
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait(app: &mut Application, phase: Phase) -> listen::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        let snapshot = app.snapshot();
        if snapshot.phase == phase {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{snapshot:?}");
        thread::sleep(Duration::from_millis(2));
    }
}

/// A summary with both known and unknown cost, used where the exact figures do not
/// matter beyond "this is what the adapter reported".
fn summary(month_confirmed: u64, month_estimated: u64) -> UsageSummary {
    UsageSummary {
        requests: 2,
        settled_requests: 1,
        confirmed_nanoyuan: month_confirmed,
        estimated_nanoyuan: month_estimated,
        unknown_requests: 1,
        retried_segments: 0,
        undated_requests: 0,
        month_confirmed_nanoyuan: month_confirmed,
        month_estimated_nanoyuan: month_estimated,
        month_unknown_requests: 1,
        trial_cap_nanoyuan: 50_000_000_000,
        confirmed_input_tokens: 1_000,
        confirmed_output_tokens: 20,
        unreadable: false,
    }
}

/// The application driven by the scripted adapter, which reports the summary and
/// lets the test finish the running session. The application's own reminder and
/// refresh rules are observed without a vendor.
fn application(runtime: &Runtime, usage: UsageSummary) -> (Application, ScriptedControl) {
    let adapter = ScriptedSessions::new()
        .with_backend("Cloud trial: controlled provider; model=controlled; region=controlled")
        .with_usage(usage);
    let control = adapter.control();
    let app = Application::with_recognition_sessions(Box::new(adapter), runtime.app(), Ok(()));
    (app, control)
}

#[test]
fn usage_from_the_recognition_adapter_is_visible_before_and_during_a_capture() {
    let runtime = Runtime::new();
    let reported = summary(420_000_000, 28_500_000_000);
    let (mut app, _) = application(&runtime, reported.clone());

    // The cost is a fact about the ledger, not about the current capture: it is
    // readable before the user starts anything.
    assert_eq!(app.snapshot().usage, Some(reported.clone()));

    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(capturing.phase, Phase::Capturing);
    assert_eq!(capturing.usage, Some(reported));
}

// ---------------------------------------------------------------------------
// The real cloud adapter, driven through the same application seam. Only the
// playback device, credential store and remote socket are substitutes.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Playback(Mutex<Option<PcmSpoolWriter>>);
struct Capture {
    playback: Arc<Playback>,
    stream: Option<PcmStream>,
}
struct Device(Arc<Playback>);
impl PlaybackAudio for Device {
    fn start(&self, directory: &std::path::Path) -> AppResult<Box<dyn Recording>> {
        let (writer, stream) = PcmSpoolWriter::new(directory, 2_000_000)?;
        *self.0.0.lock().unwrap() = Some(writer);
        Ok(Box::new(Capture {
            playback: self.0.clone(),
            stream: Some(stream),
        }))
    }
}
impl Recording for Capture {
    fn take_pcm_stream(&mut self) -> Option<PcmStream> {
        self.stream.take()
    }
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        self.playback
            .0
            .lock()
            .unwrap()
            .take()
            .expect("the controlled device still owns its spool")
            .finish("controlled playback".into())
    }
}
impl Playback {
    fn speak(&self) {
        let mut capture = self.0.lock().unwrap();
        if let Some(writer) = capture.as_mut() {
            for _ in 0..1600 {
                writer.push(7).unwrap();
            }
            writer.flush().unwrap();
        }
    }
}

struct Key;
impl CredentialSource for Key {
    fn load(&self) -> Result<Secret, CloudError> {
        Secret::new("offline-test-key".into())
    }
}
enum Outbound {
    Text(Value),
    /// PCM was sent; the harness only needs to know it happened, not to read it.
    Audio,
}
struct Remote {
    inbound: Mutex<Option<mpsc::Receiver<Result<String, CloudError>>>>,
    outbound: mpsc::Sender<Outbound>,
}
struct Socket {
    inbound: mpsc::Receiver<Result<String, CloudError>>,
    outbound: mpsc::Sender<Outbound>,
}
impl MessageTransport for Remote {
    fn verified_bounds(&self, _settings: &AliSettings) -> Option<VerifiedBounds> {
        Some(VerifiedBounds {
            max_input_tokens: 4_000_000,
            max_output_tokens: 1_000_000,
            max_pcm_bytes: 1_920_000,
            evidence: "controlled offline fixture: bounded input and output; no paid network"
                .into(),
            verified_on: "2026-11-14".into(),
        })
    }
    fn connect(
        &self,
        _settings: &AliSettings,
        _key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        Ok(Box::new(Socket {
            inbound: self.inbound.lock().unwrap().take().unwrap(),
            outbound: self.outbound.clone(),
        }))
    }
}
impl MessageSocket for Socket {
    fn send_text(&mut self, text: &str) -> Result<(), CloudError> {
        self.outbound
            .send(Outbound::Text(serde_json::from_str(text).unwrap()))
            .unwrap();
        Ok(())
    }
    fn send_audio(&mut self, _pcm: &[u8]) -> Result<(), CloudError> {
        self.outbound.send(Outbound::Audio).unwrap();
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<String>, CloudError> {
        match self.inbound.try_recv() {
            Ok(value) => value.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(CloudError::Network),
        }
    }
}

#[derive(Default)]
struct Editor(Mutex<String>);
struct Selection(Arc<Editor>);
struct Bound(Arc<Editor>);
impl TargetSelection for Selection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(Bound(self.0.clone()))))
    }
}
impl Target for Bound {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.0.lock().unwrap().clone())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut text = self.0.0.lock().unwrap();
        if *text != expected {
            return Err("draft changed".into());
        }
        *text = replacement.into();
        Ok(())
    }
}

fn event(task: &str, kind: &str, payload: Value) -> String {
    json!({ "header": { "task_id": task, "event": kind }, "payload": payload }).to_string()
}
fn receive_text(out: &mpsc::Receiver<Outbound>, action: &str) -> Value {
    loop {
        if let Outbound::Text(value) = out.recv_timeout(Duration::from_secs(5)).unwrap() {
            assert_eq!(value["header"]["action"], action);
            return value;
        }
    }
}

// ---------------------------------------------------------------------------
// Independent ledger fixtures. They are the same JSON a previous version wrote,
// so they also prove that an existing ledger still loads (the new month stamp is
// optional) and that the summary is derived from durable facts, not from a
// private in-memory shortcut.
// ---------------------------------------------------------------------------

fn token_attempt(
    task: &str,
    segment: &str,
    max_input_tokens: u64,
    max_output_tokens: u64,
    reported_input_tokens: u64,
    reported_output_tokens: u64,
    final_usage: bool,
    requested_at_utc: Option<u64>,
) -> Value {
    // The ledger only accepts a reservation equal to the verified Token bound's
    // price: 6 元 / million input tokens and 4.5 元 / million output tokens.
    let reserved_nanoyuan = max_input_tokens * 6_000 + max_output_tokens * 4_500;
    let estimated_nanoyuan = if final_usage {
        json!(reported_input_tokens * 6_000 + reported_output_tokens * 4_500)
    } else {
        Value::Null
    };
    let mut attempt = json!({
        "task_id": task,
        "segment_id": segment,
        "provider": "alibaba-model-studio",
        "model": "qwen-audio-3.1-asr-flash-message",
        "region": "cn-beijing",
        "reserved_nanoyuan": reserved_nanoyuan,
        "estimated_nanoyuan": estimated_nanoyuan,
        "billed_nanoyuan": Value::Null,
        "usage": {
            "input_tokens": if final_usage { json!(reported_input_tokens) } else { Value::Null },
            "output_tokens": if final_usage { json!(reported_output_tokens) } else { Value::Null },
            "total_tokens": if final_usage { json!(reported_input_tokens + reported_output_tokens) } else { Value::Null },
            "duration": Value::Null
        },
        "reported_final_usage": if final_usage {
            json!({"input_tokens": reported_input_tokens, "output_tokens": reported_output_tokens,
                   "total_tokens": reported_input_tokens + reported_output_tokens, "duration": Value::Null})
        } else { Value::Null },
        "final_usage": final_usage,
        "unknown_reason": if final_usage {
            Value::Null
        } else {
            json!("Final cumulative provider usage has not been confirmed; the full reservation remains committed")
        },
        "bounds": {
            "max_input_tokens": max_input_tokens,
            "max_output_tokens": max_output_tokens,
            "max_pcm_bytes": 32_000,
            "evidence": "independent ledger fixture",
            "verified_on": "2026-11-14"
        },
        "price_source": "independent fixture price source",
        "price_verified_on": "2026-11-14"
    });
    if let Some(at) = requested_at_utc {
        attempt["requested_at_utc"] = json!(at);
    }
    attempt
}

/// A request admitted under the verified free-only protection: no cash is
/// reserved, but neither the usage nor the actual bill is verified.
fn free_only_attempt(task: &str, segment: &str, requested_at_utc: Option<u64>) -> Value {
    let mut attempt = json!({
        "task_id": task,
        "segment_id": segment,
        "provider": "alibaba-model-studio",
        "model": "qwen-audio-3.1-asr-flash-message",
        "region": "cn-beijing",
        "reserved_nanoyuan": 0u64,
        "estimated_nanoyuan": 0u64,
        "billed_nanoyuan": Value::Null,
        "usage": {"input_tokens": Value::Null, "output_tokens": Value::Null,
                  "total_tokens": Value::Null, "duration": Value::Null},
        "reported_final_usage": Value::Null,
        "final_usage": true,
        "unknown_reason": "Actual billed charge has not been verified; active free-only protection limits additional cash charges to zero",
        "free_only": {
            "model": "qwen-audio-3.1-asr-flash-message",
            "region": "cn-beijing",
            "evidence": "independent ledger fixture",
            "verified_on": "2026-11-15"
        },
        "price_source": "independent fixture free-only source",
        "price_verified_on": "2026-11-15"
    });
    if let Some(at) = requested_at_utc {
        attempt["requested_at_utc"] = json!(at);
    }
    attempt
}

fn write_ledger(directory: &std::path::Path, attempts: Vec<Value>) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("ledger.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "cap_nanoyuan": 50_000_000_000u64,
            "frozen": false,
            "attempts": attempts
        }))
        .unwrap(),
    )
    .unwrap();
}

fn utc_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Scenario {
    runtime: Runtime,
    app: Application,
    playback: Arc<Playback>,
    editor: Arc<Editor>,
    input: mpsc::Sender<Result<String, CloudError>>,
    outgoing: mpsc::Receiver<Outbound>,
    ledger: TrialLedger,
}
impl Scenario {
    fn new() -> Self {
        Self::build(Runtime::new())
    }
    /// A scenario whose durable ledger already holds these attempts before the
    /// application is constructed, exactly like a restart after earlier runs.
    fn with_ledger(attempts: Vec<Value>) -> Self {
        let runtime = Runtime::new();
        write_ledger(&runtime.0.join("shared-ledger"), attempts);
        Self::build(runtime)
    }
    fn build(runtime: Runtime) -> Self {
        let playback = Arc::new(Playback::default());
        let editor = Arc::new(Editor(Mutex::new("protected draft".into())));
        let (input, incoming) = mpsc::channel();
        let (output, outgoing) = mpsc::channel();
        let ledger = TrialLedger::new(runtime.0.join("shared-ledger"));
        let sessions = CloudSessions::new(
            Arc::new(Device(playback.clone())),
            Arc::new(Remote {
                inbound: Mutex::new(Some(incoming)),
                outbound: output,
            }),
            Arc::new(Key),
            AliSettings::beijing("offline-workspace".into()),
            ledger.clone(),
        );
        let app = Application::with_recognition_sessions(
            Box::new(sessions),
            runtime.0.join("app"),
            Ok(()),
        )
        .with_targets(Arc::new(Selection(editor.clone())));
        Self {
            runtime,
            app,
            playback,
            editor,
            input,
            outgoing,
            ledger,
        }
    }
    fn start(&mut self) -> String {
        self.app.dispatch(Control::Start);
        let state = self.app.snapshot();
        assert_eq!(
            state.phase,
            Phase::Capturing,
            "capture did not start: {state:?}"
        );
        self.playback.speak();
        let run = receive_text(&self.outgoing, "run-task");
        let task = run["header"]["task_id"].as_str().unwrap().to_string();
        self.send(&task, "task-started", json!({}));
        match self.outgoing.recv_timeout(Duration::from_secs(5)).unwrap() {
            Outbound::Audio => {}
            _ => panic!("expected streaming PCM"),
        }
        task
    }
    fn send(&self, task: &str, kind: &str, payload: Value) {
        self.input.send(Ok(event(task, kind, payload))).unwrap();
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
        receive_text(&self.outgoing, "finish-task");
    }
    /// A second application on the same runtime and ledger: this is the restart
    /// path, and the only thing it inherits is what is on disk.
    fn reopened(&self) -> Application {
        let (_input, incoming) = mpsc::channel();
        let (output, _outgoing) = mpsc::channel();
        let ledger = TrialLedger::new(self.runtime.0.join("shared-ledger"));
        let sessions = CloudSessions::new(
            Arc::new(Device(Arc::new(Playback::default()))),
            Arc::new(Remote {
                inbound: Mutex::new(Some(incoming)),
                outbound: output,
            }),
            Arc::new(Key),
            AliSettings::beijing("offline-workspace".into()),
            ledger,
        );
        Application::with_recognition_sessions(
            Box::new(sessions),
            self.runtime.0.join("app"),
            Ok(()),
        )
    }
}

#[test]
fn a_finished_cloud_capture_reports_confirmed_usage_and_keeps_it_after_restart() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.send(
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"Complete result","sentence_end":true
        }}}),
    );
    scenario.stop();
    // 70,000 input tokens and 1,000 output tokens at the ledger's verified price:
    // 70,000 × 6 + 1,000 × 4.5 元 / million = 0.4245 元.
    scenario.send(
        &task,
        "task-finished",
        json!({"usage":{"input_tokens":70_000,"output_tokens":1_000,"total_tokens":71_000}}),
    );
    let result = wait(&mut scenario.app, Phase::Result);
    let delivered = scenario.editor.0.lock().unwrap().clone();
    assert!(delivered.contains("protected draft"), "{delivered}");
    assert!(delivered.contains("Complete result"), "{delivered}");
    assert_eq!(result.text, "Complete result");

    let usage = result.usage.expect("the cloud adapter reports its ledger");
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.settled_requests, 1);
    assert_eq!(usage.confirmed_nanoyuan, 424_500_000);
    assert_eq!(usage.estimated_nanoyuan, 0);
    assert_eq!(usage.unknown_requests, 0);
    assert_eq!(usage.confirmed_input_tokens, 70_000);
    assert_eq!(usage.confirmed_output_tokens, 1_000);
    // The request was dated, so it belongs to this month and to the lifetime.
    assert_eq!(usage.undated_requests, 0);
    assert_eq!(usage.month_confirmed_nanoyuan, 424_500_000);
    assert_eq!(usage.month_nanoyuan(), usage.lifetime_nanoyuan());
    // The hard trial cap and the reminder line are separate facts.
    assert_eq!(usage.trial_cap_nanoyuan, 50_000_000_000);
    assert!(!usage.reminder_reached());

    // Restarting the application changes nothing: the same ledger is the source.
    let reopened = scenario.reopened();
    assert_eq!(reopened.snapshot().usage, Some(usage));
}

#[test]
fn retries_missing_usage_and_free_only_history_stay_visible_after_a_restart() {
    // A version-1 ledger with no month stamps: the schema change is additive, so
    // the file still loads, and undated history is reported as undated instead of
    // being attributed to a month it cannot prove.
    let scenario = Scenario::with_ledger(vec![
        // A settled request for the first recording.
        token_attempt("task-a1", "session-a", 100_000, 0, 100_000, 0, true, None),
        // The automatic rebuild/retry of the same recording, never settled: the
        // whole reservation is the conservative bound.
        token_attempt(
            "task-a2",
            "session-a",
            4_000_000,
            1_000_000,
            0,
            0,
            false,
            None,
        ),
        // A different recording admitted under free-only protection: zero cash was
        // reserved, but neither its usage nor its actual bill was verified.
        free_only_attempt("task-b1", "session-b", None),
    ]);

    let usage = scenario
        .app
        .snapshot()
        .usage
        .expect("the cloud adapter reports its ledger");
    assert_eq!(usage.requests, 3);
    assert_eq!(usage.settled_requests, 2);
    // 100,000 input tokens at 6 元 / million = 0.60 元, confirmed from final usage.
    assert_eq!(usage.confirmed_nanoyuan, 600_000_000);
    // The retry has no final usage, so its whole reservation stays estimated.
    assert_eq!(usage.estimated_nanoyuan, 28_500_000_000);
    assert_eq!(usage.lifetime_nanoyuan(), 29_100_000_000);
    // Missing final usage and unverified free-only billing are both unknown; the
    // free-only request's zero cash is never presented as "nothing spent".
    assert_eq!(usage.unknown_requests, 2);
    assert_eq!(usage.retried_segments, 1);
    // Legacy attempts cannot be assigned to a month, and are excluded from it.
    assert_eq!(usage.undated_requests, 3);
    assert_eq!(usage.month_nanoyuan(), 0);
    assert_eq!(usage.month_unknown_requests, 0);
    assert!(!usage.reminder_reached());
    assert_eq!(usage.trial_cap_nanoyuan, 50_000_000_000);
    // The committed total is reported by the ledger itself, so the summary cannot
    // quietly drop a reservation.
    assert_eq!(
        scenario.ledger.snapshot().unwrap().committed_nanoyuan,
        29_100_000_000
    );

    // A restart reads the same durable record; nothing is re-zeroed.
    assert_eq!(scenario.reopened().snapshot().usage, Some(usage));
}

#[test]
fn the_monthly_total_rolls_over_but_the_lifetime_total_does_not() {
    let now = utc_now_seconds();
    let last_month = now.saturating_sub(40 * 86_400);
    let scenario = Scenario::with_ledger(vec![
        // Undated history from before this version.
        token_attempt(
            "task-old",
            "session-old",
            1_000_000,
            0,
            100_000,
            0,
            true,
            None,
        ),
        // A settled request from the previous month.
        token_attempt(
            "task-last",
            "session-last",
            2_000_000,
            0,
            2_000_000,
            0,
            true,
            Some(last_month),
        ),
        // A settled request from this month.
        token_attempt(
            "task-now",
            "session-now",
            500_000,
            0,
            500_000,
            0,
            true,
            Some(now),
        ),
        // An unverified free-only request from this month.
        free_only_attempt("task-free", "session-free", Some(now)),
    ]);

    let usage = scenario.app.snapshot().usage.unwrap();
    assert_eq!(usage.requests, 4);
    assert_eq!(usage.settled_requests, 4);
    // 0.60 (undated) + 12.00 (last month) + 3.00 (this month) 元.
    assert_eq!(usage.lifetime_nanoyuan(), 15_600_000_000);
    // Only this month's 3.00 元 counts toward the monthly figure and reminder.
    assert_eq!(usage.month_confirmed_nanoyuan, 3_000_000_000);
    assert_eq!(usage.month_estimated_nanoyuan, 0);
    assert_eq!(usage.month_nanoyuan(), 3_000_000_000);
    assert!(!usage.reminder_reached());
    // The date-less record and the free-only request are stated, not hidden.
    assert_eq!(usage.undated_requests, 1);
    assert_eq!(usage.month_unknown_requests, 1);
    assert_eq!(usage.unknown_requests, 1);
    assert_eq!(usage.retried_segments, 0);

    // The month accumulation is durable, not a per-process counter.
    let reopened = scenario.reopened();
    assert_eq!(reopened.snapshot().usage, Some(usage));
}

#[test]
fn approaching_the_monthly_line_notifies_without_touching_the_trial_cap() {
    let runtime = Runtime::new();
    // 90% of the CNY 100 reminder line: the reminder must already fire here so the
    // user is warned before crossing the line.
    let near_the_line = summary(90_000_000_000, 0);
    assert!(near_the_line.reminder_reached());
    assert!(near_the_line.month_nanoyuan() < listen::UsageSummary::MONTH_REMINDER_NANOYUAN);
    let (mut app, control) = application(&runtime, near_the_line);

    let ready = app.snapshot();
    assert_eq!(ready.notice, Some(listen::Notice::UsageReminder));
    assert_eq!(
        ready.usage.as_ref().map(|usage| usage.trial_cap_nanoyuan),
        Some(50_000_000_000),
        "the trial cap is unchanged by the reminder"
    );

    // The reminder never blocks capture or delivery.
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    app.dispatch(Control::Stop);
    control.last().finish("接近提醒线也照常完成");
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.text, "接近提醒线也照常完成");
    assert_eq!(result.notice, Some(listen::Notice::UsageReminder));
    let usage = result.usage.expect("the summary stays visible");
    assert_eq!(usage.month_nanoyuan(), 90_000_000_000);
    assert!(usage.reminder_reached());
    assert_eq!(usage.trial_cap_nanoyuan, 50_000_000_000);
}

#[test]
fn the_monthly_reminder_does_not_interrupt_a_capture_or_its_delivery() {
    let runtime = Runtime::new();
    // This month's own usage already reached the 100 CNY reminder line.
    let over_the_line = summary(120_000_000_000, 0);
    assert!(over_the_line.reminder_reached());
    let (mut app, control) = application(&runtime, over_the_line);

    // The reminder is visible as soon as the application can read the ledger, and
    // it explicitly says the feature keeps working.
    let ready = app.snapshot();
    assert_eq!(ready.phase, Phase::Ready);
    assert_eq!(ready.notice, Some(listen::Notice::UsageReminder));
    let notice = ready.notice.as_ref().unwrap().text();
    assert!(notice.contains("提醒"), "{notice}");
    assert!(notice.contains("照常继续"), "{notice}");
    assert!(notice.contains("不会自动停用"), "{notice}");

    // Reaching the reminder line does not refuse a capture.
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);

    // Nor does it cancel the capture: stopping still produces the complete
    // result, and the result is delivered normally.
    app.dispatch(Control::Stop);
    control.last().finish("提醒之后仍然完成");
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.text, "提醒之后仍然完成");
    assert_eq!(result.delivery, Some(listen::Delivery::Fallback));
    assert_eq!(result.notice, Some(listen::Notice::UsageReminder));
    let usage = result.usage.expect("the summary stays visible");
    assert_eq!(usage.month_nanoyuan(), 120_000_000_000);
    assert!(usage.reminder_reached());
}

#[test]
fn the_monthly_reminder_never_grants_or_extends_the_trial_budget() {
    // A durable ledger already past both limits: this month is over CNY 100 and the
    // committed trial total is over CNY 50. This is the one fixture shape where the
    // reminder can be reached under the current hard cap, so it proves the two
    // limits are independent.
    let over = token_attempt(
        "task-over",
        "session-over",
        19_000_000,
        0,
        0,
        0,
        false,
        Some(utc_now_seconds()),
    );
    let mut scenario = Scenario::with_ledger(vec![over]);

    let ready = scenario.app.snapshot();
    let usage = ready.usage.expect("the ledger is readable");
    // 19,000,000 input tokens reserved at 6 元 / million = 114.00 元.
    assert_eq!(usage.month_nanoyuan(), 114_000_000_000);
    assert_eq!(usage.trial_cap_nanoyuan, 50_000_000_000);
    assert!(usage.reminder_reached());
    assert_eq!(ready.notice, Some(listen::Notice::UsageReminder));

    // Starting is refused by the CNY 50 trial cap, not by the reminder line: the
    // reminder must never be read as permission to spend more.
    scenario.app.dispatch(Control::Start);
    let refused = scenario.app.snapshot();
    assert_eq!(refused.phase, Phase::Failure);
    assert!(
        refused.detail.contains("shared CNY 50 trial budget"),
        "{refused:?}"
    );
    assert!(!refused.detail.contains("每月 100 元提醒线"), "{refused:?}");

    // Nothing was granted: the same cap and the same committed total remain, and no
    // new request was admitted.
    let ledger = scenario.ledger.snapshot().unwrap();
    assert_eq!(ledger.cap_nanoyuan, 50_000_000_000);
    assert_eq!(ledger.committed_nanoyuan, 114_000_000_000);
    assert_eq!(ledger.attempts.len(), 1);
}

#[test]
fn the_usage_summary_and_the_ledger_never_expose_the_cloud_key() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.send(
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"脱敏结果","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.send(
        &task,
        "task-finished",
        json!({"usage":{"input_tokens":1_000,"output_tokens":20,"total_tokens":1_020}}),
    );
    let result = wait(&mut scenario.app, Phase::Result);
    assert_eq!(result.text, "脱敏结果");
    assert!(
        result
            .usage
            .as_ref()
            .is_some_and(|usage| usage.confirmed_nanoyuan > 0)
    );

    // The snapshot is the only source of `status.txt` and `panel.txt`, so neither
    // surface can carry the credential if the snapshot does not.
    let shown = format!("{:?}", result);
    for forbidden in ["offline-test-key", "Bearer", "Authorization"] {
        assert!(!shown.contains(forbidden), "{forbidden} in {shown}");
    }
    // The durable cost record names the provider, model, region and request, but
    // never the key that authorized it.
    let ledger = fs::read_to_string(scenario.runtime.0.join("shared-ledger/ledger.json")).unwrap();
    assert!(!ledger.contains("offline-test-key"), "{ledger}");
    assert!(
        ledger.contains("qwen-audio-3.1-asr-flash-message"),
        "{ledger}"
    );
    assert!(ledger.contains("cn-beijing"), "{ledger}");
}

#[test]
fn an_unreadable_ledger_is_reported_as_unknown_and_never_as_zero() {
    let runtime = Runtime::new();
    fs::create_dir_all(runtime.0.join("shared-ledger")).unwrap();
    fs::write(
        runtime.0.join("shared-ledger/ledger.json"),
        br#"{"version": 7, "cap_nanoyuan": 0}"#,
    )
    .unwrap();
    let mut scenario = Scenario::build(runtime);

    let usage = scenario
        .app
        .snapshot()
        .usage
        .expect("a cloud adapter always reports a summary");
    assert!(usage.unreadable);
    assert_eq!(usage.requests, 0);
    assert_eq!(usage.lifetime_nanoyuan(), 0);
    assert!(!usage.reminder_reached());
    // "Unknown" is distinguishable from a ledger that genuinely has no spend.
    assert_ne!(usage, UsageSummary::default());

    // The unreadable ledger refuses a paid request instead of admitting one on a
    // zero-looking total; the refusal names the ledger, not the monthly reminder.
    scenario.app.dispatch(Control::Start);
    let refused = scenario.app.snapshot();
    assert_eq!(refused.phase, Phase::Failure);
    assert!(refused.detail.contains("trial cost ledger"), "{refused:?}");
    let refused_usage = refused.usage.expect("the summary stays visible");
    assert!(refused_usage.unreadable);
}

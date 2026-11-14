//! Offline workflows through the application and the real cloud-session adapter.
//! The only substitutes are the playback device, credential store and remote socket.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, PlaybackAudio, Recording,
    Target, TargetSelection,
    cloud::{
        AliSettings, CloudError, CloudSessions, CredentialSource, MessageSocket, MessageTransport,
        PcmSpoolWriter, PcmStream, Secret, TrialLedger, VerifiedBounds,
    },
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "listen-cloud-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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

#[derive(Default)]
struct Playback(Mutex<Option<PcmSpoolWriter>>);
struct Capture {
    playback: Arc<Playback>,
    stream: Option<PcmStream>,
}
struct Device(Arc<Playback>);
impl PlaybackAudio for Device {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
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
            .unwrap()
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
    Audio(Vec<u8>),
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
    fn send_audio(&mut self, pcm: &[u8]) -> Result<(), CloudError> {
        self.outbound.send(Outbound::Audio(pcm.to_vec())).unwrap();
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

#[test]
fn cloud_capture_streams_before_stop_and_waits_for_whole_task_confirmation_before_safe_delivery() {
    let runtime = Runtime::new();
    let playback = Arc::new(Playback::default());
    let editor = Arc::new(Editor(Mutex::new("draft".into())));
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
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.join("app"), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));
    app.dispatch(Control::Start);
    assert_eq!(app.snapshot().phase, Phase::Capturing);
    playback.speak(); // The beginning is on disk before task-started arrives.
    let run = receive_text(&outgoing, "run-task");
    let task = run["header"]["task_id"].as_str().unwrap();
    input
        .send(Ok(event(task, "task-started", json!({}))))
        .unwrap();
    let mut uploaded = Vec::new();
    while uploaded.len() < 3200 {
        match outgoing.recv_timeout(Duration::from_secs(5)).unwrap() {
            Outbound::Audio(bytes) => uploaded.extend(bytes),
            _ => panic!("audio must be streamed while capture is still active"),
        }
    }
    assert_eq!(uploaded, vec![7, 0].repeat(1600));
    input
        .send(Ok(event(
            task,
            "result-generated",
            json!({"output":{"sentence":{
                "sentence_id":1,"text":"嗯，不，不确定是 3.1。","sentence_end":true
            }}}),
        )))
        .unwrap();
    app.poll();
    assert_eq!(*editor.0.lock().unwrap(), "draft");
    *editor.0.lock().unwrap() = "edited during capture".into();
    app.dispatch(Control::Stop);
    receive_text(&outgoing, "finish-task");
    app.poll();
    assert_eq!(app.snapshot().phase, Phase::Processing);
    assert_eq!(*editor.0.lock().unwrap(), "edited during capture");
    input
        .send(Ok(event(
            task,
            "task-finished",
            json!({"usage":{
                "input_tokens":84,"output_tokens":3,"total_tokens":87,"duration":0.1
            }}),
        )))
        .unwrap();
    let result = wait(&mut app, Phase::Result);
    assert_eq!(result.text, "嗯，不，不确定是 3.1。");
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        *editor.0.lock().unwrap(),
        "edited during capture\n嗯，不，不确定是 3.1。"
    );
    assert_eq!(
        fs::read_to_string(runtime.0.join("app/current-result.txt")).unwrap(),
        result.text
    );
    assert_eq!(ledger.snapshot().unwrap().committed_nanoyuan, 517_500);
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
        let runtime = Runtime::new();
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
        self.playback.speak();
        let run = receive_text(&self.outgoing, "run-task");
        let task = run["header"]["task_id"].as_str().unwrap().to_string();
        self.send(&task, "task-started", json!({}));
        match self.outgoing.recv_timeout(Duration::from_secs(5)).unwrap() {
            Outbound::Audio(_) => {}
            _ => panic!("expected streaming PCM"),
        }
        task
    }
    fn send(&self, task: &str, kind: &str, payload: Value) {
        self.input.send(Ok(event(task, kind, payload))).unwrap();
    }
    fn sentence(&self, task: &str, id: u64, text: &str, usage: Value) {
        self.send(
            task,
            "result-generated",
            json!({"output":{"sentence":{
            "sentence_id":id,"text":text,"sentence_end":true
        }},"usage":usage}),
        );
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
        receive_text(&self.outgoing, "finish-task");
    }
}

#[test]
fn stable_sentences_and_wrong_task_acknowledgments_cannot_deliver_after_connection_loss() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.sentence(
        &task,
        1,
        "stable but incomplete",
        json!({"input_tokens":84,"output_tokens":4,"total_tokens":88}),
    );
    // A whole-task acknowledgment before finish-task is not final completion.
    scenario.send(&task, "task-finished", json!({}));
    thread::sleep(Duration::from_millis(10));
    scenario.app.poll();
    assert_eq!(scenario.app.snapshot().phase, Phase::Capturing);
    scenario.stop();
    scenario.send("another-task", "task-finished", json!({}));
    scenario.input.send(Err(CloudError::Network)).unwrap();
    let failure = wait(&mut scenario.app, Phase::Failure);
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    assert!(failure.text.is_empty());
    assert!(
        !failure.retained_audio.is_empty(),
        "continuous PCM must remain visible for recovery"
    );
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    assert_eq!(
        scenario.ledger.snapshot().unwrap().committed_nanoyuan,
        28_500_000_000
    );
    assert!(
        !failure.retry_available,
        "a cloud failure must not offer a silent local retry"
    );
}

#[test]
fn each_cloud_capture_sends_the_start_glossary_and_explicit_fidelity_settings_without_window_data()
{
    let mut scenario = Scenario::new();
    let glossary = scenario.runtime.0.join("glossary.txt");
    fs::write(&glossary, "WASAPI\nDeepSeek Harness | 迪普西克\n").unwrap();
    scenario.app = scenario.app.with_glossary(&glossary);
    scenario.app.dispatch(Control::Start);
    fs::write(&glossary, "Changed after start\n").unwrap();
    let run = receive_text(&scenario.outgoing, "run-task");
    assert_eq!(
        run["payload"]["parameters"],
        json!({
            "format":"pcm", "sample_rate":16000,
            "disfluency_removal_enabled":false, "intermediate_result_enabled":false,
            "keep_dialect":true, "heartbeat":true,
            "vocabulary":{"WASAPI":4,"DeepSeek Harness":4}
        })
    );
    assert_eq!(run["payload"]["input"], json!({}));
    assert!(!run.to_string().contains("protected draft"));
    let task = run["header"]["task_id"].as_str().unwrap();
    scenario.send(task, "task-started", json!({}));
    scenario.stop();
    scenario.send(task, "task-finished", json!({}));
    wait(&mut scenario.app, Phase::NoSpeech);
}

#[test]
fn cumulative_duplicate_and_stale_usage_is_not_added_and_missing_final_usage_remains_fully_reserved()
 {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    for usage in [
        json!({"input_tokens":100,"output_tokens":2,"total_tokens":102}),
        json!({"input_tokens":100,"output_tokens":2,"total_tokens":102}),
        json!({"input_tokens":80,"output_tokens":1,"total_tokens":81}),
    ] {
        scenario.sentence(&task, 1, "嗯", usage);
    }
    scenario.stop();
    scenario.send(&task, "task-finished", json!({"usage":{"output_tokens":2}}));
    let result = wait(&mut scenario.app, Phase::Result);
    assert_eq!(
        result.text, "嗯",
        "short genuine answers are not a hallucination blacklist"
    );
    let ledger = scenario.ledger.snapshot().unwrap();
    assert_eq!(ledger.attempts[0].usage.input_tokens, Some(100));
    assert_eq!(ledger.attempts[0].usage.output_tokens, Some(2));
    assert_eq!(ledger.attempts[0].usage.total_tokens, Some(102));
    assert!(!ledger.attempts[0].final_usage);
    assert!(ledger.attempts[0].unknown_reason.is_some());
    assert_eq!(ledger.committed_nanoyuan, 28_500_000_000);
    assert_eq!(ledger.attempts[0].billed_nanoyuan, None);
}

#[test]
fn usage_beyond_the_verified_bound_preserves_audio_and_blocks_further_paid_trials() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.stop();
    scenario.send(
        &task,
        "task-finished",
        json!({"usage":{
            "input_tokens":4_000_001,"output_tokens":1,"total_tokens":4_000_002
        }}),
    );
    let failure = wait(&mut scenario.app, Phase::Failure);
    assert!(failure.detail.contains("billing review"), "{failure:?}");
    assert!(!failure.retained_audio.is_empty());
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    let ledger = scenario.ledger.snapshot().unwrap();
    assert!(ledger.frozen);
    assert_eq!(ledger.committed_nanoyuan, 28_500_000_000);
}

#[test]
fn whole_task_ack_does_not_turn_a_nonfinal_sentence_into_a_complete_or_no_speech_result() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.send(
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"still changing","sentence_end":false
        }}}),
    );
    scenario.stop();
    scenario.send(
        &task,
        "task-finished",
        json!({"usage":{"input_tokens":84,"output_tokens":4,"total_tokens":88}}),
    );
    let failure = wait(&mut scenario.app, Phase::Failure);
    assert!(failure.detail.contains("protocol"));
    assert!(!failure.retained_audio.is_empty());
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
}

#[test]
fn cloud_waiting_state_names_the_provider_and_does_not_claim_processing_is_local() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.stop();
    let state = scenario.app.snapshot();
    assert!(
        state.detail.contains("whole-task cloud confirmation"),
        "{state:?}"
    );
    assert!(!state.detail.contains("Processing locally"));
    assert!(
        state
            .backend
            .as_deref()
            .unwrap()
            .contains("qwen-audio-3.1-asr-flash-message")
    );
    scenario.send(&task, "task-finished", json!({}));
    wait(&mut scenario.app, Phase::NoSpeech);
}

struct MissingKey;
impl CredentialSource for MissingKey {
    fn load(&self) -> Result<Secret, CloudError> {
        Err(CloudError::MissingKey)
    }
}
struct NoPolicy;
impl MessageTransport for NoPolicy {
    fn connect(&self, _: &AliSettings, _: &Secret) -> Result<Box<dyn MessageSocket>, CloudError> {
        panic!("an unknown reservation policy must never reach the network boundary")
    }
}
#[test]
fn missing_credentials_and_unknown_cost_bounds_refuse_before_capture_or_network() {
    for (credentials, expected, unknown_cost) in [
        (
            Arc::new(MissingKey) as Arc<dyn CredentialSource>,
            "No Alibaba cloud API key",
            false,
        ),
        (
            Arc::new(Key) as Arc<dyn CredentialSource>,
            "maximum charge",
            true,
        ),
    ] {
        let runtime = Runtime::new();
        let playback = Arc::new(Playback::default());
        let ledger = TrialLedger::new(runtime.0.join("ledger"));
        let sessions = CloudSessions::new(
            Arc::new(Device(playback.clone())),
            Arc::new(NoPolicy),
            credentials,
            AliSettings::beijing("offline-workspace".into()),
            ledger.clone(),
        );
        let mut app = Application::with_recognition_sessions(
            Box::new(sessions),
            runtime.0.join("app"),
            Ok(()),
        );
        app.dispatch(Control::Start);
        let failure = app.snapshot();
        assert_eq!(failure.phase, Phase::Failure);
        if unknown_cost {
            let detail = failure.detail.to_ascii_lowercase();
            for jargon in [
                "token",
                "protocol",
                "websocket",
                "message task",
                "reservation",
            ] {
                assert!(!detail.contains(jargon), "{failure:?}");
            }
            assert!(detail.contains("no audio was sent"), "{failure:?}");
            assert!(detail.contains("use local recognition"), "{failure:?}");
            assert!(detail.contains("configuration guide"), "{failure:?}");
        }
        assert!(failure.detail.contains(expected), "{failure:?}");
        let backend = failure.backend.as_deref().unwrap();
        for selected in [
            "Alibaba Model Studio",
            "qwen-audio-3.1-asr-flash-message",
            "cn-beijing",
        ] {
            assert!(backend.contains(selected), "{failure:?}");
        }
        assert!(playback.0.lock().unwrap().is_none());
        assert!(ledger.snapshot().unwrap().attempts.is_empty());
    }
    #[cfg(windows)]
    {
        assert!(
            listen::cloud::NativeMessageTransport
                .verified_bounds(&AliSettings::beijing("offline-workspace".into()))
                .is_none(),
            "configuring a real key cannot unlock the unproved production policy"
        );
    }
}

#[test]
fn a_cloud_save_failure_preserves_complete_text_and_pcm_without_writing_the_target() {
    let mut scenario = Scenario::new();
    fs::create_dir_all(scenario.runtime.0.join("app/current-result.txt")).unwrap();
    let task = scenario.start();
    scenario.sentence(&task, 1, "完整结果", json!({}));
    scenario.stop();
    scenario.send(&task, "task-finished", json!({}));
    let failure = wait(&mut scenario.app, Phase::Failure);
    assert_eq!(failure.text, "完整结果");
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    assert!(!failure.retained_audio.is_empty());
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
}

#[test]
fn confirmed_silence_preserves_the_previous_copyable_result_and_does_not_deliver_empty_text() {
    let mut scenario = Scenario::new();
    fs::create_dir_all(scenario.runtime.0.join("app")).unwrap();
    fs::write(
        scenario.runtime.0.join("app/current-result.txt"),
        "previous complete result",
    )
    .unwrap();
    // Load the prior result through the same application construction seam.
    let (input, incoming) = mpsc::channel();
    let (output, outgoing) = mpsc::channel();
    let sessions = CloudSessions::new(
        Arc::new(Device(scenario.playback.clone())),
        Arc::new(Remote {
            inbound: Mutex::new(Some(incoming)),
            outbound: output,
        }),
        Arc::new(Key),
        AliSettings::beijing("offline-workspace".into()),
        scenario.ledger.clone(),
    );
    scenario.app = Application::with_recognition_sessions(
        Box::new(sessions),
        scenario.runtime.0.join("app"),
        Ok(()),
    )
    .with_targets(Arc::new(Selection(scenario.editor.clone())));
    scenario.input = input;
    scenario.outgoing = outgoing;
    let task = scenario.start();
    scenario.send(
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":0,"heartbeat":true,"text":"invented heartbeat text","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.send(&task, "task-finished", json!({}));
    let silent = wait(&mut scenario.app, Phase::NoSpeech);
    assert_eq!(silent.text, "previous complete result");
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    assert_eq!(
        fs::read_to_string(scenario.runtime.0.join("app/current-result.txt")).unwrap(),
        "previous complete result"
    );
}

struct ReadOnlySelection(Arc<Editor>);
struct ReadOnlyBound(Arc<Editor>);
impl TargetSelection for ReadOnlySelection {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        Ok(Some(Box::new(ReadOnlyBound(self.0.clone()))))
    }
}
impl Target for ReadOnlyBound {
    fn content(&mut self) -> AppResult<String> {
        Ok(self.0.0.lock().unwrap().clone())
    }
    fn replace(&mut self, _: &str, _: &str) -> AppResult<()> {
        Err("target became read-only".into())
    }
}
#[test]
fn a_read_only_original_target_keeps_one_complete_saved_fallback_without_local_text_filtering() {
    let mut scenario = Scenario::new();
    scenario.app = scenario
        .app
        .with_targets(Arc::new(ReadOnlySelection(scenario.editor.clone())));
    let task = scenario.start();
    scenario.sentence(&task, 2, "不确定，不确定。", json!({}));
    scenario.sentence(&task, 1, "谢谢观看。", json!({}));
    scenario.sentence(&task, 1, "谢谢观看。", json!({}));
    scenario.stop();
    let final_event = event(&task, "task-finished", json!({}));
    scenario.input.send(Ok(final_event.clone())).unwrap();
    let _ = scenario.input.send(Ok(final_event)); // A duplicate may arrive as the socket closes.
    let result = wait(&mut scenario.app, Phase::Result);
    scenario.app.dispatch(Control::Stop);
    scenario.app.poll();
    assert_eq!(result.text, "谢谢观看。不确定，不确定。");
    assert_eq!(result.delivery, Some(Delivery::Fallback));
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    assert_eq!(scenario.app.snapshot().text, result.text);
    assert_eq!(
        fs::read_to_string(scenario.runtime.0.join("app/current-result.txt")).unwrap(),
        result.text
    );
}

#[test]
fn account_errors_are_actionable_and_remote_credentials_or_audio_echoes_are_never_reported() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario
        .input
        .send(Ok(json!({"header":{
        "task_id":task,"event":"task-failed","error_code":"Arrearage",
        "error_message":"offline-test-key private audio echo"
    },"payload":{}})
        .to_string()))
        .unwrap();
    let failure = wait(&mut scenario.app, Phase::Failure);
    assert!(
        failure
            .detail
            .contains("balance, quota or a billing budget")
    );
    let observable = format!("{failure:?} {:?}", scenario.ledger.snapshot().unwrap());
    assert!(!observable.contains("offline-test-key"));
    assert!(!observable.contains("private audio echo"));
    assert_eq!(*scenario.editor.0.lock().unwrap(), "protected draft");
    assert!(!failure.retained_audio.is_empty());
}

struct ReserveOnly;
impl MessageTransport for ReserveOnly {
    fn verified_bounds(&self, _: &AliSettings) -> Option<VerifiedBounds> {
        Some(VerifiedBounds {
            max_input_tokens: 5_000_000,
            max_output_tokens: 0,
            max_pcm_bytes: 1_920_000,
            evidence: "controlled offline fixture; no paid network".into(),
            verified_on: "2026-11-14".into(),
        })
    }
    fn connect(&self, _: &AliSettings, _: &Secret) -> Result<Box<dyn MessageSocket>, CloudError> {
        Err(CloudError::Network)
    }
}
#[test]
fn another_runtime_and_restart_cannot_spend_a_pending_reservation_again() {
    let mut scenario = Scenario::new();
    let task = scenario.start();
    scenario.stop();
    scenario.input.send(Err(CloudError::Network)).unwrap();
    wait(&mut scenario.app, Phase::Failure);
    drop(scenario.app);
    let reopened = TrialLedger::new(scenario.runtime.0.join("shared-ledger"));
    let sessions = CloudSessions::new(
        Arc::new(Device(Arc::new(Playback::default()))),
        Arc::new(ReserveOnly),
        Arc::new(Key),
        AliSettings::beijing("offline-workspace".into()),
        reopened.clone(),
    );
    let mut restarted = Application::with_recognition_sessions(
        Box::new(sessions),
        scenario.runtime.0.join("different-runtime"),
        Ok(()),
    );
    restarted.dispatch(Control::Start);
    assert_eq!(restarted.snapshot().phase, Phase::Failure);
    assert!(
        restarted
            .snapshot()
            .detail
            .contains("shared CNY 50 trial budget")
    );
    let ledger = reopened.snapshot().unwrap();
    assert_eq!(ledger.attempts.len(), 1);
    assert_eq!(ledger.attempts[0].task_id, task);
    assert_eq!(ledger.committed_nanoyuan, 28_500_000_000);
}

#[test]
#[ignore = "offline child-process actor, launched by the concurrent-budget test"]
fn cross_process_budget_actor() {
    let Some(directory) = std::env::var_os("LISTEN_TEST_LEDGER_DIRECTORY") else {
        return;
    };
    let runtime = Runtime::new();
    let sessions = CloudSessions::new(
        Arc::new(Device(Arc::new(Playback::default()))),
        Arc::new(ReserveOnly),
        Arc::new(Key),
        AliSettings::beijing("offline-workspace".into()),
        TrialLedger::new(PathBuf::from(directory)),
    );
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.join("app"), Ok(()));
    app.dispatch(Control::Start);
    if app.snapshot().phase == Phase::Capturing {
        wait(&mut app, Phase::Failure);
        println!("budget_actor=admitted");
    } else {
        let detail = app.snapshot().detail;
        assert!(
            detail.contains("shared CNY 50 trial budget")
                || detail.contains("in use by another process"),
            "{detail}"
        );
        println!("budget_actor=denied");
    }
}
#[test]
fn concurrent_processes_reserve_before_connect_and_cannot_exceed_the_shared_cap() {
    use std::process::{Command, Stdio};
    let runtime = Runtime::new();
    let directory = runtime.0.join("ledger");
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cross_process_budget_actor",
                    "--ignored",
                    "--nocapture",
                ])
                .env("LISTEN_TEST_LEDGER_DIRECTORY", &directory)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut admitted = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        admitted +=
            usize::from(String::from_utf8_lossy(&output.stdout).contains("budget_actor=admitted"));
    }
    assert_eq!(
        admitted, 1,
        "four processes cannot each claim the same trial funds"
    );
    let ledger = TrialLedger::new(directory).snapshot().unwrap();
    assert_eq!(ledger.attempts.len(), 1);
    assert_eq!(ledger.committed_nanoyuan, 30_000_000_000);
}

struct GatedConnect {
    remote: Remote,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl MessageTransport for GatedConnect {
    fn verified_bounds(&self, settings: &AliSettings) -> Option<VerifiedBounds> {
        self.remote.verified_bounds(settings)
    }
    fn connect(
        &self,
        settings: &AliSettings,
        key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        self.remote.connect(settings, key)
    }
}
#[test]
fn stopping_while_the_connection_is_blocked_stops_capture_before_the_handshake_returns() {
    let runtime = Runtime::new();
    let playback = Arc::new(Playback::default());
    let (input, incoming) = mpsc::channel();
    let (output, outgoing) = mpsc::channel();
    let (entered, connecting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let sessions = CloudSessions::new(
        Arc::new(Device(playback.clone())),
        Arc::new(GatedConnect {
            remote: Remote {
                inbound: Mutex::new(Some(incoming)),
                outbound: output,
            },
            entered,
            release: Mutex::new(gate),
        }),
        Arc::new(Key),
        AliSettings::beijing("offline-workspace".into()),
        TrialLedger::new(runtime.0.join("ledger")),
    );
    let mut app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.join("app"), Ok(()));
    app.dispatch(Control::Start);
    playback.speak();
    connecting.recv_timeout(Duration::from_secs(5)).unwrap();
    app.dispatch(Control::Stop);
    thread::sleep(Duration::from_millis(50));
    playback.speak(); // Playback after Stop must fall outside this recording.
    app.poll();
    let received_at_stop = app.snapshot().progress.received_samples;
    release.send(()).unwrap();
    let run = receive_text(&outgoing, "run-task");
    let task = run["header"]["task_id"].as_str().unwrap();
    input
        .send(Ok(event(task, "task-started", json!({}))))
        .unwrap();
    receive_text(&outgoing, "finish-task");
    input
        .send(Ok(event(task, "task-finished", json!({}))))
        .unwrap();
    wait(&mut app, Phase::NoSpeech);
    assert_eq!(
        received_at_stop, 1600,
        "the stop boundary is independent of slow network setup"
    );
}

#[test]
fn an_unreadable_glossary_refuses_cloud_trial_instead_of_silently_uploading_without_the_snapshot() {
    let mut scenario = Scenario::new();
    let path = scenario.runtime.0.join("glossary.txt");
    fs::write(&path, [0xff, 0xff]).unwrap();
    scenario.app = scenario.app.with_glossary(path);
    scenario.app.dispatch(Control::Start);
    let failure = scenario.app.snapshot();
    assert_eq!(failure.phase, Phase::Failure);
    assert!(failure.detail.contains("glossary"));
    assert!(scenario.ledger.snapshot().unwrap().attempts.is_empty());
    assert!(scenario.playback.0.lock().unwrap().is_none());
}

//! Network-loss and recovery workflows through the application seam.
//!
//! The only substitutes are the playback device, credential store and remote
//! socket. Every assertion is a user-visible state, complete text, target
//! protection or audio lifecycle outcome observed through `Application::dispatch`,
//! `poll` and `snapshot`; no paid network is contacted.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, PlaybackAudio, Recording,
    Snapshot, Target, TargetSelection,
    cloud::{
        AliSettings, CloudError, CloudSessions, CredentialSource, FreeOnlyPolicy, MessageSocket,
        MessageTransport, PcmSpoolWriter, PcmStream, Secret, TrialClock, TrialCostPolicy,
        TrialLedger,
    },
    recognition::SessionState,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
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
            "listen-cloud-offline-{}-{}-{}",
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

/// Continuous 16 kHz mono PCM written to disk as the controlled playback device
/// receives it. `speak` is the only way audio enters a capture.
#[derive(Default)]
struct Playback {
    writer: Mutex<Option<PcmSpoolWriter>>,
    failure: Mutex<Option<String>>,
}
struct Device(Arc<Playback>);
struct Capture {
    playback: Arc<Playback>,
    stream: Option<PcmStream>,
}
impl PlaybackAudio for Device {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let (writer, stream) = PcmSpoolWriter::new(directory, 2_000_000)?;
        *self.0.writer.lock().unwrap() = Some(writer);
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
    fn failure(&self) -> Option<String> {
        self.playback.failure.lock().unwrap().clone()
    }
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        let writer = self.playback.writer.lock().unwrap().take();
        if let Some(reason) = self.playback.failure.lock().unwrap().clone() {
            // Dropping the writer flushes the received audio for recovery.
            drop(writer);
            return Err(reason);
        }
        writer
            .expect("the controlled playback device must still own its spool")
            .finish("controlled playback".into())
    }
}
impl Playback {
    /// Push `samples` i16 samples and publish the current chunk, exactly like a
    /// 100 ms tick of received headphone audio.
    fn speak(&self, samples: usize) {
        let mut capture = self.writer.lock().unwrap();
        let writer = capture.as_mut().expect("no capture is recording");
        for _ in 0..samples {
            writer.push(7).unwrap();
        }
        writer.flush().unwrap();
    }
    fn fail(&self, reason: &str) {
        *self.failure.lock().unwrap() = Some(reason.into());
    }
    fn is_recording(&self) -> bool {
        self.writer.lock().unwrap().is_some()
    }
}

struct Key;
impl CredentialSource for Key {
    fn load(&self) -> Result<Secret, CloudError> {
        Secret::new("offline-test-key".into())
    }
}

fn settings() -> AliSettings {
    AliSettings::beijing("offline-workspace".into())
}

enum Outbound {
    Text(Value),
    Audio(Vec<u8>),
}

enum ConnectStep {
    Fail(CloudError),
    Ready(mpsc::Receiver<Result<String, CloudError>>),
}

/// A remote that hands out exactly the connections the test prepared. Once the
/// script is empty every connect fails with `Network`, which models an outage
/// without any real socket.
struct ScriptedTransport {
    steps: Mutex<VecDeque<ConnectStep>>,
    outbound: mpsc::Sender<Outbound>,
    connects: Mutex<u32>,
}
impl ScriptedTransport {
    fn new(outbound: mpsc::Sender<Outbound>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(VecDeque::new()),
            outbound,
            connects: Mutex::new(0),
        })
    }
    /// Make the next connection succeed. The returned sender delivers the remote
    /// side of that one connection.
    fn accept(&self) -> mpsc::Sender<Result<String, CloudError>> {
        let (sender, receiver) = mpsc::channel();
        self.steps
            .lock()
            .unwrap()
            .push_back(ConnectStep::Ready(receiver));
        sender
    }
    fn fail(&self, error: CloudError) {
        self.steps
            .lock()
            .unwrap()
            .push_back(ConnectStep::Fail(error));
    }
    fn connect_count(&self) -> u32 {
        *self.connects.lock().unwrap()
    }
}
impl MessageTransport for ScriptedTransport {
    fn cost_policy(
        &self,
        settings: &AliSettings,
        _: &Secret,
    ) -> Result<TrialCostPolicy, CloudError> {
        Ok(TrialCostPolicy::FreeOnly(FreeOnlyPolicy {
            model: settings.model.clone(),
            region: settings.region.clone(),
            evidence: "controlled offline fixture: no paid network".into(),
            verified_on: "2026-11-21".into(),
        }))
    }
    fn connect(&self, _: &AliSettings, _: &Secret) -> Result<Box<dyn MessageSocket>, CloudError> {
        *self.connects.lock().unwrap() += 1;
        match self.steps.lock().unwrap().pop_front() {
            Some(ConnectStep::Ready(inbound)) => Ok(Box::new(Socket {
                inbound,
                outbound: self.outbound.clone(),
            })),
            Some(ConnectStep::Fail(error)) => Err(error),
            None => Err(CloudError::Network),
        }
    }
}

struct Socket {
    inbound: mpsc::Receiver<Result<String, CloudError>>,
    outbound: mpsc::Sender<Outbound>,
}
impl MessageSocket for Socket {
    fn send_text(&mut self, text: &str) -> Result<(), CloudError> {
        self.outbound
            .send(Outbound::Text(serde_json::from_str(text).unwrap()))
            .map_err(|_| CloudError::Network)
    }
    fn send_audio(&mut self, pcm: &[u8]) -> Result<(), CloudError> {
        self.outbound
            .send(Outbound::Audio(pcm.to_vec()))
            .map_err(|_| CloudError::Network)
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

struct Scenario {
    runtime: Runtime,
    app: Application,
    playback: Arc<Playback>,
    editor: Arc<Editor>,
    transport: Arc<ScriptedTransport>,
    ledger: TrialLedger,
    outgoing: mpsc::Receiver<Outbound>,
    original: String,
}
impl Scenario {
    fn new() -> Self {
        Self::build("protected draft", None)
    }
    fn with_clock(draft: &'static str, clock: Arc<dyn TrialClock>) -> Self {
        Self::build(draft, Some(clock))
    }
    fn build(draft: &'static str, clock: Option<Arc<dyn TrialClock>>) -> Self {
        let runtime = Runtime::new();
        let playback = Arc::new(Playback::default());
        let editor = Arc::new(Editor(Mutex::new(draft.into())));
        let (outbound, outgoing) = mpsc::channel();
        let transport = ScriptedTransport::new(outbound);
        let ledger = TrialLedger::new(runtime.0.join("ledger"));
        let mut sessions = CloudSessions::new(
            Arc::new(Device(playback.clone())),
            transport.clone(),
            Arc::new(Key),
            settings(),
            ledger.clone(),
        );
        if let Some(clock) = clock {
            sessions = sessions.with_clock(clock);
        }
        let app = Application::with_recognition_sessions(Box::new(sessions), runtime.app(), Ok(()))
            .with_targets(Arc::new(Selection(editor.clone())));
        Self {
            runtime,
            app,
            playback,
            editor,
            transport,
            ledger,
            outgoing,
            original: draft.into(),
        }
    }
    fn original_draft(&self) -> &str {
        &self.original
    }
    fn start(&mut self) {
        self.app.dispatch(Control::Start);
        assert_eq!(self.app.snapshot().phase, Phase::Capturing);
    }
    fn speak(&self, samples: usize) {
        self.playback.speak(samples);
    }
    fn stop(&mut self) {
        self.app.dispatch(Control::Stop);
    }
    fn draft(&self) -> String {
        self.editor.0.lock().unwrap().clone()
    }
    fn saved_result(&self) -> String {
        fs::read_to_string(self.runtime.app().join("current-result.txt")).unwrap_or_default()
    }
    /// Wait for a user-visible state through the public application entry.
    fn wait_for(&mut self, what: &str, matches: impl Fn(&Snapshot) -> bool) -> Snapshot {
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
    fn wait_phase(&mut self, phase: Phase) -> Snapshot {
        self.wait_for(&format!("{phase:?}"), |snapshot| snapshot.phase == phase)
    }
    fn waiting_for_network(&mut self) -> Snapshot {
        self.wait_for("the waiting-for-network state", |snapshot| {
            snapshot.recognition == Some(SessionState::WaitingForNetwork)
        })
    }
    fn receive_text(&self, action: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self
                .outgoing
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no {action} request was sent"))
            {
                Outbound::Text(value) => {
                    assert_eq!(value["header"]["action"], action);
                    return value;
                }
                Outbound::Audio(_) => continue,
            }
        }
    }
    fn receive_audio(&self, samples: usize) -> Vec<u8> {
        let expected = samples * 2;
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while received.len() < expected {
            match self
                .outgoing
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!("only {} of {expected} audio bytes arrived", received.len())
                }) {
                Outbound::Audio(bytes) => received.extend(bytes),
                Outbound::Text(value) => panic!("unexpected request before audio: {value}"),
            }
        }
        received
    }
    fn event(
        &self,
        sender: &mpsc::Sender<Result<String, CloudError>>,
        task: &str,
        kind: &str,
        payload: Value,
    ) {
        sender
            .send(Ok(json!({
                "header": { "task_id": task, "event": kind },
                "payload": payload
            })
            .to_string()))
            .unwrap();
    }
}

/// Every audio file still under the runtime directory. Used only to observe the
/// audio lifecycle, never to address a specific temporary name.
fn audio_files(root: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("pcm" | "wav")
            ) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    if root.exists() {
        walk(root, &mut out);
    }
    out
}

#[test]
fn capture_started_while_offline_keeps_the_beginning_and_recovers_without_re_recording() {
    let mut scenario = Scenario::new();
    scenario.start();
    // The beginning is spoken before any cloud connection exists. It must not be
    // lost just because the handshake is late.
    scenario.speak(1600);
    let waiting = scenario.waiting_for_network();
    assert_eq!(waiting.phase, Phase::Capturing);
    assert!(
        waiting
            .detail
            .to_lowercase()
            .contains("waiting for the network"),
        "{waiting:?}"
    );
    assert!(waiting.text.is_empty());
    assert_eq!(scenario.draft(), scenario.original_draft());
    assert_eq!(scenario.app.snapshot().phase, Phase::Capturing);
    assert!(scenario.playback.is_recording());

    // The network comes back on a later automatic attempt.
    let remote = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&remote, &task, "task-started", json!({}));
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));
    scenario.event(
        &remote,
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"开头也保留。","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.receive_text("finish-task");
    scenario.event(&remote, &task, "task-finished", json!({}));

    let result = scenario.wait_phase(Phase::Result);
    assert_eq!(result.text, "开头也保留。");
    // The draft recorded when the capture stopped is still intact, so the late
    // result is appended once to its end (#34 safe late backfill).
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert!(
        result.detail.to_lowercase().contains("recovered")
            || result
                .detail
                .to_lowercase()
                .contains("waiting for the network"),
        "{result:?}"
    );
    assert_eq!(
        scenario.draft(),
        format!("{}\n开头也保留。", scenario.original_draft())
    );
    assert_eq!(scenario.saved_result(), "开头也保留。");
    assert!(
        audio_files(&scenario.runtime.0).is_empty(),
        "the saved complete result must release the production audio"
    );
    let attempts = scenario.ledger.snapshot().unwrap().attempts;
    assert!(
        attempts.len() >= 2,
        "each connection attempt is a separate, recorded request"
    );
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.segment_id == attempts[0].segment_id),
        "request and cost state stay tied to this one capture"
    );
}

#[test]
fn a_mid_capture_drop_keeps_recording_and_rebuilds_the_task_from_retained_audio() {
    let mut scenario = Scenario::new();
    scenario.start();
    let first = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let first_task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&first, &first_task, "task-started", json!({}));
    scenario.speak(1600);
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));

    // The remote dies while capture continues. This must not end the capture nor
    // deliver the sentence that happened to be stable before the drop.
    first.send(Err(CloudError::Network)).unwrap();
    let waiting = scenario.waiting_for_network();
    assert_eq!(waiting.phase, Phase::Capturing);
    assert!(waiting.retained_audio.is_empty());
    scenario.speak(800);
    assert!(scenario.playback.is_recording());

    // Recovery rebuilds the task from the retained continuous audio. The vendor
    // offers no resumable session here, so the whole recording is replayed.
    let second = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let second_task = run["header"]["task_id"].as_str().unwrap().to_string();
    assert_ne!(second_task, first_task, "a rebuild is a new remote request");
    scenario.event(&second, &second_task, "task-started", json!({}));
    assert_eq!(
        scenario.receive_audio(2400),
        vec![7, 0].repeat(2400),
        "the rebuilt task receives the complete retained recording exactly once"
    );
    scenario.event(
        &second,
        &second_task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"第一句。","sentence_end":true
        }}}),
    );
    scenario.event(
        &second,
        &second_task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":2,"text":"第二句。","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.receive_text("finish-task");
    scenario.event(&second, &second_task, "task-finished", json!({}));

    let result = scenario.wait_phase(Phase::Result);
    assert_eq!(
        result.text, "第一句。第二句。",
        "a rebuilt session must not duplicate or drop text from the dropped one"
    );
    // The capture's own target still holds its Stop-time draft, so the rebuilt
    // result is a safe late append to that same editor's latest draft.
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        scenario.draft(),
        format!("{}\n第一句。第二句。", scenario.original_draft())
    );
    assert_eq!(scenario.saved_result(), result.text);
    assert!(
        audio_files(&scenario.runtime.0).is_empty(),
        "the saved complete result must release the production audio"
    );
}

#[test]
fn a_drop_at_stop_waits_for_the_network_and_confirms_the_saved_recording_once() {
    let mut scenario = Scenario::new();
    scenario.start();
    let first = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let first_task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&first, &first_task, "task-started", json!({}));
    scenario.speak(1600);
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));
    scenario.stop();
    scenario.receive_text("finish-task");

    // The connection dies while waiting for whole-task confirmation. Nothing is
    // written to the target and the complete recording stays on this machine.
    first.send(Err(CloudError::Network)).unwrap();
    let waiting = scenario.waiting_for_network();
    assert_eq!(waiting.phase, Phase::Processing);
    assert!(
        waiting
            .detail
            .to_lowercase()
            .contains("waiting for the network"),
        "{waiting:?}"
    );
    assert!(waiting.text.is_empty());
    assert_eq!(scenario.draft(), scenario.original_draft());
    assert!(
        !audio_files(&scenario.runtime.0).is_empty(),
        "the recording must be kept while the cloud is unreachable"
    );

    // Recovery rebuilds the whole task from the complete saved recording.
    let second = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let second_task = run["header"]["task_id"].as_str().unwrap().to_string();
    assert_ne!(second_task, first_task);
    scenario.event(&second, &second_task, "task-started", json!({}));
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));
    scenario.receive_text("finish-task");
    scenario.event(
        &second,
        &second_task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"等待后的完整结果。","sentence_end":true
        }}}),
    );
    scenario.event(&second, &second_task, "task-finished", json!({}));
    // A duplicate confirmation must not produce a second result or target write.
    scenario.event(&second, &second_task, "task-finished", json!({}));

    let result = scenario.wait_phase(Phase::Result);
    assert_eq!(result.text, "等待后的完整结果。");
    // The whole stopped capture was confirmed once, and its Stop-time draft is
    // intact, so the late result lands directly after that draft exactly once.
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        scenario.draft(),
        format!("{}\n等待后的完整结果。", scenario.original_draft())
    );
    assert_eq!(scenario.saved_result(), result.text);
    scenario.app.dispatch(Control::Stop);
    scenario.app.poll();
    assert_eq!(scenario.app.snapshot().text, result.text);
    assert_eq!(
        scenario.draft(),
        format!("{}\n等待后的完整结果。", scenario.original_draft()),
        "a repeated Stop and poll never append the same result twice"
    );
    assert!(audio_files(&scenario.runtime.0).is_empty());
}

#[test]
fn account_and_model_errors_keep_the_capture_alive_and_are_never_retried() {
    for (code, expected) in [
        ("Arrearage", "balance, quota or a billing budget"),
        ("AccessDenied.Unpurchased", "unavailable or not enabled"),
        ("InvalidApiKey", "rejected authentication"),
    ] {
        let mut scenario = Scenario::new();
        let remote = scenario.transport.accept();
        scenario.start();
        let run = scenario.receive_text("run-task");
        let task = run["header"]["task_id"].as_str().unwrap().to_string();
        scenario.event(&remote, &task, "task-started", json!({}));
        scenario.speak(800);
        assert_eq!(scenario.receive_audio(800), vec![7, 0].repeat(800));
        remote
            .send(Ok(json!({
                "header":{
                    "task_id":task,"event":"task-failed","error_code":code,
                    "error_message":"offline-test-key private audio echo"
                },
                "payload":{}
            })
            .to_string()))
            .unwrap();

        // The problem is actionable and visible, but the capture is not ended:
        // the meeting audio keeps being saved for a later retry.
        let reported = scenario.wait_for("the actionable account message", |snapshot| {
            snapshot.detail.contains(expected)
        });
        assert_eq!(reported.phase, Phase::Capturing);
        assert_eq!(reported.recognition, Some(SessionState::Capturing));
        assert!(
            !reported
                .detail
                .to_lowercase()
                .contains("waiting for the network"),
            "an account error is not a network wait: {reported:?}"
        );
        assert_eq!(
            scenario.transport.connect_count(),
            1,
            "a credential/balance/model error must not be retried at all"
        );
        let observable = format!("{reported:?}");
        assert!(!observable.contains("offline-test-key"));
        assert!(!observable.contains("private audio echo"));
        scenario.speak(400);
        assert!(scenario.playback.is_recording());

        scenario.stop();
        let failure = scenario.wait_phase(Phase::Failure);
        assert_eq!(failure.recognition, Some(SessionState::Failed));
        assert!(failure.text.is_empty());
        assert_eq!(scenario.draft(), scenario.original_draft());
        assert!(
            failure.retry_available,
            "the retained recording must be retryable after the account is fixed"
        );
        assert!(!failure.retained_audio.is_empty());
        assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    }
}

#[test]
fn a_playback_device_failure_is_a_capture_failure_not_a_network_wait() {
    let mut scenario = Scenario::new();
    scenario.start();
    scenario.speak(800);
    let waiting = scenario.waiting_for_network();
    assert_eq!(waiting.phase, Phase::Capturing);
    // The audio source itself disappears while the network is still down. The
    // user must be told the capture failed, not that waiting will restore it.
    scenario
        .playback
        .fail("The playback device was removed during capture");
    let failure = scenario.wait_phase(Phase::Failure);
    assert_eq!(failure.recognition, Some(SessionState::Failed));
    assert!(
        failure.detail.to_lowercase().contains("playback device"),
        "{failure:?}"
    );
    assert!(
        failure.detail.contains("this capture ended"),
        "a device failure is reported as a capture failure: {failure:?}"
    );
    assert!(
        !failure.detail.contains("Waiting for the network."),
        "the waiting-for-network status must not be shown for a failed device: {failure:?}"
    );
    assert!(!failure.retained_audio.is_empty());
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    assert_eq!(scenario.draft(), scenario.original_draft());
    let connects = scenario.transport.connect_count();
    thread::sleep(Duration::from_millis(150));
    scenario.app.poll();
    assert_eq!(
        scenario.transport.connect_count(),
        connects,
        "a failed device must not keep reconnecting"
    );
}

#[test]
fn a_credential_rejection_at_connect_keeps_the_capture_and_is_not_retried() {
    let mut scenario = Scenario::new();
    scenario.transport.fail(CloudError::Unauthorized);
    scenario.start();
    scenario.speak(800);
    let reported = scenario.wait_for("the credential message", |snapshot| {
        snapshot.phase == Phase::Capturing && snapshot.detail.contains("rejected authentication")
    });
    assert_eq!(reported.recognition, Some(SessionState::Capturing));
    assert_eq!(
        scenario.transport.connect_count(),
        1,
        "rejected credentials are not retried"
    );
    scenario.stop();
    let failure = scenario.wait_phase(Phase::Failure);
    assert!(failure.retry_available);
    assert!(!failure.retained_audio.is_empty());
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    assert_eq!(scenario.draft(), scenario.original_draft());
}

struct AdvancedClock(Mutex<Instant>);
impl AdvancedClock {
    fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }
    fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}
impl TrialClock for AdvancedClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

#[test]
fn a_connection_dropped_long_after_capture_start_gets_its_own_retry_window() {
    // The retry budget is anchored at the failure, not at capture start. A drop
    // more than twenty seconds after Start is still a transient outage and must
    // recover automatically instead of failing with zero retries.
    let clock = Arc::new(AdvancedClock::new());
    let mut scenario = Scenario::with_clock("protected draft", clock.clone());
    scenario.start();
    let first = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let first_task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&first, &first_task, "task-started", json!({}));
    scenario.speak(1600);
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));

    // The capture has been running well past the old 20-second window.
    clock.advance(Duration::from_secs(45));
    first.send(Err(CloudError::Network)).unwrap();
    let waiting = scenario.waiting_for_network();
    assert_eq!(waiting.phase, Phase::Capturing);
    assert!(scenario.playback.is_recording());

    // The retry connects and the complete retained recording is replayed once.
    let second = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let second_task = run["header"]["task_id"].as_str().unwrap().to_string();
    assert_ne!(second_task, first_task, "a rebuild is a new remote request");
    scenario.event(&second, &second_task, "task-started", json!({}));
    assert_eq!(scenario.receive_audio(1600), vec![7, 0].repeat(1600));
    scenario.event(
        &second,
        &second_task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"迟到断线也自动恢复。","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.receive_text("finish-task");
    scenario.event(&second, &second_task, "task-finished", json!({}));

    let result = scenario.wait_phase(Phase::Result);
    assert_eq!(result.text, "迟到断线也自动恢复。");
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        scenario.draft(),
        format!("{}\n迟到断线也自动恢复。", scenario.original_draft())
    );
}

#[test]
fn an_endless_failure_still_stops_after_the_bounded_attempts() {
    // Every attempt fails immediately and the injected clock never advances, so
    // only the attempt bound can stop the retry loop. The recording must stay
    // retryable instead of reconnecting forever.
    let clock = Arc::new(AdvancedClock::new());
    let mut scenario = Scenario::with_clock("protected draft", clock);
    scenario.start();
    scenario.speak(1600);
    scenario.waiting_for_network();
    scenario.stop();

    let deadline = Instant::now() + Duration::from_secs(20);
    let failure = loop {
        scenario.app.poll();
        let snapshot = scenario.app.snapshot();
        if snapshot.phase == Phase::Failure {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "an endless failure never stopped: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(2));
    };
    assert!(failure.text.is_empty());
    assert!(failure.retry_available, "{failure:?}");
    assert!(!failure.retained_audio.is_empty());
    assert_eq!(
        scenario.transport.connect_count(),
        8,
        "the attempt budget must be bounded, not reset by every failure"
    );
    assert_eq!(scenario.draft(), scenario.original_draft());
}

#[test]
fn bounded_automatic_retries_then_an_explicit_retry_replays_the_retained_recording() {
    let clock = Arc::new(AdvancedClock::new());
    let mut scenario = Scenario::with_clock("protected draft", clock.clone());
    scenario.start();
    scenario.speak(1600);
    scenario.waiting_for_network();
    scenario.stop();

    // Exhaust the bounded automatic retry budget without waiting in real time.
    clock.advance(Duration::from_secs(21));
    let failure = scenario.wait_phase(Phase::Failure);
    assert!(failure.text.is_empty());
    assert_eq!(failure.recognition, Some(SessionState::Failed));
    assert!(
        failure.retry_available,
        "the retained recording must stay retryable"
    );
    assert!(!failure.retained_audio.is_empty());
    assert_eq!(scenario.draft(), scenario.original_draft());
    let automatic = scenario.ledger.snapshot().unwrap().attempts;
    assert!(!automatic.is_empty());
    assert!(
        automatic.len() <= 8,
        "automatic reconnect requests must be bounded, made {}",
        automatic.len()
    );
    assert_eq!(
        scenario.transport.connect_count(),
        automatic.len() as u32,
        "every recorded request belongs to one bounded connection attempt"
    );
    assert!(
        automatic
            .iter()
            .all(|attempt| attempt.segment_id == automatic[0].segment_id),
        "retry cost stays attached to this one recording"
    );

    // The network is back. The explicit Retry reprocesses the same audio without
    // asking the user to record again; its Stop-time draft never changed, so the
    // retried result is a safe late append to that same target.
    let remote = scenario.transport.accept();
    scenario.app.dispatch(Control::Retry);
    let run = scenario.receive_text("run-task");
    let task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&remote, &task, "task-started", json!({}));
    assert_eq!(
        scenario.receive_audio(1600),
        vec![7, 0].repeat(1600),
        "the retry sends exactly the retained recording"
    );
    scenario.receive_text("finish-task");
    scenario.event(
        &remote,
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"补处理的完整结果。","sentence_end":true
        }}}),
    );
    scenario.event(&remote, &task, "task-finished", json!({}));

    let result = scenario.wait_phase(Phase::Result);
    assert_eq!(result.text, "补处理的完整结果。");
    assert_eq!(result.delivery, Some(Delivery::Appended));
    assert_eq!(
        scenario.draft(),
        format!("{}\n补处理的完整结果。", scenario.original_draft())
    );
    assert_eq!(scenario.saved_result(), result.text);
    assert!(audio_files(&scenario.runtime.0).is_empty());
    let attempts = scenario.ledger.snapshot().unwrap().attempts;
    assert!(attempts.len() > automatic.len());
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.segment_id == automatic[0].segment_id),
        "the retry request and its cost stay attached to the same recording"
    );
}

#[test]
fn a_recovered_result_that_cannot_be_saved_keeps_the_audio_and_never_writes_the_target() {
    let mut scenario = Scenario::new();
    // Make the reliable save fail without replacing any existing result.
    fs::create_dir_all(scenario.runtime.app().join("current-result.txt")).unwrap();
    scenario.start();
    scenario.speak(800);
    scenario.waiting_for_network();
    let remote = scenario.transport.accept();
    let run = scenario.receive_text("run-task");
    let task = run["header"]["task_id"].as_str().unwrap().to_string();
    scenario.event(&remote, &task, "task-started", json!({}));
    assert_eq!(scenario.receive_audio(800), vec![7, 0].repeat(800));
    scenario.event(
        &remote,
        &task,
        "result-generated",
        json!({"output":{"sentence":{
            "sentence_id":1,"text":"必须先保存的完整文字。","sentence_end":true
        }}}),
    );
    scenario.stop();
    scenario.receive_text("finish-task");
    scenario.event(&remote, &task, "task-finished", json!({}));

    let failure = scenario.wait_phase(Phase::Failure);
    assert_eq!(
        failure.text, "必须先保存的完整文字。",
        "the complete text stays available in the panel even when saving fails"
    );
    assert_eq!(scenario.draft(), scenario.original_draft());
    assert!(
        !failure.retained_audio.is_empty(),
        "a save failure must keep the audio as the recovery basis"
    );
    assert!(failure.retained_audio.iter().all(|path| path.is_file()));
    assert!(
        !audio_files(&scenario.runtime.0).is_empty(),
        "audio is only released after the complete text is reliably saved"
    );
}

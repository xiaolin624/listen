//! Quit-and-restart recovery through the application interface (#35).
//!
//! Every test replaces only the recognition sessions and the editors. A second
//! `Application` is constructed over exactly the same runtime directory, so it
//! inherits nothing but what the first process left on disk. Every assertion is a
//! user-visible state, complete text, target content or file on disk observed
//! through `Application::dispatch`, `poll` and `snapshot`; no paid network is
//! contacted and no real playback device is used.
use listen::{
    AppResult, Application, CapturedAudio, Control, Delivery, Phase, SegmentState, Snapshot,
    Target, TargetSelection,
    cloud::PcmSpoolWriter,
    recognition::{
        RecognitionReadiness, RecognitionSession, RecognitionSessions, SegmentUsage, SessionEvent,
        SessionFailure, SessionId, SessionRequest, SessionResult, SessionStatus, SessionUpdate,
    },
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-restart-recovery-{}-{}-{}",
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
    fn path(&self) -> &Path {
        &self.0
    }
    fn app(&self) -> PathBuf {
        let path = self.0.join("app");
        // A crash window is written before any application exists, so the previous
        // process's runtime directory must be creatable here.
        fs::create_dir_all(&path).unwrap();
        path
    }
    /// Write the glossary the first process captures with and return its path.
    fn glossary(&self, text: &str) -> PathBuf {
        let path = self.0.join("glossary.txt");
        fs::write(&path, text).unwrap();
        path
    }
    /// Every session directory still on disk, in name order.
    fn session_directories(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = fs::read_dir(self.app())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_dir()
                            && path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| name.starts_with("session-"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Editor(Mutex<String>);
impl Editor {
    fn new(text: &str) -> Arc<Self> {
        Arc::new(Self(Mutex::new(text.into())))
    }
    fn draft(&self) -> String {
        self.0.lock().unwrap().clone()
    }
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
        Ok(self.0.draft())
    }
    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let mut draft = self.0.0.lock().unwrap();
        if *draft != expected {
            return Err("the draft changed while writing".into());
        }
        *draft = replacement.into();
        Ok(())
    }
}

/// One controlled recognition session. `start` writes a real continuous PCM
/// recording through the same writer the cloud device uses; stopping it finishes
/// that writer, so the clean-stop marker and the byte count are the production
/// format, not a test-only copy.
struct Started {
    id: SessionId,
    directory: PathBuf,
    writer: Mutex<Option<PcmSpoolWriter>>,
    /// Test → application: exactly one terminal event.
    events: Mutex<Option<mpsc::Sender<SessionEvent>>>,
    /// Application → test: the one event this session reports.
    receiver: Mutex<mpsc::Receiver<SessionEvent>>,
    stopped: AtomicBool,
    finalize: bool,
}
impl Started {
    fn finish(&self, text: &str, recovered: bool) {
        self.send(SessionUpdate::Finished(SessionResult {
            text: text.into(),
            description: "controlled recovered playback".into(),
            timing: listen::Timing::default(),
            stopped_at: None,
            recovered,
        }));
    }
    #[allow(dead_code)]
    fn fail(&self, error: &str) {
        self.send(SessionUpdate::Failed(SessionFailure {
            error: error.into(),
            retry_audio: None,
            retry_scope: None,
            timing: listen::Timing::default(),
        }));
    }
    fn send(&self, update: SessionUpdate) {
        let sender = self
            .events
            .lock()
            .unwrap()
            .take()
            .expect("this controlled session already reported its one event");
        sender
            .send(SessionEvent {
                id: self.id,
                update,
            })
            .unwrap();
    }
    /// Close the recording the way a stopped playback device does, including the
    /// durable clean-stop marker.
    fn close_recording(&self) {
        let writer = self.writer.lock().unwrap().take();
        if let Some(writer) = writer {
            writer
                .finish("controlled playback".into())
                .expect("the controlled recording must finish");
        }
    }
}

#[derive(Clone, Debug)]
struct Retry {
    directory: PathBuf,
    terms: Vec<String>,
    audio: PathBuf,
    audio_bytes: Vec<u8>,
}

#[derive(Default)]
struct Registry {
    entries: Mutex<Vec<Arc<Started>>>,
    retries: Mutex<Vec<Retry>>,
}
impl Registry {
    fn entry(&self, index: usize) -> Arc<Started> {
        self.entries.lock().unwrap()[index].clone()
    }
    fn count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
    fn finish(&self, index: usize, text: &str, recovered: bool) {
        self.entry(index).finish(text, recovered);
    }
    #[allow(dead_code)]
    fn fail(&self, index: usize, error: &str) {
        self.entry(index).fail(error);
    }
    fn retries(&self) -> Vec<Retry> {
        self.retries.lock().unwrap().clone()
    }
    /// Finish the newest controlled session. Tests that reopened an application
    /// share the registry, so the index of the segment under test is not assumed.
    fn finish_newest(&self, text: &str, recovered: bool) {
        let index = self.entries.lock().unwrap().len() - 1;
        self.finish(index, text, recovered);
    }
    fn directory(&self, index: usize) -> PathBuf {
        self.entry(index).directory.clone()
    }
}

struct ControlledSession(Arc<Started>);
impl RecognitionSession for ControlledSession {
    fn stop(&mut self) {
        if self.0.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.0.finalize {
            self.0.close_recording();
        }
    }
    fn status(&self) -> SessionStatus {
        SessionStatus {
            state: if self.0.stopped.load(Ordering::Acquire) {
                listen::recognition::SessionState::AwaitingFinal
            } else {
                listen::recognition::SessionState::Capturing
            },
            progress: listen::streaming::CaptureProgress::default(),
            endpoint: None,
            detail: "controlled restart recovery capture".into(),
        }
    }
    fn poll(&mut self) -> Option<SessionEvent> {
        self.0.receiver.lock().unwrap().try_recv().ok()
    }
}

/// A recognition adapter whose sessions the test finishes explicitly. It records
/// every retained recording the application asks it to process again, so a
/// restart can be observed through the application interface.
struct Sessions {
    registry: Arc<Registry>,
    samples: Vec<i16>,
    finalize: bool,
    /// Why a recovered recording cannot be processed again, if the test wants the
    /// automatic resume to fail.
    retry_failure: Option<String>,
}
impl Sessions {
    fn session(
        registry: &Arc<Registry>,
        id: SessionId,
        directory: PathBuf,
        writer: Option<PcmSpoolWriter>,
        finalize: bool,
        stopped: bool,
    ) -> Box<dyn RecognitionSession> {
        let (events, receiver) = mpsc::channel();
        let started = Arc::new(Started {
            id,
            directory,
            writer: Mutex::new(writer),
            events: Mutex::new(Some(events)),
            receiver: Mutex::new(receiver),
            stopped: AtomicBool::new(stopped),
            finalize,
        });
        registry.entries.lock().unwrap().push(started.clone());
        Box::new(ControlledSession(started))
    }
}
impl RecognitionSessions for Sessions {
    fn preparation(&mut self) -> RecognitionReadiness {
        RecognitionReadiness {
            state: listen::Preparation::Ready,
            backend: Some("controlled restart recognition".into()),
            elapsed_ms: None,
            error: None,
        }
    }
    fn start(&mut self, request: SessionRequest) -> AppResult<Box<dyn RecognitionSession>> {
        fs::create_dir(&request.directory).map_err(|error| error.to_string())?;
        let (mut writer, _stream) = PcmSpoolWriter::new(&request.directory, 2_000_000)?;
        for sample in &self.samples {
            writer.push(*sample).map_err(|error| error.to_string())?;
        }
        Ok(Self::session(
            &self.registry,
            request.id,
            request.directory,
            Some(writer),
            self.finalize,
            false,
        ))
    }
    fn retry(
        &mut self,
        request: SessionRequest,
        audio: CapturedAudio,
    ) -> AppResult<Box<dyn RecognitionSession>> {
        let bytes = fs::read(&audio.path).map_err(|error| error.to_string())?;
        // The attempt is what matters, whether or not the adapter could start it.
        self.registry.retries.lock().unwrap().push(Retry {
            directory: request.directory.clone(),
            terms: request
                .glossary
                .canonical_terms()
                .map(str::to_owned)
                .collect(),
            audio: audio.path.clone(),
            audio_bytes: bytes,
        });
        if let Some(reason) = &self.retry_failure {
            return Err(reason.clone());
        }
        Ok(Self::session(
            &self.registry,
            request.id,
            request.directory,
            None,
            false,
            true,
        ))
    }
    fn segment_usage(&self, _: &Path) -> Option<SegmentUsage> {
        None
    }
}

fn application(
    runtime: &Runtime,
    registry: Arc<Registry>,
    samples: Vec<i16>,
    finalize: bool,
) -> Application {
    application_with_failure(runtime, registry, samples, finalize, None)
}

fn application_with_failure(
    runtime: &Runtime,
    registry: Arc<Registry>,
    samples: Vec<i16>,
    finalize: bool,
    retry_failure: Option<&str>,
) -> Application {
    Application::with_recognition_sessions(
        Box::new(Sessions {
            registry,
            samples,
            finalize,
            retry_failure: retry_failure.map(str::to_owned),
        }),
        runtime.app(),
        Ok(()),
    )
}

fn wait_for(app: &mut Application, what: &str, matches: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll();
        let snapshot = app.snapshot();
        if matches(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The controlled playback audio. Only its identity matters: nothing decodes it.
fn samples() -> Vec<i16> {
    (0..3200i32)
        .map(|index| ((index * 37) % 3001 - 1500) as i16)
        .collect()
}

fn sample_bytes(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

/// Write the previous process's durable capture directory with the same writer
/// the cloud device uses, so the clean-stop marker and the byte count are the
/// production format.
fn clean_capture_directory(runtime: &Runtime, name: &str, samples: &[i16]) -> PathBuf {
    let directory = runtime.app().join(name);
    fs::create_dir_all(&directory).unwrap();
    let (mut writer, _stream) = PcmSpoolWriter::new(&directory, 2_000_000).unwrap();
    for sample in samples {
        writer.push(*sample).unwrap();
    }
    writer
        .finish("previous process controlled playback".into())
        .unwrap();
    directory
}

/// Write the segment index exactly as one interrupted process left it. A crash is
/// a process that stopped between two durable writes, so the test states that
/// moment directly instead of racing a real interruption.
fn write_index(runtime: &Runtime, next_sequence: u64, records: &[serde_json::Value]) {
    let index = serde_json::json!({
        "version": 1,
        "next_sequence": next_sequence,
        "segments": records,
    });
    fs::write(runtime.app().join("segments.json"), index.to_string()).unwrap();
}

fn index_record(
    sequence: u64,
    directory: &str,
    state: &str,
    detail: &str,
    text_file: Option<&str>,
    delivery: Option<&str>,
    terms: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "sequence": sequence,
        "directory": directory,
        "state": state,
        "detail": detail,
        "text_file": text_file,
        "text_chars": text_file.map(|_| 1).unwrap_or(0),
        "delivery": delivery,
        "usage": null,
        "glossary_terms": terms,
        "updated_utc": 1.0,
    })
}

fn stored_index(runtime: &Runtime) -> serde_json::Value {
    serde_json::from_slice(&fs::read(runtime.app().join("segments.json")).unwrap()).unwrap()
}

/// The raw durable state of an abnormal exit: a continuous PCM recording written
/// without the writer's end marker, optionally larger than the bytes the previous
/// process had committed.
fn crashed_capture_directory(
    runtime: &Runtime,
    name: &str,
    pcm: &[u8],
    committed_bytes: Option<u64>,
    clean_stop: bool,
) -> PathBuf {
    let directory = runtime.app().join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("playback.pcm"), pcm).unwrap();
    fs::write(
        directory.join("pcm-format.txt"),
        "sample_rate=16000\nchannels=1\nbits=16\nencoding=signed-little-endian\n",
    )
    .unwrap();
    if let Some(committed_bytes) = committed_bytes {
        fs::write(
            directory.join("playback.state"),
            format!(
                "committed_bytes={committed_bytes}\nclean_stop={}\n",
                u8::from(clean_stop)
            ),
        )
        .unwrap();
    }
    directory
}

fn write_delivery(runtime: &Runtime, records: &[serde_json::Value]) {
    let stored = serde_json::json!({ "version": 1, "segments": records });
    fs::write(runtime.app().join("delivery.json"), stored.to_string()).unwrap();
}

fn delivery_record(sequence: u64, state: &str) -> serde_json::Value {
    serde_json::json!({
        "sequence": sequence,
        "state": state,
        "baseline": "用户草稿",
        "baseline_problem": null,
        "before": if state == "appended" { serde_json::json!("用户草稿") } else { serde_json::Value::Null },
        "after": if state == "appended" { serde_json::json!("用户草稿\n结果") } else { serde_json::Value::Null },
        "detail": "a write may or may not have happened",
        "updated_utc": 1.0,
    })
}

#[test]
fn a_started_or_uncertain_delivery_is_never_replayed_after_a_restart() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("原目标里的文字");
    // Two saved results whose target write is not provable: one write started and
    // one is recorded as uncertain. Their audio is already gone, so the complete
    // text is the only recovery basis.
    fs::write(
        runtime.app().join("segment-00000001.txt"),
        "写入已开始的结果。",
    )
    .unwrap();
    fs::write(
        runtime.app().join("segment-00000002.txt"),
        "写入不确定的结果。",
    )
    .unwrap();
    write_index(
        &runtime,
        3,
        &[
            index_record(
                1,
                "session-35-1",
                "result",
                "delivery started",
                Some("segment-00000001.txt"),
                None,
                &[],
            ),
            index_record(
                2,
                "session-35-2",
                "result",
                "delivery uncertain",
                Some("segment-00000002.txt"),
                None,
                &[],
            ),
        ],
    );
    write_delivery(
        &runtime,
        &[
            delivery_record(1, "started"),
            delivery_record(2, "uncertain"),
        ],
    );
    let delivery_before = fs::read(runtime.app().join("delivery.json")).unwrap();

    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    let recovered = wait_for(&mut app, "both saved results", |snapshot| {
        snapshot.segments.len() == 2
    });
    assert_eq!(
        recovered
            .segments
            .iter()
            .map(|segment| segment.state)
            .collect::<Vec<_>>(),
        vec![SegmentState::Result, SegmentState::Result]
    );
    assert_eq!(
        recovered
            .segments
            .iter()
            .map(|segment| segment.delivery)
            .collect::<Vec<_>>(),
        vec![Some(Delivery::Uncertain), Some(Delivery::Uncertain)],
        "an unprovable write is reported as uncertain, never as a panel-only fallback"
    );
    assert_eq!(recovered.delivery, Some(Delivery::Uncertain));
    assert_eq!(recovered.text, "写入不确定的结果。");
    assert!(
        recovered.segments[0]
            .detail
            .contains("not written again automatically"),
        "{:?}",
        recovered.segments[0]
    );
    // The target is never written again, and the durable state is not downgraded:
    // "started" and "uncertain" stay terminal for automatic appends.
    assert_eq!(editor.draft(), "原目标里的文字");
    assert_eq!(
        fs::read(runtime.app().join("delivery.json")).unwrap(),
        delivery_before,
        "recovery never rewrites a started or uncertain delivery record"
    );
    assert!(registry.retries().is_empty());
    assert_eq!(registry.count(), 0);
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000001.txt")).unwrap(),
        "写入已开始的结果。"
    );
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000002.txt")).unwrap(),
        "写入不确定的结果。"
    );
}

#[test]
fn corrupt_state_does_not_hide_saved_results_or_lose_an_unindexed_recording() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户草稿");
    let audio = samples();
    // A corrupt segment index, a complete text file the index never described, and
    // a recording whose index entry never landed. Both are real previous-process
    // artifacts; nothing may be deleted and nothing may be recognized twice.
    fs::write(runtime.app().join("segments.json"), "{not a valid index").unwrap();
    fs::write(
        runtime.app().join("segment-00000003.txt"),
        "索引损坏后仍保留的完整结果。",
    )
    .unwrap();
    let orphan = clean_capture_directory(&runtime, "session-35-orphan", &audio);
    let glossary = runtime.glossary("WASAPI\n");

    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())))
        .with_glossary(glossary);
    let recovered = wait_for(&mut app, "the recovered state", |snapshot| {
        snapshot.segments.len() == 2
            && snapshot
                .segments
                .iter()
                .any(|segment| segment.sequence == 4 && segment.state == SegmentState::Processing)
    });
    // The state is reported degraded, but the saved result is not hidden by it.
    assert!(
        recovered.detail.contains("segments.json")
            || recovered.detail.contains("multi-segment record")
            || recovered.detail.contains("segment index"),
        "{recovered:?}"
    );
    let pending = recovered
        .segments
        .iter()
        .find(|segment| segment.sequence == 3)
        .expect("the orphan complete text keeps its sequence");
    assert_eq!(pending.state, SegmentState::Result);
    assert_eq!(pending.text, "索引损坏后仍保留的完整结果。");
    assert_eq!(
        pending.delivery,
        Some(Delivery::Uncertain),
        "without an index entry the delivery state is unknown"
    );
    // The recording the index never described is adopted and processed again.
    let adopted = recovered
        .segments
        .iter()
        .find(|segment| segment.sequence == 4)
        .expect("the unindexed recording gets its own capture order");
    assert_eq!(adopted.state, SegmentState::Processing);
    let retries = registry.retries();
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert_eq!(retries[0].directory, orphan);
    assert_eq!(retries[0].audio_bytes, sample_bytes(&audio));
    assert_eq!(
        retries[0].terms,
        vec!["WASAPI".to_string()],
        "a lost capture-time snapshot falls back to the current glossary and says so"
    );
    assert_eq!(editor.draft(), "用户草稿");

    registry.finish(0, "索引损坏后补出的结果。", true);
    let settled = wait_for(&mut app, "the adopted recording's result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 4 && segment.state == SegmentState::Result)
    });
    assert_eq!(
        settled
            .segments
            .iter()
            .find(|segment| segment.sequence == 4)
            .unwrap()
            .delivery,
        Some(Delivery::Fallback)
    );
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000004.txt")).unwrap(),
        "索引损坏后补出的结果。"
    );
    assert!(
        runtime.app().join("segment-00000003.txt").exists(),
        "the only remaining evidence of the earlier result is never deleted"
    );
    // A new capture never reuses a recovered or saved sequence.
    app.dispatch(Control::Start);
    let capturing = app.snapshot();
    assert_eq!(
        capturing.segments.last().map(|segment| segment.sequence),
        Some(5),
        "{capturing:?}"
    );
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000003.txt")).unwrap(),
        "索引损坏后仍保留的完整结果。"
    );
    assert_eq!(editor.draft(), "用户草稿");
}

#[test]
fn a_result_saved_before_the_audio_was_cleaned_is_kept_pending_and_never_re_recognized() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("旧草稿");
    let audio = samples();
    // The exact durable state of a process that saved the complete result and then
    // stopped before its temporary audio was cleaned and before the delivery was
    // applied. The recording is complete (normal stop), so this is not an
    // incomplete-capture case: the text is the only thing that still matters.
    let directory = clean_capture_directory(&runtime, "session-35-1", &audio);
    fs::write(
        runtime.app().join("segment-00000001.txt"),
        "保存后崩溃的完整结果。",
    )
    .unwrap();
    write_index(
        &runtime,
        2,
        &[index_record(
            1,
            "session-35-1",
            "processing",
            "The capture stopped and its whole result is still being confirmed.",
            Some("segment-00000001.txt"),
            None,
            &["WASAPI"],
        )],
    );

    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    let recovered = app.snapshot();
    assert_eq!(recovered.segments.len(), 1, "{recovered:?}");
    assert_eq!(recovered.segments[0].sequence, 1);
    assert_eq!(recovered.segments[0].state, SegmentState::Result);
    assert_eq!(recovered.segments[0].text, "保存后崩溃的完整结果。");
    assert_eq!(
        recovered.segments[0].delivery,
        Some(Delivery::Fallback),
        "a recovered result is a panel result"
    );
    assert_eq!(recovered.text, "保存后崩溃的完整结果。");
    assert_eq!(recovered.phase, Phase::Result, "{recovered:?}");
    assert!(
        registry.retries().is_empty(),
        "an already-complete result is never recognized again: {:?}",
        registry.retries()
    );
    assert_eq!(registry.count(), 0, "no recognition session was created");
    assert_eq!(editor.draft(), "旧草稿");
    // The crash window is closed: the complete text is durable, so the production
    // audio is released and is not kept as a permanent archive.
    assert!(!directory.exists(), "the temporary audio is released");
    assert!(runtime.session_directories().is_empty());
    // The record now says what recovery decided, so the next launch repeats no
    // work and the pending result keeps its own delivery state.
    let index = stored_index(&runtime);
    assert_eq!(index["segments"][0]["state"], "result", "{index}");
    assert_eq!(index["segments"][0]["delivery"], "fallback", "{index}");
    assert_eq!(
        index["segments"][0]["text_file"], "segment-00000001.txt",
        "{index}"
    );

    // A later restart still shows it and still deletes nothing: a pending result
    // does not expire by date and its text is never removed on its own.
    drop(app);
    let mut again = application(&runtime, registry.clone(), Vec::new(), true);
    let again = wait_for(&mut again, "the retained pending result", |snapshot| {
        !snapshot.segments.is_empty()
    });
    assert_eq!(again.segments[0].sequence, 1);
    assert_eq!(again.segments[0].text, "保存后崩溃的完整结果。");
    assert_eq!(again.segments[0].delivery, Some(Delivery::Fallback));
    assert!(again.segments[0].detail.contains("Recovered"));
    assert!(
        registry.retries().is_empty(),
        "a pending result is still never recognized again"
    );
    assert!(
        runtime.app().join("segment-00000001.txt").exists(),
        "the complete text is the only remaining evidence and is never deleted"
    );
}

#[test]
fn a_stopped_capture_is_resumed_after_a_restart_and_its_result_waits_in_the_panel() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户已经写好的草稿");
    let audio = samples();

    // ---- first process: one segment, stopped, quit before the cloud answers
    {
        let glossary = runtime.glossary("DeepSeek Harness | 迪普西克\nWASAPI\n");
        let mut app = application(&runtime, registry.clone(), audio.clone(), true)
            .with_targets(Arc::new(Selection(editor.clone())))
            .with_glossary(glossary);
        app.dispatch(Control::Start);
        let capturing = app.snapshot();
        assert_eq!(capturing.phase, Phase::Capturing, "{capturing:?}");
        app.dispatch(Control::Stop);
        let stopped = wait_for(&mut app, "the stopped capture", |snapshot| {
            snapshot.phase == Phase::Processing
        });
        assert_eq!(stopped.segments.len(), 1, "{stopped:?}");
        assert_eq!(stopped.segments[0].sequence, 1);
        assert_eq!(stopped.segments[0].state, SegmentState::Processing);
        assert!(stopped.text.is_empty());
        assert_eq!(editor.draft(), "用户已经写好的草稿");
        // The user does not wait for the cloud: the first process ends now.
    }

    // ---- second process: only the durable state is inherited. The glossary file
    // is gone, so the capture-time term snapshot is the only source of the terms.
    fs::remove_file(runtime.path().join("glossary.txt")).unwrap();
    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    // The first poll is the GUI's own first tick: the recovered segments are
    // already listed, and the oldest recording starts then.
    app.poll();
    let recovered = app.snapshot();
    assert_eq!(recovered.segments.len(), 1, "{recovered:?}");
    let segment = &recovered.segments[0];
    assert_eq!(segment.sequence, 1, "the capture order is kept");
    assert_eq!(segment.state, SegmentState::Processing);
    assert!(segment.text.is_empty());
    assert!(
        recovered.detail.contains("Recovered") && recovered.detail.contains("restart"),
        "{recovered:?}"
    );
    assert!(
        !segment.detail.to_lowercase().contains("incomplete"),
        "a capture that was stopped normally is not reported as incomplete: {segment:?}"
    );
    // The recording was discovered and resumed automatically, once, from the
    // retained audio of the same segment.
    let retries = registry.retries();
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert_eq!(retries[0].directory, registry.directory(0));
    assert_eq!(retries[0].audio_bytes, sample_bytes(&audio));
    assert!(
        retries[0].audio.starts_with(&retries[0].directory),
        "the recovered recording is the retained audio of the same capture directory"
    );
    // The capture-time term snapshot survives even though the file is gone.
    assert!(!runtime.path().join("glossary.txt").exists());
    assert_eq!(
        retries[0].terms,
        vec!["DeepSeek Harness".to_string(), "WASAPI".to_string()]
    );

    registry.finish(1, "重启后恢复的完整结果。", true);
    let settled = wait_for(&mut app, "the recovered result", |snapshot| {
        snapshot
            .segments
            .first()
            .is_some_and(|segment| segment.state == SegmentState::Result)
    });
    assert_eq!(settled.text, "重启后恢复的完整结果。");
    assert_eq!(settled.delivery, Some(Delivery::Fallback));
    assert_eq!(settled.segments[0].delivery, Some(Delivery::Fallback));
    assert_eq!(
        settled.segments[0].text, "重启后恢复的完整结果。",
        "the result is in the panel"
    );
    assert_eq!(
        editor.draft(),
        "用户已经写好的草稿",
        "a recovered result is never written to an old target"
    );
    // Saved durably, then the audio is no longer needed.
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000001.txt")).unwrap(),
        "重启后恢复的完整结果。"
    );
    assert!(
        runtime.session_directories().is_empty(),
        "the audio is released only after the complete text is saved: {:?}",
        runtime.session_directories()
    );

    // ---- third process: the pending result is still there and is never
    // recognized again.
    let mut third = application(&runtime, registry.clone(), Vec::new(), true);
    let again = wait_for(&mut third, "the retained pending result", |snapshot| {
        !snapshot.segments.is_empty()
    });
    assert_eq!(again.segments.len(), 1, "{again:?}");
    assert_eq!(again.segments[0].sequence, 1);
    assert_eq!(again.segments[0].text, "重启后恢复的完整结果。");
    assert_eq!(again.segments[0].delivery, Some(Delivery::Fallback));
    assert_eq!(again.text, "重启后恢复的完整结果。");
    assert_eq!(
        registry.retries().len(),
        1,
        "a complete result is never recognized again after a restart"
    );
    assert_eq!(registry.count(), 2, "no new capture or retry was started");
    assert!(
        third.snapshot().detail.contains("Recovered"),
        "{:?}",
        third.snapshot()
    );
}

#[test]
fn several_recovered_segments_keep_their_identity_and_order_and_a_new_capture_wins() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户草稿");
    let audio = samples();

    // ---- first process: two independent segments, both waiting for the cloud
    {
        let glossary = runtime.glossary("WASAPI\n");
        let mut app = application(&runtime, registry.clone(), audio.clone(), true)
            .with_targets(Arc::new(Selection(editor.clone())))
            .with_glossary(glossary);
        for _ in 0..2 {
            app.dispatch(Control::Start);
            app.dispatch(Control::Stop);
        }
        let stopped = wait_for(&mut app, "two stopped captures", |snapshot| {
            snapshot.segments.len() == 2
                && snapshot
                    .segments
                    .iter()
                    .all(|segment| segment.state == SegmentState::Processing)
        });
        assert_eq!(
            stopped
                .segments
                .iter()
                .map(|segment| segment.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(editor.draft(), "用户草稿");
    }

    // ---- second process: both segments come back, in capture order
    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    // The first poll starts the oldest recovered recording, exactly like the GUI's
    // first tick; the recovered list itself is already visible.
    app.poll();
    let recovered = app.snapshot();
    assert_eq!(recovered.segments.len(), 2, "{recovered:?}");
    assert_eq!(
        recovered
            .segments
            .iter()
            .map(|segment| segment.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "capture order and identity survive the restart"
    );
    assert!(
        recovered
            .segments
            .iter()
            .all(|segment| segment.state == SegmentState::Processing && segment.text.is_empty()),
        "{recovered:?}"
    );
    // Only the oldest recovered recording is processed, so two old segments never
    // compete with each other or with the current work.
    assert_eq!(registry.retries().len(), 1, "{:?}", registry.retries());
    assert_eq!(registry.retries()[0].directory, registry.directory(0));
    assert_eq!(registry.retries()[0].audio_bytes, sample_bytes(&audio));

    // A new capture still wins: it starts immediately while the old segment is
    // being recovered in the background, and its result is delivered normally.
    app.dispatch(Control::Start);
    assert_eq!(
        app.snapshot().phase,
        Phase::Capturing,
        "{:?}",
        app.snapshot()
    );
    app.dispatch(Control::Stop);
    registry.finish(3, "新采集的结果。", false);
    let fresh = wait_for(&mut app, "the new capture's result", |snapshot| {
        snapshot.text == "新采集的结果。"
    });
    assert_eq!(fresh.delivery, Some(Delivery::Appended), "{fresh:?}");
    assert_eq!(
        editor.draft(),
        "用户草稿\n新采集的结果。",
        "the new capture is delivered to the target it bound at its own start"
    );
    assert_eq!(
        registry.retries().len(),
        1,
        "the older recovered segment still holds the background capacity first"
    );

    // The first recovered result arrives: it is panel-only and does not take the
    // new capture's place as the current result.
    registry.finish(2, "第一段恢复结果。", true);
    let first_done = wait_for(&mut app, "the first recovered result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.state == SegmentState::Result)
    });
    let first = first_done
        .segments
        .iter()
        .find(|segment| segment.sequence == 1)
        .unwrap();
    assert_eq!(first.text, "第一段恢复结果。");
    assert_eq!(first.delivery, Some(Delivery::Fallback));
    assert_eq!(
        first_done.text, "新采集的结果。",
        "an older recovered result never replaces the newer current result"
    );
    assert_eq!(editor.draft(), "用户草稿\n新采集的结果。");

    // Now the second old segment is processed, and its result stays pending too.
    registry.finish(4, "第二段恢复结果。", true);
    let second_done = wait_for(&mut app, "the second recovered result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 2 && segment.state == SegmentState::Result)
    });
    let retries = registry.retries();
    assert_eq!(retries.len(), 2, "{retries:?}");
    assert_eq!(retries[1].directory, registry.directory(1));
    assert_eq!(retries[1].audio_bytes, sample_bytes(&audio));
    let second = second_done
        .segments
        .iter()
        .find(|segment| segment.sequence == 2)
        .unwrap();
    assert_eq!(second.text, "第二段恢复结果。");
    assert_eq!(second.delivery, Some(Delivery::Fallback));
    assert_eq!(second_done.text, "新采集的结果。");
    assert_eq!(
        editor.draft(),
        "用户草稿\n新采集的结果。",
        "recovered results are never written to a target after a restart"
    );
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000001.txt")).unwrap(),
        "第一段恢复结果。"
    );
    assert_eq!(
        fs::read_to_string(runtime.app().join("segment-00000002.txt")).unwrap(),
        "第二段恢复结果。"
    );
    assert!(
        runtime.session_directories().is_empty(),
        "every recovered result released its audio after the complete text was saved"
    );
}

#[test]
fn partial_audio_from_an_abnormal_exit_is_repaired_and_clearly_marked_incomplete() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户草稿");
    let audio = samples();
    let full = sample_bytes(&audio);

    // Segment 1: the process was killed mid-capture. There is no end marker and the
    // file ends in the middle of a 16-bit sample, so the last byte is not audio.
    let mut torn = full.clone();
    torn.push(0x7f);
    let first = crashed_capture_directory(&runtime, "session-35-crash-1", &torn, None, false);
    // Segment 2: the end marker only committed half the bytes that reached the file
    // (the writer was torn down between the sync and the marker).
    let second = crashed_capture_directory(
        &runtime,
        "session-35-crash-2",
        &full,
        Some((full.len() / 2) as u64),
        false,
    );
    write_index(
        &runtime,
        3,
        &[
            index_record(
                1,
                "session-35-crash-1",
                "capturing",
                "capture",
                None,
                None,
                &[],
            ),
            index_record(
                2,
                "session-35-crash-2",
                "capturing",
                "capture",
                None,
                None,
                &[],
            ),
        ],
    );

    let mut app = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    let recovered = wait_for(&mut app, "the first partial recording", |snapshot| {
        snapshot.segments.len() == 2
    });
    for segment in &recovered.segments {
        assert!(
            segment.detail.to_lowercase().contains("incomplete")
                && segment
                    .detail
                    .contains("neither a complete recording nor a passing transcript"),
            "an unprovable recording is marked clearly: {segment:?}"
        );
    }

    // Only the readable whole samples of the first recording are processed, from a
    // repair copy inside the same segment directory.
    let retries = registry.retries();
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert_eq!(retries[0].directory, first);
    assert_eq!(
        retries[0].audio_bytes, full,
        "the torn trailing byte is not presented as audio"
    );
    assert_ne!(
        retries[0].audio,
        first.join("playback.pcm"),
        "a repaired copy is processed, not the torn original"
    );
    assert_eq!(
        fs::metadata(first.join("playback.pcm")).unwrap().len(),
        (full.len() + 1) as u64,
        "the original evidence is kept while it is still the only recovery basis"
    );

    registry.finish(0, "第一段残缺录音的可读部分。", true);
    let first_done = wait_for(&mut app, "the first incomplete result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 1 && segment.state == SegmentState::Result)
    });
    assert_eq!(
        first_done
            .segments
            .iter()
            .find(|segment| segment.sequence == 1)
            .unwrap()
            .text,
        "第一段残缺录音的可读部分。"
    );
    assert!(
        !first.exists(),
        "the audio is released after the text is saved"
    );

    // The second recording keeps only what the marker says was committed.
    let second_retry = wait_for(&mut app, "the second partial recording", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 2 && segment.state == SegmentState::Processing)
    });
    assert!(
        second_retry.segments[1].detail.contains("committed bytes"),
        "{second_retry:?}"
    );
    let retries = registry.retries();
    assert_eq!(retries.len(), 2, "{retries:?}");
    assert_eq!(retries[1].directory, second);
    assert_eq!(
        retries[1].audio_bytes,
        full[..full.len() / 2].to_vec(),
        "only the bytes the marker committed are processed"
    );
    registry.finish(1, "第二段残缺录音的可读部分。", true);
    let done = wait_for(&mut app, "the second incomplete result", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.sequence == 2 && segment.state == SegmentState::Result)
    });
    assert_eq!(
        done.segments
            .iter()
            .find(|segment| segment.sequence == 2)
            .unwrap()
            .text,
        "第二段残缺录音的可读部分。"
    );
    assert!(!second.exists());
    assert_eq!(editor.draft(), "用户草稿");
    assert_eq!(
        done.segments
            .iter()
            .map(|segment| segment.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "recovery keeps its capture order"
    );
}

#[test]
fn a_failed_automatic_resume_keeps_the_audio_and_never_blocks_a_new_capture() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户草稿");
    let audio = samples();
    let directory = clean_capture_directory(&runtime, "session-35-1", &audio);
    write_index(
        &runtime,
        2,
        &[index_record(
            1,
            "session-35-1",
            "processing",
            "stopped",
            None,
            None,
            &[],
        )],
    );

    // The adapter refuses the recovered recording (for example the account is not
    // usable right now). Nothing may be deleted and nothing may be re-recorded.
    let mut app = application_with_failure(
        &runtime,
        registry.clone(),
        Vec::new(),
        true,
        Some("the configured account is not available"),
    )
    .with_targets(Arc::new(Selection(editor.clone())));
    let failed = wait_for(&mut app, "the failed resume", |snapshot| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.state == SegmentState::Failed)
    });
    assert_eq!(failed.phase, Phase::Failure, "{failed:?}");
    let segment = &failed.segments[0];
    assert!(
        segment
            .detail
            .contains("could not be processed again automatically")
            && segment.detail.contains("not available"),
        "{segment:?}"
    );
    assert!(
        segment.detail.contains("kept on this machine"),
        "the recording is explicitly kept: {segment:?}"
    );
    assert!(
        directory.join("playback.pcm").is_file(),
        "the only recovery basis is never deleted when the resume fails"
    );
    assert!(
        failed
            .retained_audio
            .iter()
            .any(|path| path.ends_with("playback.pcm")),
        "{failed:?}"
    );
    // A failed old segment never blocks the next capture.
    app.dispatch(Control::Start);
    assert_eq!(
        app.snapshot().phase,
        Phase::Capturing,
        "{:?}",
        app.snapshot()
    );
    assert!(directory.join("playback.pcm").is_file());
    app.dispatch(Control::Stop);
    registry.finish_newest("新采集仍然可用。", false);
    let fresh = wait_for(&mut app, "the new capture", |snapshot| {
        snapshot.text == "新采集仍然可用。"
    });
    assert_eq!(fresh.delivery, Some(Delivery::Appended), "{fresh:?}");

    // The next launch does not spend another automatic request on the failure: the
    // segment stays failed with its audio, and the user decides what happens next.
    drop(app);
    let mut again = application(&runtime, registry.clone(), Vec::new(), true);
    let retained = wait_for(&mut again, "the retained failed recording", |snapshot| {
        !snapshot.segments.is_empty() && snapshot.phase == Phase::Failure
    });
    assert_eq!(retained.segments[0].state, SegmentState::Failed);
    assert_eq!(
        registry.retries().len(),
        1,
        "a failed automatic resume is never retried without the user asking"
    );
    assert!(directory.join("playback.pcm").is_file());
    // An explicit delete is still the way to end it.
    again.dispatch(Control::DeleteSegment(1));
    let deleted = again.snapshot();
    assert!(
        !directory.exists(),
        "the explicit per-segment delete removes the kept audio: {deleted:?}"
    );
    assert!(
        deleted
            .detail
            .contains("Segment 1 was deleted at your request"),
        "{deleted:?}"
    );
}

#[test]
fn quitting_does_not_wait_for_the_cloud_and_the_next_launch_recovers_the_capture() {
    let runtime = Runtime::new();
    let registry = Arc::new(Registry::default());
    let editor = Editor::new("用户草稿");
    let audio = samples();
    let glossary = runtime.glossary("WASAPI\n");
    let mut app = application(&runtime, registry.clone(), audio.clone(), true)
        .with_targets(Arc::new(Selection(editor.clone())))
        .with_glossary(glossary);
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let stopped = wait_for(&mut app, "the stopped capture", |snapshot| {
        snapshot.phase == Phase::Processing
    });
    assert_eq!(stopped.segments[0].state, SegmentState::Processing);

    // The cloud never answers. Quitting waits only for the received audio to be
    // flushed, never for the network request.
    let started = Instant::now();
    let note = app.prepare_exit(Duration::from_millis(300));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "quitting must not wait for the pending cloud request"
    );
    assert!(
        note.to_lowercase().contains("next launch") || note.to_lowercase().contains("recover"),
        "quitting says what happens to the unfinished work: {note}"
    );
    let quit = app.snapshot();
    assert_eq!(
        quit.segments[0].state,
        SegmentState::Processing,
        "the unfinished segment stays an unfinished segment, not a failure"
    );
    let retained = runtime.session_directories();
    assert_eq!(retained.len(), 1, "{retained:?}");
    assert!(
        retained[0].join("playback.state").is_file(),
        "the received audio was closed durably before the process went away"
    );
    assert_eq!(editor.draft(), "用户草稿");
    drop(app);

    // The next launch recovers exactly what was left and continues automatically.
    let mut again = application(&runtime, registry.clone(), Vec::new(), true)
        .with_targets(Arc::new(Selection(editor.clone())));
    let recovered = again.snapshot();
    assert_eq!(recovered.segments.len(), 1, "{recovered:?}");
    assert_eq!(recovered.segments[0].sequence, 1);
    assert_eq!(recovered.segments[0].state, SegmentState::Processing);
    let resumed = wait_for(&mut again, "the resumed recording", |_| {
        !registry.retries().is_empty()
    });
    assert_eq!(registry.retries().len(), 1, "{resumed:?}");
    assert_eq!(registry.retries()[0].audio_bytes, sample_bytes(&audio));
    registry.finish_newest("退出后重新打开恢复了这一段。", true);
    let done = wait_for(&mut again, "the recovered result", |snapshot| {
        snapshot
            .segments
            .first()
            .is_some_and(|segment| segment.state == SegmentState::Result)
    });
    assert_eq!(done.text, "退出后重新打开恢复了这一段。");
    assert_eq!(done.delivery, Some(Delivery::Fallback));
    assert_eq!(editor.draft(), "用户草稿");
}

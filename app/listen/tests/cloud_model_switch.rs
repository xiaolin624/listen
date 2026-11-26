//! #56: the cloud model is an explicit, closed switch.
//!
//! These tests drive the real launch configuration, the real settings guard and
//! the live free-only verification seam. They never read a real credential, open
//! a playback device or connect; the only substitute is the CLI status source.

#[cfg(windows)]
use listen::cloud::{FreeOnlyStatusSource, NativeMessageTransport};
use listen::{
    AppResult, Application, PlaybackAudio, Preparation, Recording,
    cloud::{
        ALI_MODEL, ALI_MODEL_STREAMING, ALI_REGION, AliSettings, CloudError, CloudSessions,
        CredentialSource, FreeOnlyPolicy, MessageSocket, MessageTransport, Secret, TrialCostPolicy,
        TrialLedger,
    },
    launch::{Config, LaunchAction},
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// `Config::parse` resolves paths against this root but never touches it.
fn root() -> PathBuf {
    std::env::temp_dir().join("listen-cloud-model-switch")
}

/// A unique runtime directory; the cloud session ledger is the only writer.
struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-cloud-model-switch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Selection must be decided before any device is opened.
struct NoCapture;
impl PlaybackAudio for NoCapture {
    fn start(&self, _directory: &Path) -> AppResult<Box<dyn Recording>> {
        Err("the model switch must not open the playback device".into())
    }
}
struct Key;
impl CredentialSource for Key {
    fn load(&self) -> Result<Secret, CloudError> {
        Secret::new("CLOUD-MODEL-SWITCH-KEY".into())
    }
}
/// The network boundary; preparation must never reach it.
struct NeverConnect;
impl MessageTransport for NeverConnect {
    fn connect(
        &self,
        _settings: &AliSettings,
        _key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        panic!("preparation must not connect")
    }
}
/// A checked policy for the settings it is asked about. The live CLI check has
/// its own offline seam; this only keeps preparation Ready for the backend text.
struct Allow;
impl MessageTransport for Allow {
    fn cost_policy(
        &self,
        settings: &AliSettings,
        _key: &Secret,
    ) -> Result<TrialCostPolicy, CloudError> {
        Ok(TrialCostPolicy::FreeOnly(FreeOnlyPolicy {
            model: settings.model.clone(),
            region: settings.region.clone(),
            evidence: "offline model-switch fixture".into(),
            verified_on: "offline model-switch fixture".into(),
        }))
    }
    fn connect(
        &self,
        _settings: &AliSettings,
        _key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        panic!("preparation must not connect")
    }
}

fn parse(arguments: &[&str]) -> Result<Config, String> {
    let root = root();
    let mut arguments: Vec<String> = arguments
        .iter()
        .map(|argument| (*argument).to_string())
        .collect();
    arguments.push("--runtime".into());
    arguments.push(root.display().to_string());
    match Config::parse(&root, arguments) {
        Ok(LaunchAction::Run(config)) => Ok(config),
        Ok(other) => panic!("expected one run configuration: {other:?}"),
        Err(message) => Err(message),
    }
}

#[test]
fn the_launch_flag_selects_a_whitelisted_model_and_refuses_an_unlisted_one() {
    // The flag is absent by default, which must mean the shipped message model is
    // used exactly as before.
    assert_eq!(parse(&[]).unwrap().ali_model(), None);
    assert_eq!(
        parse(&["--cloud-trial", "ali"]).unwrap().ali_model(),
        None,
        "an explicit --cloud-trial must keep the default model"
    );
    // Both whitelisted models are selectable, in either spelling position.
    assert_eq!(
        parse(&["--ali-model", ALI_MODEL_STREAMING])
            .unwrap()
            .ali_model(),
        Some(ALI_MODEL_STREAMING)
    );
    assert_eq!(
        parse(&["--cloud-trial", "ali", "--ali-model", ALI_MODEL])
            .unwrap()
            .ali_model(),
        Some(ALI_MODEL)
    );

    // Anything outside the whitelist is refused with both allowed ids, and the
    // refusal must say that it will not quietly use the default instead.
    let refused = parse(&["--ali-model", "whisper-large"]).unwrap_err();
    for expected in [ALI_MODEL, ALI_MODEL_STREAMING, "--ali-model"] {
        assert!(
            refused.contains(expected),
            "the refusal must name {expected}: {refused}"
        );
    }
    assert!(
        refused.contains("fall back"),
        "the refusal must not quietly fall back: {refused}"
    );
    assert!(
        !refused.contains("whisper-large"),
        "the refusal must not echo an arbitrary model id: {refused}"
    );

    // The local fallback has no cloud model, so asking for both is a configuration
    // error instead of one of the two being ignored.
    let mixed = parse(&["--engine", "sensevoice", "--ali-model", ALI_MODEL_STREAMING]).unwrap_err();
    assert!(mixed.contains("--ali-model"), "{mixed}");
    assert!(mixed.contains("--engine"), "{mixed}");
}

#[test]
fn the_settings_guard_reports_a_whitelisted_model_and_still_rejects_a_stranger() {
    // No explicit selection: the shipped model is named exactly as before.
    let default = AliSettings::beijing("switch-workspace".into());
    assert!(default.description().contains(ALI_MODEL));

    let streaming =
        AliSettings::beijing("switch-workspace".into()).with_model(Some(ALI_MODEL_STREAMING));
    let description = streaming.description();
    assert!(
        description.contains(ALI_MODEL_STREAMING),
        "the guard must name the selected whitelisted model: {description}"
    );
    assert!(
        !description.contains("unsupported model"),
        "a whitelisted model is supported: {description}"
    );

    let stranger =
        AliSettings::beijing("switch-workspace".into()).with_model(Some("some-other-model"));
    let description = stranger.description();
    assert!(
        description.contains("unsupported model"),
        "a model outside the whitelist stays unsupported: {description}"
    );
    assert!(
        !description.contains("some-other-model"),
        "the guard must not echo an arbitrary id: {description}"
    );

    // The absent form must not touch whatever the stored settings decided, so a
    // shipped/legacy model is not replaced just because the flag is missing.
    let stored = AliSettings {
        workspace_id: "switch-workspace".into(),
        model: ALI_MODEL_STREAMING.into(),
        region: ALI_REGION.into(),
    };
    assert_eq!(stored.clone().with_model(None).model, ALI_MODEL_STREAMING);
    assert_eq!(stored.with_model(Some(ALI_MODEL)).model, ALI_MODEL);

    // What `status.txt` writes as `backend=` is this description verbatim.
    let runtime = Runtime::new();
    let sessions = CloudSessions::new(
        Arc::new(NoCapture),
        Arc::new(Allow),
        Arc::new(Key),
        AliSettings::beijing("switch-workspace".into()).with_model(Some(ALI_MODEL_STREAMING)),
        TrialLedger::new(runtime.0.join("ledger")),
    );
    let app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.join("app"), Ok(()));
    let snapshot = app.snapshot();
    assert_eq!(snapshot.preparation, Preparation::Ready, "{snapshot:?}");
    let backend = snapshot.backend.as_deref().unwrap_or_default();
    assert!(
        backend.contains(ALI_MODEL_STREAMING),
        "the reported backend must name the selected model: {snapshot:?}"
    );
}

/// The live CLI is replaced only at its output boundary, exactly like the existing
/// free-only fixtures. It records which model it was asked about, so the check
/// cannot quietly verify a different model than the one the task will use.
#[cfg(windows)]
struct QuotaStatus {
    asked: Arc<Mutex<Vec<String>>>,
    record_model: &'static str,
}
#[cfg(windows)]
impl FreeOnlyStatusSource for QuotaStatus {
    fn workspaces(&self) -> Result<Vec<u8>, CloudError> {
        Ok(json!([{"workspaceId":"switch-workspace","default":true}])
            .to_string()
            .into_bytes())
    }
    fn quota(&self, model: &str) -> Result<Vec<u8>, CloudError> {
        self.asked.lock().unwrap().push(model.to_owned());
        Ok(json!([{
            "model": self.record_model, "type": "Audio", "remaining": 1000000,
            "total": 1000000, "expires": "9999-12-31", "autoStop": true
        }])
        .to_string()
        .into_bytes())
    }
}

#[cfg(windows)]
#[test]
fn the_live_free_only_check_queries_and_requires_the_selected_model() {
    let key = Secret::new("CLOUD-MODEL-SWITCH-KEY".into()).unwrap();
    let streaming =
        AliSettings::beijing("switch-workspace".into()).with_model(Some(ALI_MODEL_STREAMING));

    // Streaming selected: the CLI must be asked about streaming, and streaming's
    // own record authorizes the task.
    let asked = Arc::new(Mutex::new(Vec::new()));
    let transport = NativeMessageTransport::with_status_source(Arc::new(QuotaStatus {
        asked: asked.clone(),
        record_model: ALI_MODEL_STREAMING,
    }));
    match transport.cost_policy(&streaming, &key) {
        Ok(TrialCostPolicy::FreeOnly(policy)) => {
            assert_eq!(policy.model, ALI_MODEL_STREAMING);
        }
        other => panic!("streaming's own free quota must authorize it: {other:?}"),
    }
    assert_eq!(*asked.lock().unwrap(), [ALI_MODEL_STREAMING.to_owned()]);

    // A record belonging to the other whitelisted model must not authorize this
    // model: the check stays exact instead of accepting any model's quota.
    let asked = Arc::new(Mutex::new(Vec::new()));
    let transport = NativeMessageTransport::with_status_source(Arc::new(QuotaStatus {
        asked: asked.clone(),
        record_model: ALI_MODEL,
    }));
    assert_eq!(
        transport.cost_policy(&streaming, &key).err(),
        Some(CloudError::ProtectionUnavailable)
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [ALI_MODEL_STREAMING.to_owned()],
        "the check must ask about the selected model, not the shipped one"
    );

    // Without a selection the shipped model is still the one queried and accepted.
    let asked = Arc::new(Mutex::new(Vec::new()));
    let transport = NativeMessageTransport::with_status_source(Arc::new(QuotaStatus {
        asked: asked.clone(),
        record_model: ALI_MODEL,
    }));
    assert!(
        transport
            .cost_policy(&AliSettings::beijing("switch-workspace".into()), &key)
            .is_ok()
    );
    assert_eq!(*asked.lock().unwrap(), [ALI_MODEL.to_owned()]);
}

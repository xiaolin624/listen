//! Packaging/config guarantees for the cloud path that #40 made the daily default.
//!
//! The paid-network and real-editor acceptance is recorded in
//! `docs/issue-40-acceptance-20261121.md`; these tests deliberately prove only the
//! offline configuration surface: the shipped settings file and credential
//! boundary reject invalid values before capture/connect, the runtime description
//! names the one selected vendor/model/region, and no key material reaches the
//! runtime files.

use listen::{
    AppResult, Application, Control, Phase, PlaybackAudio, Preparation, Recording,
    cloud::{
        ALI_MODEL, ALI_REGION, AliSettings, CloudError, CloudSessions, CredentialSource,
        MessageSocket, MessageTransport, Secret, TrialLedger,
    },
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const FAKE_KEY: &str = "DELIVERY-CONFIG-KEY-9F3A";

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "listen-delivery-config-{}-{unique}",
            std::process::id()
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

/// A playback boundary that records any attempt to open the device. The invalid
/// configurations below must be refused before this is reached.
struct NoCapture(Arc<AtomicBool>);
impl PlaybackAudio for NoCapture {
    fn start(&self, _directory: &Path) -> AppResult<Box<dyn Recording>> {
        self.0.store(true, Ordering::SeqCst);
        Err("capture must not start for an invalid cloud configuration".into())
    }
}

/// The network boundary itself. Reaching it with an invalid configuration is the
/// failure this test is written to catch.
struct NeverConnect;
impl MessageTransport for NeverConnect {
    fn connect(
        &self,
        _settings: &AliSettings,
        _key: &Secret,
    ) -> Result<Box<dyn MessageSocket>, CloudError> {
        panic!("an invalid cloud configuration must be refused before any connection")
    }
}

struct FixedKey(&'static str);
impl CredentialSource for FixedKey {
    fn load(&self) -> Result<Secret, CloudError> {
        Secret::new(self.0.to_owned())
    }
}

struct MissingKey;
impl CredentialSource for MissingKey {
    fn load(&self) -> Result<Secret, CloudError> {
        Err(CloudError::MissingKey)
    }
}

fn application(
    runtime: &Runtime,
    credentials: Arc<dyn CredentialSource>,
    settings: AliSettings,
    started: Arc<AtomicBool>,
) -> (Application, TrialLedger) {
    let ledger = TrialLedger::new(runtime.0.join("ledger"));
    let sessions = CloudSessions::new(
        Arc::new(NoCapture(started)),
        Arc::new(NeverConnect),
        credentials,
        settings,
        ledger.clone(),
    );
    let app =
        Application::with_recognition_sessions(Box::new(sessions), runtime.0.join("app"), Ok(()));
    (app, ledger)
}

fn files_mentioning(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return hits;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            hits.extend(files_mentioning(&path, needle));
        } else if let Ok(bytes) = fs::read(&path) {
            if bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
            {
                hits.push(path);
            }
        }
    }
    hits
}

#[test]
fn the_shipped_cloud_description_names_the_one_selected_vendor_model_and_region() {
    let description = AliSettings::beijing("package-verification-workspace".into()).description();
    for expected in ["Alibaba Model Studio", ALI_MODEL, ALI_REGION] {
        assert!(description.contains(expected), "{description}");
    }
    let lower = description.to_ascii_lowercase();
    for stale in ["two-provider", "doubao", "volcengine", "two vendors"] {
        assert!(!lower.contains(stale), "{description}");
    }
    assert!(!description.contains(FAKE_KEY), "{description}");

    let unsupported_model = AliSettings {
        workspace_id: "package-verification-workspace".into(),
        model: "some-other-model".into(),
        region: ALI_REGION.into(),
    }
    .description();
    assert!(
        unsupported_model.contains("unsupported model"),
        "{unsupported_model}"
    );
    assert!(
        !unsupported_model.contains("some-other-model"),
        "{unsupported_model}"
    );

    let unsupported_region = AliSettings {
        workspace_id: "package-verification-workspace".into(),
        model: ALI_MODEL.into(),
        region: "cn-shanghai".into(),
    }
    .description();
    assert!(
        unsupported_region.contains("unsupported region"),
        "{unsupported_region}"
    );
    assert!(
        !unsupported_region.contains("cn-shanghai"),
        "{unsupported_region}"
    );
}

#[test]
fn invalid_workspace_model_or_region_is_refused_before_capture_and_never_starts_the_device() {
    let cases = [
        (String::new(), ALI_MODEL.to_string(), ALI_REGION.to_string()),
        (
            "bad workspace".to_string(),
            ALI_MODEL.to_string(),
            ALI_REGION.to_string(),
        ),
        (
            "bad/workspace".to_string(),
            ALI_MODEL.to_string(),
            ALI_REGION.to_string(),
        ),
        (
            "a".repeat(65),
            ALI_MODEL.to_string(),
            ALI_REGION.to_string(),
        ),
        (
            "package-verification-workspace".to_string(),
            "some-other-model".to_string(),
            ALI_REGION.to_string(),
        ),
        (
            "package-verification-workspace".to_string(),
            ALI_MODEL.to_string(),
            "cn-shanghai".to_string(),
        ),
    ];
    for (workspace, model, region) in cases {
        let runtime = Runtime::new();
        let started = Arc::new(AtomicBool::new(false));
        let (mut app, ledger) = application(
            &runtime,
            Arc::new(FixedKey(FAKE_KEY)),
            AliSettings {
                workspace_id: workspace,
                model,
                region,
            },
            started.clone(),
        );

        let before = app.snapshot();
        assert_eq!(before.phase, Phase::Failure, "{before:?}");
        assert_eq!(before.preparation, Preparation::Failed, "{before:?}");
        let backend = before.backend.as_deref().unwrap_or_default();
        assert!(backend.contains("Alibaba Model Studio"), "{before:?}");
        assert!(backend.contains(ALI_MODEL) || backend.contains("unsupported model"));
        assert!(backend.contains(ALI_REGION) || backend.contains("unsupported region"));

        app.dispatch(Control::Start);
        let after = app.snapshot();
        assert_eq!(after.phase, Phase::Failure, "{after:?}");
        assert!(
            after.detail.contains("valid Alibaba workspace"),
            "{after:?}"
        );
        assert!(!after.detail.contains(FAKE_KEY), "{after:?}");
        assert!(
            !started.load(Ordering::SeqCst),
            "the playback device opened"
        );
        assert!(
            ledger.snapshot().unwrap().attempts.is_empty(),
            "a refused configuration must not reserve a paid request"
        );
        assert!(
            files_mentioning(&runtime.0, FAKE_KEY).is_empty(),
            "the key reached a runtime file: {:?}",
            files_mentioning(&runtime.0, FAKE_KEY)
        );
    }
}

#[test]
fn missing_or_malformed_credentials_are_refused_without_echoing_the_secret() {
    // The credential boundary rejects an empty key and anything that is not
    // printable ASCII (the same rule the local Credential Manager helper applies).
    assert_eq!(
        Secret::new(String::new()).err(),
        Some(CloudError::MissingKey)
    );
    assert_eq!(
        Secret::new("   ".into()).err(),
        Some(CloudError::MissingKey)
    );
    let too_long = "a".repeat(4097);
    for malformed in [
        "bad key",
        "bad\tkey",
        "key\nwith-newline",
        too_long.as_str(),
    ] {
        assert_eq!(
            Secret::new(malformed.into()).err(),
            Some(CloudError::Credential),
            "accepted {malformed:?}"
        );
    }
    assert!(Secret::new("a".repeat(4096)).is_ok());
    assert!(Secret::new(FAKE_KEY.into()).is_ok());

    for credentials in [
        Arc::new(MissingKey) as Arc<dyn CredentialSource>,
        Arc::new(FixedKey("bad key")) as Arc<dyn CredentialSource>,
    ] {
        let runtime = Runtime::new();
        let started = Arc::new(AtomicBool::new(false));
        let (mut app, ledger) = application(
            &runtime,
            credentials,
            AliSettings::beijing("package-verification-workspace".into()),
            started.clone(),
        );
        let before = app.snapshot();
        assert_eq!(before.phase, Phase::Failure, "{before:?}");
        assert!(
            before.detail.contains("No Alibaba cloud API key")
                || before.detail.contains("invalid format"),
            "{before:?}"
        );
        app.dispatch(Control::Start);
        let after = app.snapshot();
        assert_eq!(after.phase, Phase::Failure, "{after:?}");
        assert!(
            after.detail.contains("No Alibaba cloud API key")
                || after.detail.contains("invalid format"),
            "{after:?}"
        );
        assert!(!after.detail.contains(FAKE_KEY), "{after:?}");
        assert!(
            !started.load(Ordering::SeqCst),
            "the playback device opened"
        );
        assert!(ledger.snapshot().unwrap().attempts.is_empty());
        assert!(
            files_mentioning(&runtime.0, FAKE_KEY).is_empty()
                && files_mentioning(&runtime.0, "bad key").is_empty(),
            "key material reached a runtime file"
        );
    }
}

#[test]
fn the_shipped_settings_file_rejects_unknown_or_incomplete_fields() {
    let valid = format!(
        r#"{{"workspace_id":"package-verification-workspace","model":"{ALI_MODEL}","region":"{ALI_REGION}"}}"#
    );
    let settings: AliSettings = serde_json::from_str(&valid).unwrap();
    assert_eq!(settings.workspace_id, "package-verification-workspace");
    assert_eq!(settings.model, ALI_MODEL);
    assert_eq!(settings.region, ALI_REGION);

    for invalid in [
        // An unknown field must not be silently accepted (deny_unknown_fields).
        format!(
            r#"{{"workspace_id":"w","model":"{ALI_MODEL}","region":"{ALI_REGION}","verified":true}}"#
        ),
        // Missing fields are not a usable configuration.
        r#"{"workspace_id":"w","model":"qwen-audio-3.1-asr-flash-message"}"#.to_string(),
        // The shipped file is JSON, not an arbitrary token.
        "package-verification-workspace".to_string(),
    ] {
        assert!(
            serde_json::from_str::<AliSettings>(&invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

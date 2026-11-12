//! Readiness must mean the configured local backend can actually load its model.
//! These tests use throwaway engine directories with deliberately broken copies;
//! they never open a real model, audio device, or desktop control.
#![cfg(windows)]

use listen::LocalTranscriber;
use listen::whisper::{Backend, Whisper};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "listen-readiness-{label}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

struct FakeEngine {
    directory: PathBuf,
    executable: PathBuf,
    model: PathBuf,
    vad_model: PathBuf,
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// `fake` is copied as both local executables. `where.exe` exits non-zero for the
/// model-load arguments, which is how a broken CLI is represented. The DLL is
/// deliberately not the pinned build, so the in-process host is unavailable too.
fn fake_engine(label: &str, fake: &str) -> FakeEngine {
    let directory = temp_dir(label);
    let executable = directory.join("whisper-cli.exe");
    fs::copy(Path::new(r"C:\Windows\System32").join(fake), &executable).unwrap();
    fs::copy(
        Path::new(r"C:\Windows\System32").join(fake),
        directory.join("vad-speech-segments.exe"),
    )
    .unwrap();
    fs::write(
        directory.join("whisper.dll"),
        b"not the pinned whisper.cpp DLL",
    )
    .unwrap();
    let model = directory.join("ggml-test.bin");
    // `check_ready` only reads the magic; the real load must be what rejects it.
    fs::write(&model, b"ggml\x00\x00\x00\x00").unwrap();
    let vad_model = directory.join("ggml-test-vad.bin");
    fs::write(&vad_model, b"ggml\x00\x00\x00\x00").unwrap();
    FakeEngine {
        directory,
        executable,
        model,
        vad_model,
    }
}

fn prepare(whisper: &Whisper) -> Result<String, String> {
    let receiver = whisper
        .prepare()
        .expect("a model-host backend must report preparation, not return None");
    receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("preparation must finish")
}

#[test]
fn a_host_failure_with_a_broken_cli_is_reported_as_failure_not_ready() {
    let engine = fake_engine("host-broken-cli", "where.exe");
    let whisper = Whisper::with_backend(
        engine.executable.clone(),
        engine.model.clone(),
        engine.vad_model.clone(),
        "zh".into(),
        1,
        Backend::ModelHost,
    );
    let error = prepare(&whisper).expect_err("a broken model/host must not claim Ready");
    assert!(
        error.contains("whisper.cpp CLI") && error.contains("could not be loaded"),
        "{error}"
    );
    assert!(error.contains("Capture is unavailable"), "{error}");
}

#[test]
fn an_explicit_cli_backend_must_load_the_model_before_it_is_ready() {
    let engine = fake_engine("cli-broken", "where.exe");
    let whisper = Whisper::with_backend(
        engine.executable.clone(),
        engine.model.clone(),
        engine.vad_model.clone(),
        "zh".into(),
        1,
        Backend::Cli,
    );
    let error = prepare(&whisper).expect_err("an unverified CLI model must not claim Ready");
    assert!(
        error.contains("could not be loaded by the whisper.cpp CLI"),
        "{error}"
    );
    assert!(error.contains("not a claim of readiness"), "{error}");
}

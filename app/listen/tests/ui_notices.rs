//! User-visible conditions that live outside the capture state machine: a
//! clipboard copy the platform refused, and a capsule the platform refused to hide
//! from screen capture. Both are driven at the same `Application` boundary the GUI
//! and `--control` use, and asserted through `Snapshot`, which is exactly what
//! `status.txt` and the expanded panel render. No device or window is opened.
#![cfg(windows)]

use listen::{
    AppResult, Application, CapturedAudio, Control, LocalTranscriber, PlaybackAudio, Recording,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Runtime(PathBuf);
impl Runtime {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "listen-notices-{}-{}",
            std::process::id(),
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
            description: "notice test playback device".into(),
        })
    }
}

struct Model;
impl LocalTranscriber for Model {
    fn check_ready(&self) -> AppResult<()> {
        Ok(())
    }
    fn transcribe(&self, _audio: &CapturedAudio, _directory: &Path) -> AppResult<String> {
        Ok("第一行：Rust 和 WASAPI。".into())
    }
}

fn delivered_result() -> (Application, Runtime) {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model),
        runtime.0.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    app.dispatch(Control::Stop);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.poll();
        if app.snapshot().phase == listen::Phase::Result {
            return (app, runtime);
        }
        assert!(std::time::Instant::now() < deadline, "{:?}", app.snapshot());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_failed_clipboard_copy_is_reported_instead_of_silently_dropped() {
    let (mut app, _runtime) = delivered_result();
    app.copy_result(|_| Err("the clipboard is locked by another application".into()));

    let notice = app
        .snapshot()
        .notice
        .expect("a failed clipboard copy must be user-visible");
    let text = notice.text();
    assert!(
        text.contains("Could not copy the result: the clipboard is locked by another application"),
        "{text}"
    );
    assert!(
        text.contains("The clipboard was not replaced"),
        "the user must be told the clipboard still holds whatever it held before: {text}"
    );
}

#[test]
fn a_successful_copy_writes_the_complete_result_and_clears_an_earlier_copy_failure() {
    let (mut app, _runtime) = delivered_result();
    app.copy_result(|_| Err("the clipboard is locked".into()));
    assert!(app.snapshot().notice.is_some());

    let mut copied = None;
    app.copy_result(|text| {
        copied = Some(text.to_string());
        Ok(())
    });
    assert_eq!(
        copied.as_deref(),
        Some("第一行：Rust 和 WASAPI。"),
        "only the complete result is copied"
    );
    assert!(
        app.snapshot().notice.is_none(),
        "a successful copy clears its own earlier failure: {:?}",
        app.snapshot().notice
    );
}

#[test]
fn a_copy_with_no_complete_result_does_not_touch_the_clipboard_or_report_an_error() {
    let runtime = Runtime::new();
    let mut app = Application::new(
        Arc::new(Playback),
        Arc::new(Model),
        runtime.0.clone(),
        Ok(()),
    );
    assert!(app.snapshot().text.is_empty());
    app.copy_result(|_| panic!("the clipboard must not be touched without a result"));
    assert!(app.snapshot().notice.is_none());
}

#[test]
fn a_capsule_that_cannot_hide_from_screen_capture_is_reported_and_survives_a_copy() {
    let (mut app, _runtime) = delivered_result();
    app.report_capture_exclusion("Access is denied");
    let text = app
        .snapshot()
        .notice
        .clone()
        .expect("a failed exclusion must be user-visible; the ADR promises the capsule is hidden")
        .text();
    assert!(
        text.contains("could not be hidden from screen capture"),
        "{text}"
    );
    assert!(
        text.contains("screen sharing") || text.contains("recordings"),
        "the user must learn the practical consequence: {text}"
    );
    assert!(
        text.contains("Access is denied"),
        "the platform's reason must be kept: {text}"
    );

    // It is a privacy warning about the capsule, not about the clipboard, so a
    // later successful copy must not erase it.
    app.copy_result(|_| Ok(()));
    assert_eq!(
        app.snapshot().notice.map(|notice| notice.text()),
        Some(text),
        "a successful copy must not clear the capture-exclusion warning"
    );
}

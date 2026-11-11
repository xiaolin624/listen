//! Explicit, offline model acceptance at the agreed Application start/stop/output seam.
//! No playback device, desktop controls, or GUI are opened by these tests.
#![cfg(windows)]

use listen::{
    AppResult, Application, CapturedAudio, Control, Phase, PlaybackAudio, Recording,
    whisper::Whisper,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct FixturePlayback(PathBuf);
struct FixtureRecording(PathBuf);
impl PlaybackAudio for FixturePlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let path = directory.join("audio.wav");
        fs::copy(&self.0, &path).map_err(|e| e.to_string())?;
        Ok(Box::new(FixtureRecording(path)))
    }
}
impl Recording for FixtureRecording {
    fn finish(self: Box<Self>) -> AppResult<CapturedAudio> {
        Ok(CapturedAudio {
            path: self.0,
            description: "known offline WAV fixture; no real capture".into(),
        })
    }
}

fn configured_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("Set {name}; see docs/issue-9-verification.md"))
}

#[test]
#[ignore = "requires explicit local engine/model and the generated quality corpus"]
fn non_speech_audio_never_replaces_the_previous_transcript_with_generated_speech() {
    for sample in ["silence", "instrumental", "notification"] {
        verify_no_speech(sample);
    }
}

fn verify_no_speech(sample: &str) {
    let directory = std::env::temp_dir().join(format!(
        "listen-local-quality-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let previous = "上一次转写。\nReview the API response.";
    fs::write(directory.join("current-result.txt"), previous).unwrap();
    let mut app = Application::new(
        Arc::new(FixturePlayback(
            configured_path("LISTEN_QUALITY_CORPUS").join(format!("{sample}.wav")),
        )),
        Arc::new(Whisper {
            executable: configured_path("LISTEN_WHISPER"),
            model: configured_path("LISTEN_MODEL"),
            vad_model: configured_path("LISTEN_VAD_MODEL"),
            language: "zh".into(),
            threads: 6,
        }),
        directory.clone(),
        Ok(()),
    );
    app.dispatch(Control::Start);
    assert_eq!(
        app.snapshot().phase,
        Phase::Capturing,
        "{:?}",
        app.snapshot()
    );
    app.dispatch(Control::Stop);
    let deadline = Instant::now() + Duration::from_secs(120);
    while app.snapshot().phase == Phase::Processing {
        assert!(Instant::now() < deadline);
        app.poll();
        thread::sleep(Duration::from_millis(10));
    }
    let result = app.snapshot();
    assert_eq!(result.phase, Phase::NoSpeech, "{result:?}");
    assert_eq!(result.text, previous);
    assert_eq!(
        fs::read_to_string(directory.join("current-result.txt")).unwrap(),
        previous
    );
    assert!(result.retained_audio.is_empty());
    fs::remove_dir_all(&directory).unwrap();
}

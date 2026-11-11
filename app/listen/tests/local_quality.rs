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
struct EmptyPlayback;
impl PlaybackAudio for EmptyPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let path = directory.join("audio.wav");
        hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .and_then(|writer| writer.finalize())
        .map_err(|e| e.to_string())?;
        Ok(Box::new(FixtureRecording(path)))
    }
}
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
    for sample in ["silence", "instrumental", "notification", "empty"] {
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
    let playback: Arc<dyn PlaybackAudio> = if sample == "empty" {
        Arc::new(EmptyPlayback)
    } else {
        Arc::new(FixturePlayback(
            configured_path("LISTEN_QUALITY_CORPUS").join(format!("{sample}.wav")),
        ))
    };
    let mut app = Application::new(
        playback,
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

#[test]
#[ignore = "requires explicit local engine/model and the generated quality corpus"]
fn english_technical_words_and_spoken_numbers_reach_the_copyable_result() {
    let directory = std::env::temp_dir().join(format!(
        "listen-numbers-quality-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let mut app = Application::new(
        Arc::new(FixturePlayback(
            configured_path("LISTEN_QUALITY_CORPUS").join("english-numbers.wav"),
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
    assert_eq!(result.phase, Phase::Result, "{result:?}");
    println!("Known english-numbers fixture result: {}", result.text);
    let text = result.text.to_lowercase();
    // Source truth is the spoken corpus, not a snapshot of another decoder run.
    for phrase in [
        "api uses rust",
        "windows",
        "whisper",
        "cpu",
        "gpu",
        "version 1.8.2",
        "port 8080",
        "retry 3 times",
        "wait 2 seconds",
    ] {
        assert!(
            text.contains(phrase),
            "Missing source content {phrase:?}: {result:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(directory.join("current-result.txt")).unwrap(),
        result.text
    );
    assert!(result.retained_audio.is_empty());
    fs::remove_dir_all(&directory).unwrap();
}

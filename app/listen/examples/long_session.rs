//! Silent wall-clock replay at the same Start/Stop/result seam as the hotkey.
//! Only the playback device is replaced; segmentation, local Whisper and output
//! lifecycle are production code. This program never opens an audio device.
use listen::{
    AppResult, Application, CapturedAudio, Control, LocalTranscriber, Phase, PlaybackAudio,
    Recording, TimedText,
    streaming::{CaptureProgress, Progress, SegmentConfig, SegmentStream, SegmentedAudioWriter},
    whisper::Whisper,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Tape {
    input: PathBuf,
    samples: u64,
    config: SegmentConfig,
}
struct Playing {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<AppResult<CapturedAudio>>>,
    stream: Option<SegmentStream>,
    progress: Arc<Progress>,
}
impl PlaybackAudio for Tape {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let mut reader = hound::WavReader::open(&self.input).map_err(|e| e.to_string())?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.sample_rate != 16_000
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
            || reader.duration() == 0
        {
            return Err("--input must be a nonempty 16 kHz mono signed 16-bit PCM WAV".into());
        }
        let (mut writer, stream) = SegmentedAudioWriter::new(directory, self.config)?;
        let progress = stream.progress.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let samples = self.samples;
        let worker = thread::spawn(move || {
            let start = Instant::now();
            let mut received = 0u64;
            while received < samples && !signal.load(Ordering::Acquire) {
                let block = (samples - received).min(1600);
                let due = Duration::from_secs_f64((received + block) as f64 / 16_000.0);
                while start.elapsed() < due && !signal.load(Ordering::Acquire) {
                    thread::sleep(
                        due.saturating_sub(start.elapsed())
                            .min(Duration::from_millis(5)),
                    );
                }
                if signal.load(Ordering::Acquire) {
                    break;
                }
                for _ in 0..block {
                    let sample = match reader.samples::<i16>().next() {
                        Some(sample) => sample.map_err(|e| e.to_string())?,
                        None => {
                            reader.seek(0).map_err(|e| e.to_string())?;
                            reader
                                .samples::<i16>()
                                .next()
                                .unwrap()
                                .map_err(|e| e.to_string())?
                        }
                    };
                    writer.push(sample)?;
                    received += 1;
                }
                writer.flush()?;
            }
            while !signal.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            writer.finish(format!(
                "silent wall-clock WAV replay; {received} samples, {:.3} s",
                received as f64 / 16_000.0
            ))
        });
        Ok(Box::new(Playing {
            stop,
            worker: Some(worker),
            stream: Some(stream),
            progress,
        }))
    }
}
impl Recording for Playing {
    fn take_segments(&mut self) -> Option<SegmentStream> {
        self.stream.take()
    }
    fn progress(&self) -> Option<CaptureProgress> {
        Some(self.progress.snapshot())
    }
    fn failure(&self) -> Option<String> {
        self.worker
            .as_ref()
            .filter(|w| w.is_finished())
            .map(|_| "WAV replay interrupted; finishing received audio".into())
    }
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| "WAV replay worker panicked".to_string())?
    }
}
impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct DelayedModel {
    inner: Whisper,
    delay: Duration,
}
impl LocalTranscriber for DelayedModel {
    fn check_ready(&self) -> AppResult<()> {
        self.inner.check_ready()
    }
    fn prepare(&self) -> Option<std::sync::mpsc::Receiver<AppResult<String>>> {
        self.inner.prepare()
    }
    fn backend_note(&self) -> Option<String> {
        self.inner.backend_note()
    }
    fn transcribe(&self, audio: &CapturedAudio, directory: &Path) -> AppResult<String> {
        thread::sleep(self.delay);
        self.inner.transcribe(audio, directory)
    }
    fn transcribe_segment(
        &self,
        audio: &CapturedAudio,
        directory: &Path,
    ) -> AppResult<Vec<TimedText>> {
        thread::sleep(self.delay);
        self.inner.transcribe_segment(audio, directory)
    }
}

fn run() -> AppResult<()> {
    let mut input = None;
    let mut runtime = None;
    let mut engine = None;
    let mut model = None;
    let mut vad_model = None;
    let mut seconds = 3600.0f64;
    let mut delay_ms = 0u64;
    let mut threads = 6usize;
    let mut interval = 5.0f64;
    let mut backend = listen::whisper::Backend::ModelHost;
    let mut config = SegmentConfig::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(
                "long_session --input <16k-mono-PCM16.wav> --runtime <directory> --engine <whisper-cli.exe> --model <ggml.bin> --vad-model <ggml-silero.bin> [--backend host|cli] [--seconds 3600] [--threads 6] [--interval-seconds 5] [--delay-ms 0] [--spool-bytes 9600440]\nOnly playback is substituted; this uses real wall-clock pacing, production segmentation and actual local inference. No desktop or audio device is opened."
            );
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {arg}"))?;
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(value)),
            "--runtime" => runtime = Some(PathBuf::from(value)),
            "--engine" => engine = Some(PathBuf::from(value)),
            "--model" => model = Some(PathBuf::from(value)),
            "--vad-model" => vad_model = Some(PathBuf::from(value)),
            "--backend" => {
                backend = match value.as_str() {
                    "host" => listen::whisper::Backend::ModelHost,
                    "cli" => listen::whisper::Backend::Cli,
                    _ => return Err("--backend must be host or cli".into()),
                }
            }
            "--seconds" => seconds = value.parse().map_err(|_| "Invalid --seconds")?,
            "--threads" => threads = value.parse().map_err(|_| "Invalid --threads")?,
            "--delay-ms" => delay_ms = value.parse().map_err(|_| "Invalid --delay-ms")?,
            "--interval-seconds" => {
                interval = value.parse().map_err(|_| "Invalid --interval-seconds")?
            }
            "--spool-bytes" => {
                config.spool_limit_bytes = value.parse().map_err(|_| "Invalid --spool-bytes")?
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    if !seconds.is_finite()
        || seconds <= 0.0
        || !interval.is_finite()
        || interval <= 0.0
        || threads == 0
    {
        return Err("Seconds, interval and threads must be positive finite values".into());
    }
    let runtime = runtime.ok_or("--runtime is required")?;
    fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    let requested = (seconds * 16_000.0).ceil() as u64;
    let tape = Tape {
        input: input.ok_or("--input is required")?,
        samples: requested,
        config,
    };
    let transcriber = DelayedModel {
        inner: Whisper::with_backend(
            engine.ok_or("--engine is required")?,
            model.ok_or("--model is required")?,
            vad_model.ok_or("--vad-model is required")?,
            "zh".into(),
            threads,
            backend,
        ),
        delay: Duration::from_millis(delay_ms),
    };
    let mut app = Application::new(Arc::new(tape), Arc::new(transcriber), runtime, Ok(()));
    // First-use preparation is deliberately not reported as Ready. The harness
    // waits for the same state the GUI shows before it starts a capture.
    let preparation_started = Instant::now();
    loop {
        app.poll();
        let state = app.snapshot();
        if state.phase != Phase::Preparing {
            println!(
                "{}",
                serde_json::json!({
                    "phase": format!("{:?}", state.phase),
                    "preparation_ms": state.timing.preparation_ms,
                    "backend": state.backend,
                    "detail": state.detail,
                })
            );
            if state.phase == Phase::Failure {
                return Err("Local model preparation failed; inspect the JSON record".into());
            }
            break;
        }
        if preparation_started.elapsed() > Duration::from_secs(120) {
            return Err("Local model preparation did not finish within 120 seconds".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    let start = Instant::now();
    let mut stopped = None;
    let mut next_report = Duration::ZERO;
    app.dispatch(Control::Start);
    loop {
        app.poll();
        let state = app.snapshot();
        if state.phase == Phase::Capturing && state.progress.received_samples >= requested {
            stopped = Some(Instant::now());
            app.dispatch(Control::Stop);
        }
        if state.phase == Phase::Processing && stopped.is_none() {
            stopped = Some(Instant::now());
        }
        let terminal = matches!(
            state.phase,
            Phase::Result | Phase::NoSpeech | Phase::Failure
        );
        if start.elapsed() >= next_report || terminal {
            println!(
                "{}",
                serde_json::json!({
                    "elapsed_seconds": start.elapsed().as_secs_f64(), "phase": format!("{:?}", state.phase),
                    "received_samples": state.progress.received_samples,
                    "audio_seconds": state.progress.received_samples as f64 / 16_000.0,
                    "buffered_samples": state.progress.buffered_samples,
                    "pending_segments": state.progress.pending_segments,
                    "peak_pending_segments": state.progress.peak_pending_segments,
                    "recognized_segments": state.progress.recognized_segments,
                    "spool_bytes": state.progress.spool_bytes,
                    "stop_to_result_seconds": stopped.map(|s| s.elapsed().as_secs_f64()),
                    "terminal": terminal, "result_characters": state.text.chars().count(),
                    "detail": state.detail,
                    "retained_audio": state.retained_audio,
                    "backend": state.backend,
                    "preparation_ms": state.timing.preparation_ms,
                    "timing_stop_to_result_ms": state.timing.stop_to_result_ms,
                    "timing_finalize_ms": state.timing.finalize_ms,
                    "timing_recognition_ms": state.timing.recognition_ms,
                    "timing_last_window_wait_ms": state.timing.last_window_wait_ms,
                    "timing_last_window_inference_ms": state.timing.last_window_inference_ms,
                    "timing_save_ms": state.timing.save_ms,
                    "timing_delivery_ms": state.timing.delivery_ms,
                    "timing_recognition_windows": state.timing.recognition_windows,
                })
            );
            next_report = start.elapsed() + Duration::from_secs_f64(interval);
        }
        if terminal {
            if state.phase == Phase::Failure {
                return Err(
                    "Long-session flow failed; inspect the JSON record and retained recovery files"
                        .into(),
                );
            }
            if state.progress.received_samples != requested {
                return Err("Replay did not receive the requested sample count".into());
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

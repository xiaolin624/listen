//! Records the default multimedia render endpoint to a 16 kHz mono 16-bit PCM
//! WAV and never recognizes anything. This is the recorder for the real
//! Tencent-Meeting corpus (#22): the playback device is opened exactly the way
//! the application opens it, but there is no model, no target editor and no GUI.
//! Audio routed only to the default communication endpoint is not included.
use listen::{
    AppResult, CaptureEndpoint, PlaybackAudio, audio::WasapiPlayback,
    streaming::retained_audio_path,
};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SAMPLE_RATE: u32 = 16_000;
/// The shared playback spool queues at most eight 30 s windows. Keeping a single
/// take under four minutes means this recorder never depends on a recognition
/// worker draining that queue.
const MAX_SECONDS: f64 = 240.0;

struct Record {
    output: PathBuf,
    seconds: Option<f64>,
    runtime: Option<PathBuf>,
    report: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("capture_only: {error}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let Some(options) = parse()? else {
        return Ok(());
    };
    record(options)
}

/// Returns `None` after printing help. Anything unparsable is a hard error so a
/// mistyped output path never turns into a silently misplaced recording.
fn parse() -> AppResult<Option<Record>> {
    let mut output = None;
    let mut seconds = None;
    let mut runtime = None;
    let mut report = None;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--help" || argument == "-h" {
            println!(
                "capture_only --output <path.wav> [--seconds N | press Enter to stop] [--runtime <directory>] [--report <path.json>]\n\
                 Records the default multimedia render endpoint to a 16 kHz mono 16-bit PCM WAV; nothing is recognized.\n\
                 --seconds must be positive and at most {MAX_SECONDS:.0}; without it the recorder stops on Enter.\n\
                 --runtime keeps the temporary capture windows (default: a fresh directory under the system temp directory).\n\
                 --report writes the captured endpoint and the measured audio metadata as JSON."
            );
            return Ok(None);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {argument}"))?;
        match argument.as_str() {
            "--output" => output = Some(PathBuf::from(value)),
            "--runtime" => runtime = Some(PathBuf::from(value)),
            "--report" => report = Some(PathBuf::from(value)),
            "--seconds" => {
                let parsed: f64 = value
                    .parse()
                    .map_err(|_| format!("Invalid --seconds: {value}"))?;
                if !parsed.is_finite() || parsed <= 0.0 || parsed > MAX_SECONDS {
                    return Err(format!(
                        "--seconds must be positive and at most {MAX_SECONDS:.0}"
                    ));
                }
                seconds = Some(parsed);
            }
            _ => return Err(format!("Unknown option: {argument}")),
        }
    }
    Ok(Some(Record {
        output: absolute(output.ok_or("--output is required")?)?,
        seconds,
        runtime: runtime.map(absolute).transpose()?,
        report: report.map(absolute).transpose()?,
    }))
}

fn absolute(path: PathBuf) -> AppResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(env::current_dir()
        .map_err(|error| format!("Cannot resolve the current directory: {error}"))?
        .join(path))
}

fn record(options: Record) -> AppResult<()> {
    let Record {
        output,
        seconds,
        runtime,
        report,
    } = options;
    let parent = output.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let runtime = runtime.unwrap_or_else(|| {
        env::temp_dir().join(format!(
            "listen-capture-only-{}-{unique}",
            std::process::id()
        ))
    });
    fs::create_dir_all(&runtime)
        .map_err(|error| format!("Cannot create {}: {error}", runtime.display()))?;
    // Each take gets its own directory so a previous recording can never be
    // merged into this one.
    let directory = runtime.join(format!("capture-{}-{unique}", std::process::id()));
    fs::create_dir(&directory)
        .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;

    println!(
        "Recording the default multimedia render endpoint. No recognition and no network are involved."
    );
    println!("Output WAV: {}", output.display());
    let started = Instant::now();
    let recording = WasapiPlayback
        .start(&directory)
        .map_err(|error| format!("Cannot start playback capture: {error}"))?;
    let endpoint = recording.endpoint();
    println!("Capturing the {}.", describe(&endpoint));
    println!("Capture windows are kept in: {}", directory.display());
    match seconds {
        Some(seconds) => println!("Recording for {seconds:.3} seconds."),
        None => {
            println!(
                "Recording. Press Enter to stop. Keep a single take under {MAX_SECONDS:.0} seconds."
            );
            io::stdout().flush().map_err(|error| error.to_string())?;
        }
    }

    let mut interruption = None;
    match seconds {
        Some(seconds) => {
            let deadline = Instant::now() + Duration::from_secs_f64(seconds);
            let mut next_report = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(error) = recording.failure() {
                    interruption = Some(error);
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                if seconds > 10.0 && now >= next_report {
                    report_progress(recording.progress().map(|p| p.received_samples));
                    next_report = now + Duration::from_secs(5);
                }
                thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(25)),
                );
            }
        }
        None => {
            // EOF on a redirected stdin (the recording script closes it when the
            // user presses Enter) stops the take exactly like an Enter key press.
            let mut line = String::new();
            io::stdin()
                .read_line(&mut line)
                .map_err(|error| format!("Cannot read the stop key: {error}"))?;
            if let Some(error) = recording.failure() {
                interruption = Some(error);
            }
        }
    }
    println!(
        "Stopping after {:.3} s of wall clock.",
        started.elapsed().as_secs_f64()
    );
    let captured = recording
        .finish()
        .map_err(|error| format!("Capture could not be finished: {error}"))?;
    let merged = if captured.path.is_file() {
        captured.path.clone()
    } else {
        // finish() returns the session directory; a single window is already the
        // whole take and several windows are rejoined without repeating overlap.
        if !has_capture_windows(&captured.path) {
            return Err(no_audio_message(&directory));
        }
        retained_audio_path(&captured.path)
            .map_err(|error| format!("Cannot assemble the recorded audio: {error}"))?
    };
    let info = wav_info(&merged)?;
    if info.samples == 0 {
        return Err(no_audio_message(&directory));
    }
    fs::copy(&merged, &output)
        .map_err(|error| format!("Cannot write {}: {error}", output.display()))?;
    let written = wav_info(&output)?;
    println!(
        "Wrote {}: {} samples, {:.3} s, {} Hz, {} channel(s), {} bit PCM.",
        output.display(),
        written.samples,
        written.duration_seconds,
        written.sample_rate,
        written.channels,
        written.bits_per_sample
    );
    println!("Captured endpoint: {}.", describe(&endpoint));
    if let Some(path) = &report {
        let json = serde_json::json!({
            "output": output.display().to_string(),
            "capture_directory": directory.display().to_string(),
            "endpoint": endpoint.as_ref().map(|endpoint| serde_json::json!({
                "role": endpoint.role,
                "name": endpoint.name,
                "id": endpoint.id,
            })),
            "requested_seconds": seconds,
            "wall_seconds": started.elapsed().as_secs_f64(),
            "samples": written.samples,
            "duration_seconds": written.duration_seconds,
            "sample_rate": written.sample_rate,
            "channels": written.channels,
            "bits_per_sample": written.bits_per_sample,
            "source": "WasapiPlayback loopback of the default multimedia render endpoint; no recognition",
        });
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&json).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
        println!("Capture report: {}.", path.display());
    }
    match interruption {
        Some(error) => Err(format!(
            "Capture was interrupted: {error} The WAV written to {} is incomplete; record this sentence again.",
            output.display()
        )),
        None => Ok(()),
    }
}

fn report_progress(received_samples: Option<u64>) {
    match received_samples {
        Some(samples) => println!(
            "Captured {:.1} s so far.",
            samples as f64 / SAMPLE_RATE as f64
        ),
        None => println!("Capturing."),
    }
}

/// Zero samples is not a storage error: the render endpoint produced no loopback
/// data, which commonly means nothing was playing on it yet.
fn no_audio_message(directory: &Path) -> String {
    format!(
        "The capture produced no audio samples, so there is nothing to score. Was anything playing through the default multimedia render endpoint? Windows can stop loopback data while the render device is idle, so start the meeting audio first and record again. Existing capture windows: {}",
        directory.display()
    )
}

/// Whether the capture thread ever opened a playback window. No window means no
/// sample reached the writer, which is a different outcome from a merge failure.
fn has_capture_windows(directory: &Path) -> bool {
    fs::read_dir(directory)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().is_dir()
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("segment-"))
            })
        })
        .unwrap_or(false)
}

fn describe(endpoint: &Option<CaptureEndpoint>) -> String {
    match endpoint {
        Some(endpoint) => format!("{} '{}' ({})", endpoint.role, endpoint.name, endpoint.id),
        None => "unidentified default multimedia render endpoint".into(),
    }
}

struct WavInfo {
    samples: u64,
    duration_seconds: f64,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
}

/// Re-read the file header so the reported metadata describes the file the user
/// actually received, not what the capture path intended to write.
fn wav_info(path: &Path) -> AppResult<WavInfo> {
    let reader = hound::WavReader::open(path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int
        || spec.channels != 1
        || spec.sample_rate != SAMPLE_RATE
        || spec.bits_per_sample != 16
    {
        return Err(format!(
            "{} is not a 16 kHz mono 16-bit PCM WAV ({} Hz, {} channel(s), {} bit, {:?})",
            path.display(),
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample,
            spec.sample_format
        ));
    }
    let samples = u64::from(reader.duration());
    Ok(WavInfo {
        samples,
        duration_seconds: samples as f64 / SAMPLE_RATE as f64,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits_per_sample: spec.bits_per_sample,
    })
}

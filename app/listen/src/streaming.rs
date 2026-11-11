//! Bounded PCM windows shared by WASAPI capture and controlled playback devices.
//! Recognition receives paths, never an accumulating in-memory recording.
use crate::{AppResult, CapturedAudio, LocalTranscriber, TimedText};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

pub const SAMPLE_RATE: u64 = 16_000;

#[derive(Clone, Copy)]
pub struct SegmentConfig {
    pub window_samples: usize,
    pub overlap_samples: usize,
    pub max_pending_segments: usize,
    pub spool_limit_bytes: u64,
}
impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            window_samples: 30 * 16_000,
            overlap_samples: 2 * 16_000,
            max_pending_segments: 8,
            spool_limit_bytes: 10 * (30 * 16_000 * 2 + 44),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureProgress {
    pub received_samples: u64,
    pub buffered_samples: usize,
    pub pending_segments: usize,
    pub recognized_segments: u64,
    pub spool_bytes: u64,
    pub peak_pending_segments: usize,
}

#[derive(Default)]
pub struct Progress {
    received: AtomicU64,
    buffered: AtomicUsize,
    pending: AtomicUsize,
    recognized: AtomicU64,
    spool: AtomicU64,
    peak_pending: AtomicUsize,
    error: Mutex<Option<String>>,
}
impl Progress {
    pub fn snapshot(&self) -> CaptureProgress {
        CaptureProgress {
            received_samples: self.received.load(Ordering::Relaxed),
            buffered_samples: self.buffered.load(Ordering::Relaxed),
            pending_segments: self.pending.load(Ordering::Relaxed),
            recognized_segments: self.recognized.load(Ordering::Relaxed),
            spool_bytes: self.spool.load(Ordering::Relaxed),
            peak_pending_segments: self.peak_pending.load(Ordering::Relaxed),
        }
    }
    pub fn failure(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
    fn fail(&self, error: String) {
        *self.error.lock().unwrap() = Some(error);
    }
}

struct AudioSegment {
    audio: CapturedAudio,
    start_sample: u64,
    samples: usize,
    overlap_samples: usize,
    final_segment: bool,
}

pub struct SegmentStream {
    receiver: mpsc::Receiver<AudioSegment>,
    pub progress: Arc<Progress>,
}

/// Device-side writer. Each accepted sample is stored before a segment is queued.
/// A full queue interrupts capture explicitly and leaves its WAV on disk.
pub struct SegmentedAudioWriter {
    directory: PathBuf,
    config: SegmentConfig,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    path: PathBuf,
    sender: mpsc::SyncSender<AudioSegment>,
    progress: Arc<Progress>,
    overlap: VecDeque<i16>,
    index: u64,
    window_start: u64,
    samples: usize,
    fresh_samples: usize,
}
impl SegmentedAudioWriter {
    pub fn new(directory: &Path, config: SegmentConfig) -> AppResult<(Self, SegmentStream)> {
        if config.window_samples == 0
            || config.overlap_samples >= config.window_samples
            || config.max_pending_segments == 0
        {
            return Err("Invalid bounded playback window configuration".into());
        }
        // Retry after a failed first recognition needs to know how much audio the
        // adjacent windows share, so the whole capture can be rejoined without
        // repeating the overlap. This is capture metadata, not a user-visible name.
        fs::write(
            directory.join("capture-manifest.txt"),
            format!(
                "window_samples={}\noverlap_samples={}\n",
                config.window_samples, config.overlap_samples
            ),
        )
        .map_err(|e| format!("Cannot write temporary capture metadata: {e}"))?;
        let (sender, receiver) = mpsc::sync_channel(config.max_pending_segments);
        let progress = Arc::new(Progress::default());
        let writer = Self {
            directory: directory.into(),
            config,
            writer: None,
            path: PathBuf::new(),
            sender,
            progress: progress.clone(),
            overlap: VecDeque::with_capacity(config.overlap_samples),
            index: 0,
            window_start: 0,
            samples: 0,
            fresh_samples: 0,
        };
        Ok((writer, SegmentStream { receiver, progress }))
    }

    fn open(&mut self) -> AppResult<()> {
        self.reserve(44 + self.overlap.len() as u64 * 2)?;
        let directory = self.directory.join(format!("segment-{:08}", self.index));
        fs::create_dir(&directory)
            .map_err(|e| format!("Cannot create playback spool directory: {e}"))?;
        self.path = directory.join("playback.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE as u32,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&self.path, spec)
            .map_err(|e| format!("Cannot create temporary playback audio: {e}"))?;
        for &sample in &self.overlap {
            writer
                .write_sample(sample)
                .map_err(|e| format!("Cannot store playback overlap: {e}"))?;
        }
        self.samples = self.overlap.len();
        self.fresh_samples = 0;
        self.progress
            .spool
            .fetch_add(44 + self.samples as u64 * 2, Ordering::Relaxed);
        self.writer = Some(writer);
        Ok(())
    }

    fn reserve(&self, bytes: u64) -> AppResult<()> {
        if self
            .progress
            .spool
            .load(Ordering::Relaxed)
            .saturating_add(bytes)
            > self.config.spool_limit_bytes
        {
            return Err("Playback storage budget exhausted; capture interrupted, received audio and checkpoint retained".into());
        }
        Ok(())
    }

    pub fn push(&mut self, sample: i16) -> AppResult<()> {
        let result: AppResult<()> = (|| {
            if self.writer.is_none() {
                self.open()?;
            }
            self.reserve(2)?;
            self.writer
                .as_mut()
                .unwrap()
                .write_sample(sample)
                .map_err(|e| {
                    format!("Cannot write temporary playback audio (storage exhausted): {e}")
                })?;
            self.samples += 1;
            self.fresh_samples += 1;
            self.progress.received.fetch_add(1, Ordering::Relaxed);
            self.progress.spool.fetch_add(2, Ordering::Relaxed);
            if self.config.overlap_samples > 0 {
                if self.overlap.len() == self.config.overlap_samples {
                    self.overlap.pop_front();
                }
                self.overlap.push_back(sample);
                self.progress
                    .buffered
                    .store(self.overlap.len(), Ordering::Relaxed);
            }
            if self.samples == self.config.window_samples {
                self.emit(false, false)?;
            }
            Ok(())
        })();
        if let Err(error) = &result {
            self.progress.fail(error.clone());
        }
        result
    }

    pub fn flush(&mut self) -> AppResult<()> {
        if let Some(writer) = &mut self.writer {
            writer
                .flush()
                .map_err(|e| format!("Cannot flush temporary playback audio: {e}"))?;
        }
        Ok(())
    }

    fn emit(&mut self, final_segment: bool, tolerate_disconnected: bool) -> AppResult<()> {
        self.writer
            .take()
            .unwrap()
            .finalize()
            .map_err(|e| format!("Cannot finalize retained playback audio: {e}"))?;
        let segment = AudioSegment {
            audio: CapturedAudio {
                path: self.path.clone(),
                description: format!("bounded playback window {}", self.index),
            },
            start_sample: self.window_start,
            samples: self.samples,
            overlap_samples: self.config.overlap_samples,
            final_segment,
        };
        let pending = self.progress.pending.fetch_add(1, Ordering::Relaxed) + 1;
        self.progress
            .peak_pending
            .fetch_max(pending, Ordering::Relaxed);
        match self.sender.try_send(segment) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                return Err("Recognition backlog exhausted the bounded playback spool; capture interrupted, received audio and checkpoint retained".into());
            }
            Err(mpsc::TrySendError::Disconnected(_)) if tolerate_disconnected => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(
                    "Local recognition stopped; capture interrupted, received audio retained"
                        .into(),
                );
            }
        }
        self.index += 1;
        if !final_segment {
            self.window_start += (self.samples - self.config.overlap_samples) as u64;
        }
        self.fresh_samples = 0;
        Ok(())
    }

    pub fn finish(mut self, description: String) -> AppResult<CapturedAudio> {
        if self.fresh_samples > 0 {
            // The last window is finalized on disk before it is queued. If the
            // recognition worker already stopped, the audio is still complete and
            // its own failure is reported separately; hand-off alone is not a
            // capture failure.
            self.emit(true, true)?;
        }
        Ok(CapturedAudio {
            path: self.directory.clone(),
            description,
        })
    }
}

/// One WAV file holding the whole capture that a failed first recognition attempt
/// left on disk. A single retained window is already whole; several windows are
/// rejoined with their shared overlap removed, so a whole-file retry never repeats
/// words at a window boundary.
pub fn retained_audio_path(directory: &Path) -> AppResult<PathBuf> {
    let mut segments = Vec::new();
    let entries =
        fs::read_dir(directory).map_err(|e| format!("Cannot read retained capture: {e}"))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("segment-") || !entry.path().is_dir() {
            continue;
        }
        let path = entry.path().join("playback.wav");
        if path.is_file() {
            segments.push(path);
        }
    }
    segments.sort();
    match segments.len() {
        0 => Err("The failed capture left no complete retained audio".into()),
        1 => Ok(segments.pop().unwrap()),
        _ => merge_segments(directory, &segments),
    }
}

fn merge_segments(directory: &Path, segments: &[PathBuf]) -> AppResult<PathBuf> {
    let manifest = fs::read_to_string(directory.join("capture-manifest.txt"))
        .map_err(|e| format!("Cannot read the temporary capture metadata: {e}"))?;
    let overlap = manifest
        .lines()
        .find_map(|line| line.strip_prefix("overlap_samples="))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .ok_or_else(|| {
            "The temporary capture metadata does not describe the window overlap".to_string()
        })?;
    let output = directory.join("retry-audio.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&output, spec)
        .map_err(|e| format!("Cannot create the retained capture for retry: {e}"))?;
    let mut written = 0usize;
    for (index, segment) in segments.iter().enumerate() {
        let mut reader = hound::WavReader::open(segment)
            .map_err(|e| format!("Cannot read retained audio {}: {e}", segment.display()))?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.sample_rate != SAMPLE_RATE as u32
            || spec.bits_per_sample != 16
        {
            return Err(format!(
                "Retained audio {} is not the expected mono 16-bit 16 kHz window",
                segment.display()
            ));
        }
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let skip = if index == 0 { 0 } else { overlap };
        if samples.len() < skip {
            return Err(format!(
                "Retained audio {} is too short to rejoin the capture",
                segment.display()
            ));
        }
        for &sample in &samples[skip..] {
            writer
                .write_sample(sample)
                .map_err(|e| format!("Cannot rejoin retained capture audio: {e}"))?;
            written += 1;
        }
    }
    writer
        .finalize()
        .map_err(|e| format!("Cannot finalize the retained capture for retry: {e}"))?;
    if written == 0 {
        return Err("The failed capture left no audio samples to retry".into());
    }
    Ok(output)
}

pub(crate) struct RecognitionWorker {
    pub progress: Arc<Progress>,
    worker: JoinHandle<AppResult<String>>,
}
impl RecognitionWorker {
    pub fn start(
        stream: SegmentStream,
        transcriber: Arc<dyn LocalTranscriber>,
        directory: PathBuf,
    ) -> Self {
        let progress = stream.progress.clone();
        let state = progress.clone();
        let worker = thread::spawn(move || {
            // A crashing local backend is a recoverable failure, not a silent stall.
            // Report it at once so capture stops and its audio is retained.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                recognize(stream, transcriber, directory)
            }));
            let result = match outcome {
                Ok(result) => result,
                Err(payload) => {
                    let reason = payload
                        .downcast_ref::<&str>()
                        .map(|reason| reason.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    Err(format!(
                        "Local segment recognition worker panicked ({reason}); received audio and checkpoint retained"
                    ))
                }
            };
            if let Err(error) = &result {
                state.fail(error.clone());
            }
            result
        });
        Self { progress, worker }
    }
    pub fn finish(self) -> AppResult<String> {
        self.worker.join().map_err(|_| {
            "Local segment recognition worker panicked; received audio and checkpoint retained"
                .to_string()
        })?
    }
}

fn recognize(
    stream: SegmentStream,
    transcriber: Arc<dyn LocalTranscriber>,
    directory: PathBuf,
) -> AppResult<String> {
    let mut committed = String::new();
    let mut tail = String::new();
    while let Ok(segment) = stream.receiver.recv() {
        let workspace = segment.audio.path.parent().unwrap();
        let spans = transcriber.transcribe_segment(&segment.audio, workspace)?;
        // Overlapped windows share ownership at the middle of the overlap. Hold the
        // latest trailing half until another window arrives or capture actually stops.
        let begin = if segment.start_sample == 0 {
            0
        } else {
            segment.overlap_samples as u64 / 2
        };
        let end = if segment.final_segment {
            segment.samples as u64
        } else {
            segment.samples as u64 - segment.overlap_samples as u64 / 2
        };
        // A speech-free window must keep the trailing half held by the previous
        // window: that text still needs another window to claim it, or the final
        // tail to commit it. Clearing it here would erase already recognized words.
        if !spans.is_empty() {
            tail.clear();
        }
        for TimedText {
            start_ms,
            end_ms,
            text,
        } in spans
        {
            let midpoint = (start_ms + end_ms) * SAMPLE_RATE / 2000;
            if midpoint >= begin && midpoint < end {
                committed.push_str(&text);
            } else if midpoint >= end {
                tail.push_str(&text);
            }
        }
        let checkpoint = format!("{committed}{tail}");
        let staged = directory.join("partial-result.next");
        fs::write(&staged, checkpoint.trim()).map_err(|e| {
            format!("Cannot save recoverable transcript checkpoint (storage exhausted): {e}")
        })?;
        fs::rename(&staged, directory.join("partial-result.txt"))
            .map_err(|e| format!("Cannot commit recoverable transcript checkpoint: {e}"))?;
        let bytes = fs::metadata(&segment.audio.path)
            .map_err(|e| e.to_string())?
            .len();
        fs::remove_dir_all(workspace)
            .map_err(|e| format!("Cannot release recognized playback spool: {e}"))?;
        stream.progress.spool.fetch_sub(bytes, Ordering::Relaxed);
        stream.progress.pending.fetch_sub(1, Ordering::Relaxed);
        stream.progress.recognized.fetch_add(1, Ordering::Relaxed);
    }
    committed.push_str(&tail);
    Ok(committed.trim().into())
}

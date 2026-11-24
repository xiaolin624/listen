//! Append-only, bounded continuous PCM. Publish bytes only after syncing them to
//! disk; the network can fall behind without blocking or discarding the capture.
use crate::{AppResult, CapturedAudio, streaming::CaptureProgress};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

pub const PCM_CHUNK_BYTES: usize = 3_200; // 100 ms, 16 kHz mono i16

/// The small durable descriptor that lets a later process decide whether a
/// `playback.pcm` was closed normally and how many bytes were committed. It is
/// written atomically only when the recording ends, so a process that was killed
/// mid-capture leaves no marker at all and recovery must treat the file as
/// incomplete instead of presenting it as a complete recording (#35).
pub const PCM_STATE_FILE: &str = "playback.state";

#[derive(Default)]
pub(crate) struct PcmProgress {
    committed: AtomicU64,
    received: AtomicU64,
    buffered: AtomicUsize,
    complete: AtomicBool,
}
impl PcmProgress {
    pub(crate) fn snapshot(&self) -> CaptureProgress {
        CaptureProgress {
            received_samples: self.received.load(Ordering::Acquire),
            buffered_samples: self.buffered.load(Ordering::Acquire) / 2,
            spool_bytes: self.committed.load(Ordering::Acquire),
            ..CaptureProgress::default()
        }
    }
}

pub struct PcmSpoolWriter {
    file: File,
    path: PathBuf,
    limit: u64,
    buffer: Vec<u8>,
    progress: Arc<PcmProgress>,
    /// Whether the end-of-recording marker was already written. `finish` sets it
    /// so the `Drop` fallback cannot downgrade a clean stop.
    state_written: bool,
}
pub struct PcmStream {
    file: File,
    read: u64,
    pub(crate) progress: Arc<PcmProgress>,
}
impl PcmSpoolWriter {
    pub fn new(directory: &Path, limit: u64) -> AppResult<(Self, PcmStream)> {
        let path = directory.join("playback.pcm");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Cannot create retained PCM audio: {e}"))?;
        std::fs::write(
            directory.join("pcm-format.txt"),
            "sample_rate=16000\nchannels=1\nbits=16\nencoding=signed-little-endian\n",
        )
        .map_err(|e| format!("Cannot save PCM format: {e}"))?;
        let reader = File::open(&path).map_err(|e| format!("Cannot open PCM stream: {e}"))?;
        let progress = Arc::new(PcmProgress::default());
        Ok((
            Self {
                file,
                path,
                limit,
                buffer: Vec::with_capacity(PCM_CHUNK_BYTES),
                progress: progress.clone(),
                state_written: false,
            },
            PcmStream {
                file: reader,
                read: 0,
                progress,
            },
        ))
    }
    pub fn push(&mut self, sample: i16) -> AppResult<()> {
        if self
            .progress
            .committed
            .load(Ordering::Acquire)
            .saturating_add(self.buffer.len() as u64)
            .saturating_add(2)
            > self.limit
        {
            self.flush()?;
            return Err("Continuous playback storage limit reached; capture stopped and received PCM is retained".into());
        }
        self.buffer.extend_from_slice(&sample.to_le_bytes());
        self.progress.received.fetch_add(1, Ordering::Release);
        self.progress
            .buffered
            .store(self.buffer.len(), Ordering::Release);
        if self.buffer.len() >= PCM_CHUNK_BYTES {
            self.flush()?;
        }
        Ok(())
    }
    pub fn flush(&mut self) -> AppResult<()> {
        if !self.buffer.is_empty() {
            self.file
                .seek(SeekFrom::Start(
                    self.progress.committed.load(Ordering::Acquire),
                ))
                .and_then(|_| self.file.write_all(&self.buffer))
                .and_then(|()| self.file.sync_data())
                .map_err(|e| format!("Cannot persist received PCM; capture is incomplete: {e}"))?;
            self.progress
                .committed
                .fetch_add(self.buffer.len() as u64, Ordering::Release);
            self.buffer.clear();
            self.progress.buffered.store(0, Ordering::Release);
        }
        Ok(())
    }
    pub fn finish(mut self, description: String) -> AppResult<CapturedAudio> {
        self.flush()?;
        self.progress.complete.store(true, Ordering::Release);
        self.state_written = true;
        // The marker is written only after every committed chunk was synced, so
        // its byte count always describes audio that is already readable.
        let committed = self.progress.committed.load(Ordering::Acquire);
        let marker = pcm_state(committed, true);
        crate::cloud::persist_result(&self.path.with_file_name(PCM_STATE_FILE), marker.as_bytes())
            .map_err(|e| format!("Cannot save the PCM end-of-recording marker: {e}"))?;
        Ok(CapturedAudio {
            path: self.path.clone(),
            description,
        })
    }
}
impl Drop for PcmSpoolWriter {
    fn drop(&mut self) {
        let _ = self.flush();
        if self.state_written {
            return;
        }
        // A recording that ends without `finish` (the device was torn down, or the
        // process is exiting) still publishes how much audio was committed. It is
        // explicitly not a clean stop: a later process must mark it incomplete.
        let committed = self.progress.committed.load(Ordering::Acquire);
        let marker = pcm_state(committed, false);
        let _ = crate::cloud::persist_result(
            &self.path.with_file_name(PCM_STATE_FILE),
            marker.as_bytes(),
        );
    }
}

/// The exact `playback.state` contents. One key per line, so a human can read it
/// and a later version can add keys without changing this one.
pub fn pcm_state(committed_bytes: u64, clean_stop: bool) -> String {
    format!(
        "committed_bytes={committed_bytes}\nclean_stop={}\n",
        u8::from(clean_stop)
    )
}
impl PcmStream {
    /// Read an already finished continuous recording again. Used only by the
    /// explicit retry control after the session reported a failure; the complete
    /// spool is kept on disk until a result is reliably saved.
    pub fn resume(path: &Path) -> AppResult<Self> {
        let file = File::open(path)
            .map_err(|error| format!("Cannot open the retained recording: {error}"))?;
        let bytes = file
            .metadata()
            .map_err(|error| format!("Cannot inspect the retained recording: {error}"))?
            .len();
        if bytes == 0 || bytes % 2 != 0 {
            return Err(
                "The retained recording is empty or incomplete, so it cannot be recognized again"
                    .into(),
            );
        }
        let progress = Arc::new(PcmProgress::default());
        progress.committed.store(bytes, Ordering::Release);
        progress.received.store(bytes / 2, Ordering::Release);
        progress.complete.store(true, Ordering::Release);
        Ok(Self {
            file,
            read: 0,
            progress,
        })
    }
    pub(crate) fn next(&mut self) -> AppResult<Option<Vec<u8>>> {
        let bytes = (self.progress.committed.load(Ordering::Acquire) - self.read)
            .min(PCM_CHUNK_BYTES as u64) as usize;
        if bytes == 0 {
            return Ok(None);
        }
        let mut data = vec![0; bytes];
        self.file
            .read_exact(&mut data)
            .map_err(|e| format!("Cannot read retained PCM: {e}"))?;
        self.read += bytes as u64;
        Ok(Some(data))
    }
    pub(crate) fn drained(&self) -> bool {
        self.progress.complete.load(Ordering::Acquire)
            && self.read == self.progress.committed.load(Ordering::Acquire)
    }
    /// Re-read the retained spool from its beginning. A vendor without resumable
    /// sessions is rebuilt from the complete retained audio; nothing is kept in
    /// memory, so the replay is bounded by the on-disk budget.
    pub(crate) fn rewind(&mut self) -> AppResult<()> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("Cannot re-read retained PCM: {error}"))?;
        self.read = 0;
        Ok(())
    }
}

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
        Ok(CapturedAudio {
            path: self.path.clone(),
            description,
        })
    }
}
impl Drop for PcmSpoolWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
impl PcmStream {
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
}

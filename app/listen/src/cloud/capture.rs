//! Capture stop never waits for network setup, send or receive to return.
use crate::{AppResult, CapturedAudio, Recording};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(super) struct CaptureControl {
    recording: Mutex<Option<Box<dyn Recording>>>,
    stopped: AtomicBool,
    stopped_at: Mutex<Option<Instant>>,
    outcome: Mutex<Option<AppResult<CapturedAudio>>>,
    changed: Condvar,
}
impl CaptureControl {
    pub fn new(recording: Box<dyn Recording>) -> Arc<Self> {
        Arc::new(Self {
            recording: Mutex::new(Some(recording)),
            stopped: AtomicBool::new(false),
            stopped_at: Mutex::new(None),
            outcome: Mutex::new(None),
            changed: Condvar::new(),
        })
    }
    pub fn stop(self: &Arc<Self>) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        *self.stopped_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let capture = self.clone();
        thread::spawn(move || {
            let recording = capture
                .recording
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let result = recording
                .ok_or_else(|| {
                    "Capture ended without a recording; received PCM is incomplete".into()
                })
                .and_then(|recording| recording.finish());
            *capture.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            capture.changed.notify_all();
        });
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn stopped_at(&self) -> Option<Instant> {
        *self.stopped_at.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn failure(&self) -> Option<String> {
        if self.is_stopped() {
            return None;
        }
        self.recording
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|recording| recording.failure())
    }
    pub fn outcome(&self) -> Option<AppResult<CapturedAudio>> {
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn wait(&self) -> AppResult<CapturedAudio> {
        let mut result = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("Capture did not finish within its bounded stop wait; received PCM is retained and incomplete".into());
            }
            result = self
                .changed
                .wait_timeout(result, deadline.duration_since(now))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

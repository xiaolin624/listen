//! WASAPI render-endpoint loopback, streamed to disk on a dedicated capture thread.
//! No microphone/capture endpoint API is used.
use crate::{AppResult, CapturedAudio, PlaybackAudio, Recording};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Media::Audio::{
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
            IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEXTENSIBLE,
            eMultimedia, eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize,
        },
    },
    core::GUID,
};

pub struct WasapiPlayback;
struct WasapiRecording {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<AppResult<CapturedAudio>>>,
}

impl PlaybackAudio for WasapiPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let path = directory.join("playback.wav");
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = capture(&path, signal, &tx);
            if let Err(error) = &result {
                let _ = tx.try_send(Err(error.clone()));
            }
            result
        });
        match rx.recv().map_err(|_| {
            "Playback capture thread stopped during device initialization".to_string()
        })? {
            Ok(()) => Ok(Box::new(WasapiRecording {
                stop,
                worker: Some(worker),
            })),
            Err(error) => {
                let _ = worker.join();
                Err(format!("Cannot open the default playback device: {error}"))
            }
        }
    }
}

impl Recording for WasapiRecording {
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().map_err(|_| {
            "Playback capture thread panicked; partial audio was retained".to_string()
        })?
    }

    fn failure(&self) -> Option<String> {
        self.worker
            .as_ref()
            .filter(|worker| worker.is_finished())
            .map(|_| "Playback capture was interrupted".into())
    }
}

impl Drop for WasapiRecording {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct Resampler {
    step: f64,
    next: f64,
    index: u64,
    previous: f32,
}
impl Resampler {
    fn new(rate: u32) -> Self {
        Self {
            step: rate as f64 / 16_000.0,
            next: 0.0,
            index: 0,
            previous: 0.0,
        }
    }
    fn push(&mut self, value: f32, output: &mut impl FnMut(i16) -> AppResult<()>) -> AppResult<()> {
        if self.index == 0 {
            output((value.clamp(-1.0, 1.0) * 32767.0) as i16)?;
            self.next = self.step;
        } else {
            while self.next <= self.index as f64 {
                let fraction = (self.next - (self.index - 1) as f64) as f32;
                let interpolated = self.previous + (value - self.previous) * fraction;
                output((interpolated.clamp(-1.0, 1.0) * 32767.0) as i16)?;
                self.next += self.step;
            }
        }
        self.previous = value;
        self.index += 1;
        Ok(())
    }
}

fn capture(
    path: &PathBuf,
    stop: Arc<AtomicBool>,
    started: &mpsc::SyncSender<AppResult<()>>,
) -> AppResult<CapturedAudio> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let _com = Com;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| e.to_string())?;
        let id_ptr = device.GetId().map_err(|e| e.to_string())?;
        let endpoint = id_ptr.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(id_ptr.0.cast()));
        let endpoint = endpoint?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| e.to_string())?;
        let ptr = client.GetMixFormat().map_err(|e| e.to_string())?;
        let format = *ptr;
        let rate = format.nSamplesPerSec;
        let channels = format.nChannels as usize;
        let bits = format.wBitsPerSample;
        let block = format.nBlockAlign as usize;
        let tag = format.wFormatTag;
        let subtype = if tag == 0xfffe && format.cbSize >= 22 {
            Some((*ptr.cast::<WAVEFORMATEXTENSIBLE>()).SubFormat)
        } else {
            None
        };
        let float =
            tag == 3 || subtype == Some(GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71));
        let pcm =
            tag == 1 || subtype == Some(GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71));
        let supported = channels > 0
            && rate > 0
            && block >= channels * (bits as usize / 8)
            && ((float && bits == 32) || (pcm && matches!(bits, 16 | 24 | 32)));
        if !supported {
            CoTaskMemFree(Some(ptr.cast()));
            return Err(format!(
                "Unsupported playback mix format: tag={tag}, {rate} Hz, {channels} channels, {bits} bits"
            ));
        }
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000,
            0,
            ptr,
            None,
        );
        CoTaskMemFree(Some(ptr.cast()));
        init.map_err(|e| e.to_string())?;
        let reader: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec)
            .map_err(|e| format!("Cannot create temporary playback audio: {e}"))?;
        let mut resampler = Resampler::new(rate);
        let mut frames = 0u64;
        let mut discontinuities = 0u64;
        let mut peak = 0f32;
        let mut flushed = Instant::now();
        client.Start().map_err(|e| e.to_string())?;
        let _ = started.send(Ok(()));
        let result = (|| -> AppResult<()> {
            loop {
                while reader
                    .GetNextPacketSize()
                    .map_err(|e| format!("Playback device interrupted: {e}"))?
                    > 0
                {
                    let mut data = std::ptr::null_mut();
                    let mut count = 0;
                    let mut flags = 0;
                    reader
                        .GetBuffer(&mut data, &mut count, &mut flags, None, None)
                        .map_err(|e| format!("Cannot read playback audio: {e}"))?;
                    if flags & 1 != 0 {
                        discontinuities += 1;
                    }
                    let packet_result = (|| -> AppResult<()> {
                        let silent = flags & 2 != 0;
                        if !silent && data.is_null() {
                            return Err(
                                "Playback device returned an empty non-silent buffer".into()
                            );
                        }
                        for frame in 0..count as usize {
                            let mut mono = 0f32;
                            for channel in 0..channels {
                                if !silent {
                                    let sample =
                                        data.add(frame * block + channel * (bits as usize / 8));
                                    mono += if float {
                                        std::ptr::read_unaligned(sample.cast::<f32>())
                                    } else if bits == 16 {
                                        std::ptr::read_unaligned(sample.cast::<i16>()) as f32
                                            / 32768.0
                                    } else if bits == 24 {
                                        i32::from_le_bytes([
                                            0,
                                            *sample,
                                            *sample.add(1),
                                            *sample.add(2),
                                        ]) as f32
                                            / 2147483648.0
                                    } else {
                                        std::ptr::read_unaligned(sample.cast::<i32>()) as f32
                                            / 2147483648.0
                                    };
                                }
                            }
                            mono /= channels as f32;
                            if !mono.is_finite() {
                                mono = 0.0;
                            }
                            peak = peak.max(mono.abs());
                            frames += 1;
                            resampler.push(mono, &mut |sample| {
                                // WAV's u32 length is a storage-format limit, never a fixed capture timer.
                                if writer.len() >= (u32::MAX - 128) / 2 {
                                    return Err("Temporary WAV storage capacity exhausted; captured audio was retained".into());
                                }
                                writer.write_sample(sample).map_err(|e| format!("Cannot write temporary playback audio: {e}"))
                            })?;
                        }
                        Ok(())
                    })();
                    let released = reader.ReleaseBuffer(count).map_err(|e| e.to_string());
                    packet_result?;
                    released?;
                }
                if flushed.elapsed() >= Duration::from_secs(1) {
                    writer
                        .flush()
                        .map_err(|e| format!("Cannot flush temporary playback audio: {e}"))?;
                    flushed = Instant::now();
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        let stopped = client.Stop().map_err(|e| e.to_string());
        let finalized = writer
            .finalize()
            .map_err(|e| format!("Cannot finalize retained playback audio: {e}"));
        result?;
        stopped?;
        finalized?;
        // Silence and an idle render device are valid capture results. Only real
        // device/packet/storage errors above are failures; speech detection owns
        // the no-speech outcome and must not lose quiet valid speech here.
        Ok(CapturedAudio {
            path: path.clone(),
            description: format!(
                "default-render/multimedia {endpoint}; {:.3} s, {rate} Hz/{channels} ch/{bits} bit; peak={peak:.5}; discontinuities={discontinuities}",
                frames as f64 / rate as f64
            ),
        })
    }
}

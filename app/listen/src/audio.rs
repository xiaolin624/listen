//! WASAPI render-endpoint loopback, streamed to disk on a dedicated capture thread.
//! No microphone/capture endpoint API is used.
use crate::streaming::{
    CaptureProgress, Progress, SegmentConfig, SegmentStream, SegmentedAudioWriter,
};
use crate::{AppResult, CaptureEndpoint, CapturedAudio, PlaybackAudio, Recording};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEXTENSIBLE, eMultimedia, eRender,
        },
        System::Com::StructuredStorage::{PropVariantClear, PropVariantToBSTR},
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize, STGM_READ,
        },
    },
    core::GUID,
};

/// The OS role of the single render endpoint this tool captures. The role is
/// reported to the user verbatim so that a communications endpoint or another
/// device is never implied to be part of the capture.
const ENDPOINT_ROLE: &str = "default multimedia render endpoint";
/// Polling interval for the OS default endpoint. WASAPI raises
/// `AUDCLNT_E_DEVICE_INVALIDATED` immediately for a removed client, while a route
/// switch is detected by comparing this identity.
const ROUTE_CHECK_INTERVAL: Duration = Duration::from_millis(250);

pub struct WasapiPlayback;
struct WasapiRecording {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<AppResult<CapturedAudio>>>,
    segments: Option<SegmentStream>,
    progress: Arc<Progress>,
    endpoint: CaptureEndpoint,
    last_error: Arc<Mutex<Option<String>>>,
}

impl PlaybackAudio for WasapiPlayback {
    fn start(&self, directory: &Path) -> AppResult<Box<dyn Recording>> {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let (writer, segments) = SegmentedAudioWriter::new(directory, SegmentConfig::default())?;
        let progress = segments.progress.clone();
        let last_error = Arc::new(Mutex::new(None));
        let capture_error = last_error.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = capture(writer, signal, &tx, capture_error.clone());
            if let Err(error) = &result {
                let _ = tx.try_send(Err(error.clone()));
            }
            result
        });
        match rx.recv().map_err(|_| {
            "Playback capture thread stopped during device initialization".to_string()
        })? {
            Ok(endpoint) => Ok(Box::new(WasapiRecording {
                stop,
                worker: Some(worker),
                segments: Some(segments),
                progress,
                endpoint,
                last_error,
            })),
            Err(error) => {
                let _ = worker.join();
                Err(format!(
                    "Cannot open the default multimedia render endpoint: {error}"
                ))
            }
        }
    }
}

impl Recording for WasapiRecording {
    fn take_segments(&mut self) -> Option<SegmentStream> {
        self.segments.take()
    }
    fn progress(&self) -> Option<CaptureProgress> {
        Some(self.progress.snapshot())
    }
    fn finish(mut self: Box<Self>) -> AppResult<CapturedAudio> {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().map_err(|_| {
            "Playback capture thread panicked; partial audio was retained".to_string()
        })?
    }

    fn failure(&self) -> Option<String> {
        if let Some(error) = self.last_error.lock().ok().and_then(|error| error.clone()) {
            return Some(error);
        }
        self.progress.failure().or_else(|| {
            self.worker
                .as_ref()
                .filter(|worker| worker.is_finished())
                .map(|_| {
                    format!(
                        "Playback capture from the {} '{}' ({}) was interrupted; received audio is retained and this capture is incomplete",
                        self.endpoint.role, self.endpoint.name, self.endpoint.id
                    )
                })
        })
    }

    fn endpoint(&self) -> Option<CaptureEndpoint> {
        Some(self.endpoint.clone())
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

/// Read the endpoint identity the user is actually routed to. The device id is
/// required so a default-route switch can be detected; the friendly name is best
/// effort and never blocks capture by itself.
fn endpoint_identity(device: &IMMDevice) -> AppResult<CaptureEndpoint> {
    unsafe {
        let id_ptr = device.GetId().map_err(|e| e.to_string())?;
        let id = id_ptr.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(id_ptr.0.cast()));
        let id = id?;
        let name = (|| -> Option<String> {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let name = PropVariantToBSTR(&value).ok().map(|name| name.to_string());
            let _ = PropVariantClear(&mut value);
            name
        })()
        .unwrap_or_else(|| "unknown endpoint name".into());
        Ok(CaptureEndpoint {
            role: ENDPOINT_ROLE.into(),
            id,
            name,
        })
    }
}

/// Turns a WASAPI/COM failure into the exact interruption the user sees. A removed
/// endpoint is named; the message never claims that the partial capture is complete.
fn capture_error(
    endpoint: &CaptureEndpoint,
    operation: &str,
    error: &windows::core::Error,
) -> String {
    let reason = if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        "was removed or became invalid (AUDCLNT_E_DEVICE_INVALIDATED)".to_string()
    } else if error.code() == AUDCLNT_E_SERVICE_NOT_RUNNING {
        "lost the Windows audio service (AUDCLNT_E_SERVICE_NOT_RUNNING)".to_string()
    } else {
        format!("could not be read while {operation} ({error})")
    };
    format!(
        "The playback endpoint '{}' ({}) {reason}. Capture stopped; received audio is retained and this capture is incomplete.",
        endpoint.name, endpoint.id
    )
}

fn route_changed_message(previous: &CaptureEndpoint, current: &CaptureEndpoint) -> String {
    format!(
        "The default multimedia render endpoint changed during capture from '{}' ({}) to '{}' ({}). Capture stopped before audio from the new endpoint was mixed in; received audio is retained and this capture is incomplete.",
        previous.name, previous.id, current.name, current.id
    )
}

fn capture(
    mut writer: SegmentedAudioWriter,
    stop: Arc<AtomicBool>,
    started: &mpsc::SyncSender<AppResult<CaptureEndpoint>>,
    last_error: Arc<Mutex<Option<String>>>,
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
            .map_err(|e| format!("Cannot select the {ENDPOINT_ROLE}: {e}"))?;
        let endpoint = endpoint_identity(&device)?;
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
        let mut resampler = Resampler::new(rate);
        let mut frames = 0u64;
        let mut discontinuities = 0u64;
        let mut peak = 0f32;
        let mut flushed = Instant::now();
        let mut route_checked = Instant::now();
        client
            .Start()
            .map_err(|e| capture_error(&endpoint, "starting playback capture", &e))?;
        let _ = started.send(Ok(endpoint.clone()));
        let result = (|| -> AppResult<()> {
            loop {
                while reader
                    .GetNextPacketSize()
                    .map_err(|e| capture_error(&endpoint, "reading packet size", &e))?
                    > 0
                {
                    let mut data = std::ptr::null_mut();
                    let mut count = 0;
                    let mut flags = 0;
                    reader
                        .GetBuffer(&mut data, &mut count, &mut flags, None, None)
                        .map_err(|e| capture_error(&endpoint, "reading playback audio", &e))?;
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
                            resampler.push(mono, &mut |sample| writer.push(sample))?;
                        }
                        Ok(())
                    })();
                    let released = reader
                        .ReleaseBuffer(count)
                        .map_err(|e| capture_error(&endpoint, "releasing playback audio", &e));
                    packet_result?;
                    released?;
                }
                if flushed.elapsed() >= Duration::from_secs(1) {
                    writer.flush()?;
                    flushed = Instant::now();
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                // A default-route switch leaves the old client alive in WASAPI, so
                // packet errors alone would keep reporting the old endpoint as if it
                // were still the user's route. Compare the OS default identity.
                if route_checked.elapsed() >= ROUTE_CHECK_INTERVAL {
                    route_checked = Instant::now();
                    let current = enumerator
                        .GetDefaultAudioEndpoint(eRender, eMultimedia)
                        .map_err(|error| {
                            format!(
                                "The default multimedia render endpoint '{}' ({}) could not be confirmed during capture ({error}); the route may have been removed or changed. Capture stopped; received audio is retained and this capture is incomplete.",
                                endpoint.name, endpoint.id
                            )
                        })?;
                    let current = endpoint_identity(&current).map_err(|error| {
                        format!(
                            "The default multimedia render endpoint changed during capture but the new endpoint could not be identified ({error}); capture stopped; received audio is retained and this capture is incomplete."
                        )
                    })?;
                    if current.id != endpoint.id {
                        return Err(route_changed_message(&endpoint, &current));
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        if let Err(error) = &result {
            if let Ok(mut last) = last_error.lock() {
                *last = Some(error.clone());
            }
        }
        let stopped = client
            .Stop()
            .map_err(|e| capture_error(&endpoint, "stopping playback capture", &e));
        let finalized = writer.finish(format!(
                "default multimedia render endpoint '{}' ({}); audio routed only to the default communication endpoint is not included; {:.3} s, {rate} Hz/{channels} ch/{bits} bit; peak={peak:.5}; discontinuities={discontinuities}",
                endpoint.name, endpoint.id, frames as f64 / rate as f64
            ));
        result?;
        stopped?;
        // Silence and an idle render device are valid capture results. Only real
        // device/packet/storage errors above are failures; speech detection owns
        // the no-speech outcome and must not lose quiet valid speech here.
        finalized
    }
}

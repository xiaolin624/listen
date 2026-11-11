//! Disposable native Windows feasibility probe, not the production application.
//! Never automatically sends a message or writes into an existing user's editor.

use std::{
    env,
    error::Error,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Media::Audio::{
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
            IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, PlaySoundW, SND_FILENAME,
            SND_SYNC, WAVEFORMATEXTENSIBLE, eMultimedia, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
                UIA_ValuePatternId,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{BSTR, GUID, PCWSTR, w},
};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

struct Com;
impl Com {
    fn enter() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn main() -> ProbeResult<()> {
    let args: Vec<String> = env::args().collect();
    let _com = Com::enter()?;
    match args.get(1).map(String::as_str) {
        Some("audio") => {
            let seconds: f64 = args.get(2).ok_or("missing duration")?.parse()?;
            if !seconds.is_finite() || seconds <= 0.0 { return Err("duration must be positive".into()); }
            let output = args.get(3).ok_or("missing output WAV path")?;
            let playback = args.get(4).map(|s| s.to_owned());
            capture(seconds, Path::new(output), playback)?;
        }
        Some("target-self-test") => target_self_test()?,
        _ => return Err("usage: listen-windows-probe audio <seconds> <output.wav> [test-playback.wav] | target-self-test".into()),
    }
    Ok(())
}

struct Resampler {
    step: f64,
    next: f64,
    index: u64,
    previous: f32,
}

impl Resampler {
    fn new(input_rate: u32) -> Self {
        Self {
            step: input_rate as f64 / 16_000.0,
            next: 0.0,
            index: 0,
            previous: 0.0,
        }
    }

    fn push(
        &mut self,
        value: f32,
        output: &mut impl FnMut(i16) -> ProbeResult<()>,
    ) -> ProbeResult<()> {
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

fn capture(seconds: f64, output: &Path, playback: Option<String>) -> ProbeResult<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format_ptr = client.GetMixFormat()?;
        let format = *format_ptr;
        let sample_rate = format.nSamplesPerSec;
        let channels = format.nChannels as usize;
        let bits = format.wBitsPerSample;
        let block_align = format.nBlockAlign as usize;
        let tag = format.wFormatTag;
        let float_format = tag == 3
            || (tag == 0xfffe && {
                let sub_format = (*format_ptr.cast::<WAVEFORMATEXTENSIBLE>()).SubFormat;
                sub_format == GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)
            });
        if channels == 0 || sample_rate == 0 || !(bits == 16 || bits == 24 || bits == 32) {
            CoTaskMemFree(Some(format_ptr.cast()));
            return Err("unsupported playback mix format".into());
        }
        if float_format && bits != 32 {
            CoTaskMemFree(Some(format_ptr.cast()));
            return Err("unsupported float mix format".into());
        }
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000,
            0,
            format_ptr,
            None,
        );
        CoTaskMemFree(Some(format_ptr.cast()));
        init?;
        let capture: IAudioCaptureClient = client.GetService()?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(output, spec)?;
        let mut resampler = Resampler::new(sample_rate);
        let mut frames_seen = 0u64;
        let mut squares = 0f64;
        let mut peak = 0f32;
        let mut discontinuities = 0u64;
        println!(
            "capture: endpoint=default-render role=multimedia sample_rate={sample_rate} channels={channels} bits={bits} float={float_format}"
        );
        client.Start()?;
        let playback_thread = playback.map(|path| {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
                PlaySoundW(PCWSTR(wide.as_ptr()), None, SND_FILENAME | SND_SYNC).as_bool()
            })
        });
        let start = Instant::now();
        let result = (|| -> ProbeResult<()> {
            while start.elapsed().as_secs_f64() < seconds {
                while capture.GetNextPacketSize()? > 0 {
                    let mut data = std::ptr::null_mut();
                    let mut count = 0;
                    let mut flags = 0;
                    capture.GetBuffer(&mut data, &mut count, &mut flags, None, None)?;
                    if flags & 1 != 0 {
                        discontinuities += 1;
                    }
                    let write_result = (|| -> ProbeResult<()> {
                        let silent = flags & 2 != 0;
                        if !silent && data.is_null() {
                            return Err("null non-silent audio packet".into());
                        }
                        for frame in 0..count as usize {
                            let mut mono = 0f32;
                            for channel in 0..channels {
                                if !silent {
                                    let ptr = data
                                        .add(frame * block_align + channel * (bits as usize / 8));
                                    mono += if float_format {
                                        std::ptr::read_unaligned(ptr.cast::<f32>())
                                    } else if bits == 16 {
                                        std::ptr::read_unaligned(ptr.cast::<i16>()) as f32 / 32768.0
                                    } else if bits == 24 {
                                        let signed =
                                            i32::from_le_bytes([0, *ptr, *ptr.add(1), *ptr.add(2)]);
                                        signed as f32 / 2147483648.0
                                    } else {
                                        std::ptr::read_unaligned(ptr.cast::<i32>()) as f32
                                            / 2147483648.0
                                    };
                                }
                            }
                            mono /= channels as f32;
                            if !mono.is_finite() {
                                mono = 0.0;
                            }
                            squares += (mono as f64).powi(2);
                            peak = peak.max(mono.abs());
                            frames_seen += 1;
                            resampler.push(mono, &mut |sample| {
                                writer.write_sample(sample)?;
                                Ok(())
                            })?;
                        }
                        Ok(())
                    })();
                    capture.ReleaseBuffer(count)?;
                    write_result?;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        client.Stop()?;
        writer.finalize()?;
        if let Some(join) = playback_thread {
            if !join.join().unwrap_or(false) {
                return Err("test playback failed".into());
            }
        }
        result?;
        let duration = frames_seen as f64 / sample_rate as f64;
        let rms = if frames_seen == 0 {
            0.0
        } else {
            (squares / frames_seen as f64).sqrt()
        };
        println!(
            "capture_result: duration_seconds={duration:.3} frames={frames_seen} rms={rms:.6} peak={peak:.6} discontinuities={discontinuities}"
        );
        println!("output: {}", output.display());
        if frames_seen == 0 || peak < 0.00001 {
            return Err(
                "no non-silent playback captured; verify the actual headphone playback endpoint"
                    .into(),
            );
        }
    }
    Ok(())
}

unsafe fn append_to_target(target: &IUIAutomationElement, text: &str) -> ProbeResult<String> {
    unsafe {
        if !target.CurrentIsEnabled()?.as_bool() {
            return Err("target is disabled".into());
        }
        let pattern: IUIAutomationValuePattern = target.GetCurrentPatternAs(UIA_ValuePatternId)?;
        if pattern.CurrentIsReadOnly()?.as_bool() {
            return Err("target is read-only".into());
        }
        let existing = pattern.CurrentValue()?.to_string();
        let combined = if existing.is_empty() {
            text.to_owned()
        } else {
            format!("{existing}\r\n{text}")
        };
        pattern.SetValue(&BSTR::from(combined.as_str()))?;
        let observed = pattern.CurrentValue()?.to_string();
        if observed.replace("\r\n", "\n") != combined.replace("\r\n", "\n") {
            return Err("write could not be confirmed; use the fallback text box without retrying automatically".into());
        }
        Ok(observed)
    }
}

unsafe extern "system" fn probe_window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        if message == WM_APP + 1 {
            let _ = DestroyWindow(HWND(wparam.0 as *mut _));
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn target_self_test() -> ProbeResult<()> {
    let (sender, receiver) = mpsc::channel();
    let gui = thread::spawn(move || -> Result<(), String> {
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(probe_window),
                hInstance: instance.into(),
                lpszClassName: w!("ListenProbeOnly"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err("could not register probe window".to_owned());
            }
            let parent = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("ListenProbeOnly"),
                w!("Listen probe fixture"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                400,
                240,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            let target = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!("原有文字"),
                WS_CHILD | WINDOW_STYLE(0x0004),
                0,
                0,
                300,
                80,
                Some(parent),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            let other = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!("其他输入框"),
                WS_CHILD,
                0,
                90,
                300,
                40,
                Some(parent),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            sender
                .send((parent.0 as usize, target.0 as usize, other.0 as usize))
                .map_err(|e| e.to_string())?;
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    });
    let (parent, first, second) = receiver.recv_timeout(Duration::from_secs(10))?;
    let parent = HWND(parent as *mut _);
    let first = HWND(first as *mut _);
    let second = HWND(second as *mut _);
    let result = (|| -> ProbeResult<()> {
        unsafe {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
            let saved_target = automation.ElementFromHandle(first)?;
            let other_target = automation.ElementFromHandle(second)?;
            SetWindowTextW(first, w!("原有文字\r\n采集中新增的编辑"))?;
            let text = append_to_target(&saved_target, "测试转写：中文 English 123。")?;
            if !text.contains("采集中新增的编辑") || !text.ends_with("测试转写：中文 English 123。")
            {
                return Err("append did not preserve the latest contents".into());
            }
            let other_pattern: IUIAutomationValuePattern =
                other_target.GetCurrentPatternAs(UIA_ValuePatternId)?;
            if other_pattern.CurrentValue()?.to_string() != "其他输入框" {
                return Err("wrong target changed".into());
            }
            println!(
                "PASS: saved native target; Unicode append; preserved edits made during capture; other input unchanged"
            );
            PostMessageW(
                Some(parent),
                WM_APP + 1,
                WPARAM(first.0 as usize),
                LPARAM(0),
            )?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while IsWindow(Some(first)).as_bool() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if append_to_target(&saved_target, "must not be inserted").is_ok() {
                return Err("destroyed target should fall back".into());
            }
            println!(
                "PASS: destroyed target rejected; production caller must show the fallback text box"
            );
        }
        Ok(())
    })();
    unsafe {
        let _ = PostMessageW(Some(parent), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    gui.join()
        .map_err(|_| "probe GUI thread panicked")?
        .map_err(|e| -> Box<dyn Error> { e.into() })?;
    result
}

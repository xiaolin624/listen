use crate::{
    Config, WasapiPlayback,
    capsule::{CapsuleState, capsule_state, preparation_name},
    capsule_window::{self, Capsule},
};
use listen::{Application, Control, Delivery, Phase, Snapshot, target::NativeTargets};
use std::{cell::RefCell, fs, path::PathBuf, sync::Arc};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            },
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

/// The hidden owner window. It receives `--control`, hotkey and tray messages; the
/// user-visible surface is the status capsule only. The class name is private to
/// this implementation: scripts drive Listen through `--control` and `status.txt`.
const HOST_CLASS: PCWSTR = w!("ListenHostWindowV2");
const CONTROL_MESSAGE: u32 = WM_APP + 10;
const TRAY_MESSAGE: u32 = WM_APP + 11;
const ID_TOGGLE: usize = 101;
const ID_EXIT: usize = 103;
const ID_SHOW: usize = 104;
const ID_RETRY: usize = 105;
const ID_CLEANUP: usize = 106;
/// How often the application snapshot is polled and `status.txt` is refreshed.
const POLL_TIMER_MS: u32 = 100;

struct Ui {
    app: Application,
    /// Hidden owner window; the capsule has its own top-level window.
    hwnd: HWND,
    capsule: Capsule,
    runtime: PathBuf,
    last: Option<Snapshot>,
    tray: bool,
    /// `--hidden` still shows the capsule (it is the only user-visible surface) but
    /// suppresses transient tray notifications.
    notify: bool,
    quit_requested: bool,
}
thread_local! { static UI: RefCell<Option<Ui>> = const { RefCell::new(None) }; }

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn fixed<const N: usize>(target: &mut [u16; N], value: &str) {
    for (index, code) in value.encode_utf16().take(N - 1).enumerate() {
        target[index] = code;
    }
}

pub fn show_error(error: &str) {
    show_message(error, MB_ICONERROR);
}

pub fn show_help(help: &str) {
    show_message(help, MB_ICONINFORMATION);
}

fn show_message(message: &str, icon: MESSAGEBOX_STYLE) {
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(wide(message).as_ptr()),
            w!("Listen"),
            MB_OK | icon,
        );
    }
}

pub fn control(command: &str) -> Result<(), String> {
    let id = match command {
        "start" => 1,
        "stop" => 2,
        "toggle" => 3,
        "show" => 4,
        "quit" => 5,
        "retry" => 6,
        "cleanup" => 7,
        _ => return Err(format!("Unknown control command: {command}")),
    };
    unsafe {
        let hwnd =
            FindWindowW(HOST_CLASS, None).map_err(|_| "Listen is not running".to_string())?;
        PostMessageW(Some(hwnd), CONTROL_MESSAGE, WPARAM(id), LPARAM(0))
            .map_err(|e| format!("Cannot contact running Listen: {e}"))
    }
}

pub fn run(config: Config) -> Result<(), String> {
    // Per-monitor DPI awareness must be set before any window exists.
    capsule_window::enable_dpi_awareness();
    unsafe {
        if FindWindowW(HOST_CLASS, None).is_ok() {
            return Err(
                "Listen is already running. Exit it from the tray before changing configuration."
                    .into(),
            );
        }
        fs::create_dir_all(&config.runtime).map_err(|e| {
            format!(
                "Cannot create runtime directory {}: {e}",
                config.runtime.display()
            )
        })?;
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: HOST_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Cannot register Listen's window class".into());
        }
        // A tiny off-screen owner window: it makes the tray menu behave like a
        // normal foreground menu, and it never appears in the taskbar. Showing it
        // does not activate it and nothing is ever painted into it.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            HOST_CLASS,
            w!("Listen"),
            WS_POPUP,
            -32000,
            -32000,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let registration = RegisterHotKey(Some(hwnd), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'R' as u32).map_err(|e| format!("Ctrl+Alt+R: {e}. Release this shortcut in the other application, then restart Listen."));

        // The capsule exists before the model starts preparing, so the measured
        // startup budget only covers window creation and the first frame. Its first
        // state is deliberately "not ready yet" rather than a false "ready".
        let capsule = capsule_window::create(&config.runtime, CapsuleState::Preparing)?;

        let app = Application::new(
            Arc::new(WasapiPlayback),
            config.transcriber(),
            config.runtime.clone(),
            registration,
        )
        .with_targets(Arc::new(NativeTargets));
        capsule.set_state(capsule_state(&app.snapshot()));

        let mut state = Ui {
            app,
            hwnd,
            capsule,
            runtime: config.runtime,
            last: None,
            tray: false,
            notify: !config.hidden,
            quit_requested: false,
        };
        let mut icon = state.icon_data();
        icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon.uCallbackMessage = TRAY_MESSAGE;
        state.tray = Shell_NotifyIconW(NIM_ADD, &icon).as_bool();
        UI.with(|ui| *ui.borrow_mut() = Some(state));
        let registered = UI.with(|ui| {
            let mut slot = ui.borrow_mut();
            if slot.as_mut().is_none() {
                return false;
            }
            SetTimer(Some(hwnd), 1, POLL_TIMER_MS, None) != 0
        });
        if !registered {
            return Err("Cannot start the application status timer".into());
        }
        UI.with(|ui| {
            if let Some(state) = ui.borrow_mut().as_mut() {
                state.refresh();
            }
        });
        let mut message = MSG::default();
        loop {
            let next = GetMessageW(&mut message, None, 0, 0).0;
            if next == -1 {
                return Err("Listen Windows message loop failed".into());
            }
            if next == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = UnregisterHotKey(Some(hwnd), 1);
        UI.with(|ui| {
            if let Some(state) = ui.borrow_mut().take() {
                if state.tray {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &state.icon_data());
                }
                // Dropping the state destroys the capsule window.
            }
        });
        Ok(())
    }
}

impl Ui {
    fn icon_data(&self) -> NOTIFYICONDATAW {
        let state = self.app.snapshot();
        let name = format!("Listen — {:?} — Ctrl+Alt+R", state.phase);
        let icon_id = match state.phase {
            Phase::Failure => IDI_ERROR,
            Phase::Capturing => IDI_WARNING,
            Phase::Processing | Phase::Preparing => IDI_INFORMATION,
            _ => IDI_APPLICATION,
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            hIcon: unsafe { LoadIconW(None, icon_id).unwrap_or_default() },
            ..Default::default()
        };
        fixed(&mut data.szTip, &name);
        data
    }

    unsafe fn refresh(&mut self) {
        unsafe {
            self.app.poll();
            let state = self.app.snapshot();
            let visual = capsule_state(&state);
            // The capsule draws itself; this only pushes a changed visual state.
            self.capsule.set_state(visual);
            let changed = self.last.as_ref().is_none_or(|last| {
                last.phase != state.phase
                    || last.preparation != state.preparation
                    || last.detail != state.detail
                    || last.text != state.text
                    || last.retained_audio != state.retained_audio
                    || last.delivery != state.delivery
                    || last.retry_available != state.retry_available
                    || last.backend != state.backend
                    || last.timing != state.timing
            });
            if !changed {
                return;
            }
            let phase_changed = self
                .last
                .as_ref()
                .is_some_and(|last| last.phase != state.phase);
            let timing = &state.timing;
            let status = format!(
                "phase={:?}\ndetail={}\nendpoint_role={}\nendpoint_id={}\nendpoint_name={}\ncurrent_result={}\nretained_audio={}\nretry_available={}\nbackend={}\npreparation={}\ncapsule={}\nstartup_capsule_ms={}\ncapsule_capture_excluded={}\ntiming_preparation_ms={}\ntiming_stop_to_result_ms={}\ntiming_finalize_ms={}\ntiming_recognition_ms={}\ntiming_last_window_wait_ms={}\ntiming_last_window_inference_ms={}\ntiming_save_ms={}\ntiming_delivery_ms={}\ntiming_recognition_windows={}\nprocess_id={}\n",
                state.phase,
                state.detail,
                state
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.role.as_str())
                    .unwrap_or(""),
                state
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.id.as_str())
                    .unwrap_or(""),
                state
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.name.as_str())
                    .unwrap_or(""),
                self.runtime.join("current-result.txt").display(),
                state
                    .retained_audio
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
                state.retry_available,
                state.backend.as_deref().unwrap_or(""),
                preparation_name(state.preparation),
                visual.name(),
                self.capsule.startup_ms(),
                if self.capsule.capture_excluded() {
                    1
                } else {
                    0
                },
                timing
                    .preparation_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .stop_to_result_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .finalize_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .recognition_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .last_window_wait_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .last_window_inference_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .save_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing
                    .delivery_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                timing.recognition_windows,
                std::process::id()
            );
            let _ = fs::write(self.runtime.join("status.txt"), status);
            if self.tray {
                let mut icon = self.icon_data();
                icon.uFlags = NIF_TIP | NIF_ICON;
                if phase_changed && self.notify {
                    icon.uFlags |= NIF_INFO;
                    icon.dwInfoFlags = if state.phase == Phase::Failure {
                        NIIF_ERROR
                    } else {
                        NIIF_INFO
                    };
                    fixed(
                        &mut icon.szInfoTitle,
                        &format!("Listen — {:?}", state.phase),
                    );
                    fixed(
                        &mut icon.szInfo,
                        match state.phase {
                            Phase::Capturing => {
                                "Capturing the default multimedia playback endpoint. Ctrl+Alt+R stops."
                            }
                            Phase::Processing => {
                                "Recognizing locally. Wait for the current result."
                            }
                            Phase::Preparing => {
                                "Preparing the local model. Capture can still start now."
                            }
                            Phase::Result => {
                                if state.delivery == Some(Delivery::Appended) {
                                    "Appended to the original target. Review and send manually."
                                } else {
                                    "Check the original target. The last result is kept for review."
                                }
                            }
                            Phase::NoSpeech => {
                                "No valid speech detected. Nothing was delivered; the previous result is unchanged."
                            }
                            Phase::Failure => {
                                if state.retry_available {
                                    "Recognition failed. The captured audio is retained; retry it locally from the tray menu."
                                } else {
                                    "The operation failed. Retry or clean the retained audio from the tray menu."
                                }
                            }
                            _ => "Ready. Ctrl+Alt+R starts playback capture.",
                        },
                    );
                }
                let _ = Shell_NotifyIconW(NIM_MODIFY, &icon);
            }
            self.last = Some(state.clone());
            if self.quit_requested && !matches!(state.phase, Phase::Capturing | Phase::Processing) {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }

    unsafe fn command(&mut self, id: usize) {
        unsafe {
            match id {
                1 => self.app.dispatch(Control::Start),
                2 => self.app.dispatch(Control::Stop),
                3 | ID_TOGGLE => self.app.dispatch(Control::Toggle),
                6 | ID_RETRY => self.app.dispatch(Control::Retry),
                7 | ID_CLEANUP => self.app.dispatch(Control::CleanupAudio),
                // There is no panel yet (#20): reveal the capsule without focus.
                4 | ID_SHOW => self.capsule.reveal(),
                5 | ID_EXIT => {
                    self.quit_requested = true;
                    if self.app.snapshot().phase == Phase::Capturing {
                        self.app.dispatch(Control::Stop);
                    }
                    if !matches!(
                        self.app.snapshot().phase,
                        Phase::Capturing | Phase::Processing
                    ) {
                        let _ = DestroyWindow(self.hwnd);
                    }
                }
                _ => {}
            }
            self.refresh();
        }
    }
}

unsafe fn show_tray_menu(hwnd: HWND, state: &Snapshot) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let idle = !matches!(
            state.phase,
            Phase::Capturing | Phase::Processing | Phase::Preparing
        );
        let retry_available = idle && state.retry_available;
        let availability = |enabled: bool| {
            if enabled {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            }
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, w!("开始 / 停止  Ctrl+Alt+R"));
        // Ticket #20 adds "打开面板" and the refine entry here.
        let _ = AppendMenuW(
            menu,
            availability(retry_available),
            ID_RETRY,
            w!("重试本地识别"),
        );
        let _ = AppendMenuW(
            menu,
            availability(retry_available),
            ID_CLEANUP,
            w!("清理失败音频"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        if selected.0 != 0 {
            let _ = PostMessageW(
                Some(hwnd),
                WM_COMMAND,
                WPARAM(selected.0 as usize),
                LPARAM(0),
            );
        }
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn window_proc(
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
        let handled = UI.with(|slot| {
            // Native menus and window updates may synchronously reenter this
            // procedure; a busy slot simply defers to the next poll.
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return false;
            };
            let Some(state) = slot.as_mut() else {
                return false;
            };
            match message {
                WM_TIMER => {
                    state.refresh();
                    true
                }
                WM_HOTKEY => {
                    state.command(3);
                    true
                }
                CONTROL_MESSAGE => {
                    state.command(wparam.0);
                    true
                }
                WM_COMMAND => {
                    state.command(wparam.0 & 0xffff);
                    true
                }
                TRAY_MESSAGE => {
                    if matches!(lparam.0 as u32, WM_LBUTTONUP | WM_LBUTTONDBLCLK) {
                        // Ticket #20 opens the expanded panel here instead.
                        state.command(ID_SHOW);
                    }
                    true
                }
                _ => false,
            }
        });
        if handled {
            if message == TRAY_MESSAGE && lparam.0 as u32 == WM_RBUTTONUP {
                // Native menus dispatch hotkey, control and timer messages while
                // open. Release the UI borrow before entering that nested loop.
                let snapshot =
                    UI.with(|slot| slot.borrow().as_ref().map(|state| state.app.snapshot()));
                if let Some(snapshot) = snapshot {
                    show_tray_menu(hwnd, &snapshot);
                }
            }
            LRESULT(0)
        } else {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}

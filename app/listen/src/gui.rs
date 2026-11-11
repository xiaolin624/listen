use crate::{Config, WasapiPlayback, Whisper};
use listen::{Application, Control, Delivery, Phase, Snapshot, target::NativeTargets};
use std::{cell::RefCell, fs, sync::Arc};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{EM_SETLIMITTEXT, EM_SETSEL},
            Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            },
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

const CLASS: PCWSTR = w!("ListenCaptureToCopyWindowV1");
const CONTROL_MESSAGE: u32 = WM_APP + 10;
const TRAY_MESSAGE: u32 = WM_APP + 11;
const ID_TOGGLE: usize = 101;
const ID_COPY: usize = 102;
const ID_EXIT: usize = 103;
const ID_SHOW: usize = 104;

struct Ui {
    app: Application,
    hwnd: HWND,
    status: HWND,
    result: HWND,
    toggle: HWND,
    copy: HWND,
    exit: HWND,
    explanation: HWND,
    label: HWND,
    runtime: std::path::PathBuf,
    last: Option<Snapshot>,
    tray: bool,
    quit_requested: bool,
}
thread_local! { static UI: RefCell<Option<Ui>> = const { RefCell::new(None) }; }

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
unsafe fn text(hwnd: HWND, value: &str) -> windows::core::Result<()> {
    unsafe { SetWindowTextW(hwnd, PCWSTR(wide(value).as_ptr())) }
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
        _ => return Err(format!("Unknown control command: {command}")),
    };
    unsafe {
        let hwnd = FindWindowW(CLASS, None).map_err(|_| "Listen is not running".to_string())?;
        PostMessageW(Some(hwnd), CONTROL_MESSAGE, WPARAM(id), LPARAM(0))
            .map_err(|e| format!("Cannot contact running Listen: {e}"))
    }
}

pub fn run(config: Config) -> Result<(), String> {
    unsafe {
        if let Ok(existing) = FindWindowW(CLASS, None) {
            let _ = ShowWindow(existing, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(existing);
            return Err("Listen is already running. Exit it from its window or tray before changing configuration.".into());
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
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Cannot register Listen window class".into());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("Listen — 耳机音频本地转写"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            670,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        let child =
            |class: PCWSTR, value: &str, style: WINDOW_STYLE, id: usize| -> Result<HWND, String> {
                let handle = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    PCWSTR(wide(value).as_ptr()),
                    WS_CHILD | WS_VISIBLE | style,
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(id as *mut _)),
                    Some(instance.into()),
                    None,
                )
                .map_err(|e| e.to_string())?;
                SendMessageW(
                    handle,
                    WM_SETFONT,
                    Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                    Some(LPARAM(1)),
                );
                Ok(handle)
            };
        let explanation = child(
            w!("STATIC"),
            "Ctrl+Alt+R 开始 / 停止  ·  直接采集默认播放设备的全部声音  ·  本地识别",
            WINDOW_STYLE::default(),
            110,
        )?;
        let status = child(
            w!("EDIT"),
            "",
            WS_BORDER
                | WS_VSCROLL
                | WINDOW_STYLE(ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32),
            111,
        )?;
        let label = child(
            w!("STATIC"),
            "备用文本框：当前完整结果（复制后由你手动粘贴和发送）",
            WINDOW_STYLE::default(),
            112,
        )?;
        let result = child(
            w!("EDIT"),
            "",
            WS_BORDER
                | WS_VSCROLL
                | WS_HSCROLL
                | WS_TABSTOP
                | WINDOW_STYLE(
                    ES_MULTILINE as u32
                        | ES_READONLY as u32
                        | ES_AUTOVSCROLL as u32
                        | ES_AUTOHSCROLL as u32,
                ),
            113,
        )?;
        SendMessageW(result, EM_SETLIMITTEXT, Some(WPARAM(0x7ffffffe)), None);
        let toggle = child(
            w!("BUTTON"),
            "开始采集",
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            ID_TOGGLE,
        )?;
        let copy = child(
            w!("BUTTON"),
            "复制完整结果",
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            ID_COPY,
        )?;
        let exit = child(
            w!("BUTTON"),
            "退出",
            WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            ID_EXIT,
        )?;
        let registration = RegisterHotKey(Some(hwnd), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'R' as u32).map_err(|e| format!("Ctrl+Alt+R: {e}. Release this shortcut in the other application, then restart Listen."));
        let app = Application::new(
            Arc::new(WasapiPlayback),
            Arc::new(Whisper {
                executable: config.engine,
                model: config.model,
                vad_model: config.vad_model,
                language: config.language,
                threads: config.threads,
            }),
            config.runtime.clone(),
            registration,
        )
        .with_targets(Arc::new(NativeTargets));
        let mut state = Ui {
            app,
            hwnd,
            status,
            result,
            toggle,
            copy,
            exit,
            explanation,
            label,
            runtime: config.runtime,
            last: None,
            tray: false,
            quit_requested: false,
        };
        let mut icon = state.icon_data();
        icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon.uCallbackMessage = TRAY_MESSAGE;
        state.tray = Shell_NotifyIconW(NIM_ADD, &icon).as_bool();
        state.layout();
        state.refresh();
        let show = !config.hidden || !state.tray || state.app.snapshot().phase == Phase::Failure;
        UI.with(|ui| *ui.borrow_mut() = Some(state));
        if SetTimer(Some(hwnd), 1, 100, None) == 0 {
            return Err("Cannot start the application status timer".into());
        }
        if show {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        }
        let mut message = MSG::default();
        loop {
            let next = GetMessageW(&mut message, None, 0, 0).0;
            if next == -1 {
                return Err("Listen Windows message loop failed".into());
            }
            if next == 0 {
                break;
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let _ = UnregisterHotKey(Some(hwnd), 1);
        UI.with(|ui| {
            if let Some(state) = ui.borrow_mut().take() {
                if state.tray {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &state.icon_data());
                }
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
            Phase::Processing => IDI_INFORMATION,
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

    unsafe fn layout(&self) {
        unsafe {
            let mut rect = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rect);
            let width = (rect.right - 32).max(320);
            let bottom = (rect.bottom - 52).max(360);
            let _ = MoveWindow(self.explanation, 16, 14, width, 24, true);
            let _ = MoveWindow(self.status, 16, 46, width, 120, true);
            let _ = MoveWindow(self.label, 16, 182, width, 24, true);
            let _ = MoveWindow(self.result, 16, 210, width, (bottom - 224).max(120), true);
            let _ = MoveWindow(self.toggle, 16, bottom, 140, 32, true);
            let _ = MoveWindow(self.copy, 168, bottom, 160, 32, true);
            let _ = MoveWindow(self.exit, (width - 104).max(340), bottom, 120, 32, true);
        }
    }

    unsafe fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    unsafe fn refresh(&mut self) {
        unsafe {
            self.app.poll();
            let state = self.app.snapshot();
            let changed = self.last.as_ref().is_none_or(|last| {
                last.phase != state.phase
                    || last.detail != state.detail
                    || last.text != state.text
                    || last.retained_audio != state.retained_audio
                    || last.delivery != state.delivery
            });
            if !changed {
                return;
            }
            let phase_changed = self
                .last
                .as_ref()
                .is_some_and(|last| last.phase != state.phase);
            let mut detail = format!(
                "{:?}\r\n{}",
                state.phase,
                state.detail.replace('\n', "\r\n")
            );
            if self.quit_requested {
                detail.push_str("\r\nFinishing the current operation before exiting.");
            }
            if !self.tray {
                detail.push_str(
                    "\r\nTray icon unavailable; minimizing keeps this window on the taskbar.",
                );
            }
            let _ = text(self.status, &detail);
            let _ = text(
                self.hwnd,
                &format!("Listen — {:?} — Ctrl+Alt+R", state.phase),
            );
            if self
                .last
                .as_ref()
                .is_none_or(|last| last.text != state.text)
            {
                let visible = state.text.replace("\r\n", "\n").replace('\n', "\r\n");
                let loaded = text(self.result, &visible).is_ok()
                    && GetWindowTextLengthW(self.result) as usize == visible.encode_utf16().count();
                if !loaded {
                    let _ = text(
                        self.status,
                        &format!(
                            "Failure: the fallback control could not display the complete result. The full current result is available at {}",
                            self.runtime.join("current-result.txt").display()
                        ),
                    );
                }
            }
            let _ = text(
                self.toggle,
                if state.phase == Phase::Capturing {
                    "停止并转写"
                } else if state.phase == Phase::Processing {
                    "正在本机转写…"
                } else {
                    "开始采集"
                },
            );
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                self.toggle,
                state.phase != Phase::Processing,
            );
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                self.copy,
                !state.text.is_empty(),
            );
            let status = format!(
                "phase={:?}\ndetail={}\ncurrent_result={}\nretained_audio={}\nprocess_id={}\n",
                state.phase,
                state.detail,
                self.runtime.join("current-result.txt").display(),
                state
                    .retained_audio
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" | "),
                std::process::id()
            );
            if let Err(error) = fs::write(self.runtime.join("status.txt"), status) {
                let _ = text(
                    self.status,
                    &format!("{detail}\r\nCannot persist current status: {error}"),
                );
            }
            if self.tray {
                let mut icon = self.icon_data();
                icon.uFlags = NIF_TIP | NIF_ICON;
                if phase_changed {
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
                                "Capturing the default playback device. Ctrl+Alt+R stops."
                            }
                            Phase::Processing => {
                                "Recognizing locally. Wait for the current result."
                            }
                            Phase::Result => {
                                if state.delivery == Some(Delivery::Appended) {
                                    "Appended to the original target. Review and send manually."
                                } else {
                                    "Check the original target, then use the complete fallback result."
                                }
                            }
                            Phase::NoSpeech => {
                                "No valid speech detected. Nothing was delivered; the previous result is unchanged."
                            }
                            Phase::Failure => {
                                "The operation failed. Open Listen for details and retained audio paths."
                            }
                            _ => "Ready. Ctrl+Alt+R starts playback capture.",
                        },
                    );
                }
                let _ = Shell_NotifyIconW(NIM_MODIFY, &icon);
            }
            if phase_changed
                && (state.phase == Phase::Failure
                    || (state.phase == Phase::Result && state.delivery != Some(Delivery::Appended)))
            {
                self.show();
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
                4 | ID_SHOW => self.show(),
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
                ID_COPY => {
                    // WM_COPY only responds to this explicit button. It never pastes or sends.
                    SendMessageW(self.result, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                    SendMessageW(self.result, WM_COPY, None, None);
                }
                _ => {}
            }
            self.refresh();
        }
    }
}

unsafe fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("显示当前结果"));
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, w!("开始 / 停止  Ctrl+Alt+R"));
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
        if message == WM_GETMINMAXINFO {
            let limits = &mut *(lparam.0 as *mut MINMAXINFO);
            limits.ptMinTrackSize = POINT { x: 620, y: 500 };
            return LRESULT(0);
        }
        let handled = UI.with(|slot| {
            // SetWindowText/child layout may synchronously reenter this procedure.
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
                WM_SIZE => {
                    state.layout();
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
                WM_CLOSE => {
                    let _ = ShowWindow(hwnd, if state.tray { SW_HIDE } else { SW_MINIMIZE });
                    true
                }
                TRAY_MESSAGE => {
                    if matches!(lparam.0 as u32, WM_LBUTTONUP | WM_LBUTTONDBLCLK) {
                        state.show();
                    }
                    true
                }
                _ => false,
            }
        });
        if handled {
            if message == TRAY_MESSAGE && lparam.0 as u32 == WM_RBUTTONUP {
                // Native menus dispatch hotkey, control and timer messages while open.
                // Release the UI borrow before entering that nested message loop.
                show_tray_menu(hwnd);
            }
            LRESULT(0)
        } else {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}

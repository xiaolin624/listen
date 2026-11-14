use crate::{
    Config,
    capsule::{CapsuleState, capsule_state, preparation_name},
    capsule_window::{self, Capsule},
    panel,
    panel_window::{self, Panel},
};
use listen::{
    Application, Control, Delivery, Phase, Refine, Snapshot, deepseek::DeepSeek, paste,
    target::NativeTargets,
};
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
/// Open the expanded result panel (the tray menu item and a tray left click).
const ID_SHOW: usize = 104;
const ID_RETRY: usize = 105;
const ID_CLEANUP: usize = 106;
/// `--control panel`: toggle the expanded result panel.
const ID_PANEL: usize = 107;
/// `--control copy`: copy the complete current result without opening the panel.
const ID_COPY_RESULT: usize = 108;
/// How often the application snapshot is polled and `status.txt` is refreshed.
const POLL_TIMER_MS: u32 = 100;

struct Ui {
    app: Application,
    /// Hidden owner window; the capsule and the expanded panel are its only
    /// user-visible surfaces.
    hwnd: HWND,
    capsule: Capsule,
    /// Created on first use, then only shown or hidden, so reopening is instant and
    /// the window keeps its settings and position for the session.
    panel: Option<Panel>,
    runtime: PathBuf,
    last: Option<Snapshot>,
    /// Last `status.txt` contents, so `panel_visible`/`panel_error` changes are
    /// persisted even when the application snapshot itself did not change.
    last_status: String,
    /// Last `panel.txt` contents. The file is the contract for automation.
    last_panel_text: String,
    /// The last fallback/failure the panel opened for, so a closed panel is not
    /// re-opened on every poll and a new event does open again.
    last_auto_open: Option<String>,
    /// Why the panel window could not be created, if it ever fails. Reported in
    /// `status.txt` as `panel_error` so a failure is diagnosable without a console.
    panel_error: Option<String>,
    tray: bool,
    /// `--hidden` still shows the capsule and the panel on demand (they are the only
    /// user-visible surfaces) but suppresses transient tray notifications.
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
        "panel" => 8,
        "copy" => 9,
        "refine" => 10,
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

        let mut app = config
            .application(registration)?
            .with_targets(Arc::new(NativeTargets::new()))
            // Text refine is separately opt-in. Explicit cloud recognition never
            // automatically invokes this connector or sends a second vendor request.
            .with_refine(Arc::new(DeepSeek::from_environment()));
        capsule.set_state(capsule_state(&app.snapshot()));
        // The ADR promises the capsule is invisible to every screen share. If the
        // platform refuses, say so in the panel/status instead of leaving only
        // `capsule_capture_excluded=0` to be discovered by reading a file.
        if let Some(reason) = capsule.capture_exclusion_error().map(str::to_string) {
            app.report_capture_exclusion(&reason);
        }

        let mut state = Ui {
            app,
            hwnd,
            capsule,
            panel: None,
            runtime: config.runtime,
            last: None,
            last_status: String::new(),
            last_panel_text: String::new(),
            last_auto_open: None,
            panel_error: None,
            tray: false,
            notify: !config.hidden,
            quit_requested: false,
        };
        let mut icon = state.icon_data();
        icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon.uCallbackMessage = TRAY_MESSAGE;
        state.tray = Shell_NotifyIconW(NIM_ADD, &icon).as_bool();
        UI.with(|ui| *ui.borrow_mut() = Some(state));
        // A click on the capsule toggles the expanded result panel. The handler is a
        // plain function so the capsule window never depends on the application.
        capsule_window::set_click_handler(toggle_panel_on_click);
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
            // Dialog keyboard processing for the expanded panel: Esc closes it and
            // Tab moves between its read-only text and its buttons. Messages that do
            // not belong to the panel fall through to the normal dispatch below.
            let dialog = UI.with(|ui| ui.borrow().as_ref().and_then(|state| state.panel_dialog()));
            if let Some(dialog) = dialog {
                if IsDialogMessageW(dialog, &message).as_bool() {
                    continue;
                }
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

            // `panel.txt` is the panel's full text (status + complete result) in
            // reading order. It is derived from the same snapshot as the window and
            // is written whenever it changes, open or not, so automation never needs
            // UIA or window handles to read the panel.
            let panel_text = panel::text(&state);
            if panel_text != self.last_panel_text {
                let _ = fs::write(self.runtime.join("panel.txt"), &panel_text);
                self.last_panel_text = panel_text;
            }

            // A result that could not be appended opens the panel by itself once:
            // the text must be reachable without the user discovering the capsule.
            // A successful append and a failure never do; the capsule turns red for
            // a failure and the user clicks it to read the reason.
            let auto_open = panel::auto_open_key(&state);
            if auto_open != self.last_auto_open {
                if auto_open.is_some() {
                    self.open_panel();
                }
                self.last_auto_open = auto_open;
            }
            if let Some(panel) = self.panel.as_mut() {
                panel.apply(&state);
            }

            let changed = self.last.as_ref().is_none_or(|last| {
                last.phase != state.phase
                    || last.preparation != state.preparation
                    || last.recognition != state.recognition
                    || last.detail != state.detail
                    || last.text != state.text
                    || last.retained_audio != state.retained_audio
                    || last.delivery != state.delivery
                    || last.retry_available != state.retry_available
                    || last.backend != state.backend
                    || last.timing != state.timing
                    || last.refine != state.refine
                    || last.refined_text != state.refined_text
                    || last.notice != state.notice
            });
            let phase_changed = self
                .last
                .as_ref()
                .is_some_and(|last| last.phase != state.phase);
            let timing = &state.timing;
            let panel_visible = self.panel_visible();
            let panel_error = self
                .panel_error
                .clone()
                .unwrap_or_default()
                .replace('\r', "")
                .replace('\n', " ");
            // The refine reason is one line so the file stays a simple key=value
            // contract. Only a state that stopped refine explains itself.
            let refine_reason = match &state.refine {
                Refine::Unavailable(reason)
                | Refine::NotApplied(reason)
                | Refine::Failed(reason) => reason.replace('\r', "").replace('\n', " "),
                _ => String::new(),
            };
            // A notice is a user-facing sentence with no key=value structure; it is
            // one line so `status.txt` stays a simple contract.
            let notice = state
                .notice
                .as_ref()
                .map(|notice| notice.text().replace('\r', "").replace('\n', " "))
                .unwrap_or_default();
            let status = format!(
                "phase={:?}\ndetail={}\nendpoint_role={}\nendpoint_id={}\nendpoint_name={}\ncurrent_result={}\nretained_audio={}\nretry_available={}\nbackend={}\nglossary={}\npreparation={}\nrefine={}\nrefine_reason={}\nnotice={}\ncapsule={}\npanel_visible={}\npanel_error={}\nstartup_capsule_ms={}\ncapsule_capture_excluded={}\ntiming_capture_start_ms={}\ntiming_preparation_ms={}\ntiming_stop_to_result_ms={}\ntiming_finalize_ms={}\ntiming_recognition_ms={}\ntiming_last_window_wait_ms={}\ntiming_last_window_inference_ms={}\ntiming_save_ms={}\ntiming_delivery_ms={}\ntiming_recognition_windows={}\nprocess_id={}\nrecognition={}\n",
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
                self.app.glossary_status(),
                preparation_name(state.preparation),
                state.refine.name(),
                refine_reason,
                notice,
                visual.name(),
                if panel_visible { 1 } else { 0 },
                panel_error,
                self.capsule.startup_ms(),
                if self.capsule.capture_excluded() {
                    1
                } else {
                    0
                },
                timing
                    .capture_start_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
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
                std::process::id(),
                state
                    .recognition
                    .map(|state| state.name())
                    .unwrap_or("none"),
            );
            // `status.txt` also changes when only the panel opened/closed or the
            // panel window failed to create, which the snapshot comparison above
            // cannot see.
            let status_changed = status != self.last_status;
            if status_changed {
                let _ = fs::write(self.runtime.join("status.txt"), &status);
                self.last_status = status;
            }
            if !changed && !status_changed {
                return;
            }
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
                                if state.cloud_trial() {
                                    "Waiting for complete cloud recognition; received audio is retained."
                                } else {
                                    "Recognizing locally. Wait for the current result."
                                }
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
                                    "The operation failed. Open the panel for the reason and any retained audio paths."
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

    /// Toggle the expanded result panel. `--control panel` and a capsule click reach
    /// this; the button/tray paths use `open_panel` instead.
    fn toggle_panel(&mut self) {
        if self.panel_visible() {
            self.close_panel();
        } else {
            self.open_panel();
        }
    }

    /// Open (creating it on first use) and show the panel next to the capsule.
    /// Showing never activates the window, so this is safe from a poll update, from
    /// `--control show` and from a capsule click.
    fn open_panel(&mut self) {
        if self.panel.is_none() {
            match Panel::create(self.capsule.hwnd(), self.hwnd) {
                Ok(panel) => {
                    self.panel = Some(panel);
                    self.panel_error = None;
                }
                Err(error) => {
                    self.panel_error = Some(error);
                    return;
                }
            }
        }
        self.capsule.reveal();
        if let Some(panel) = self.panel.as_mut() {
            panel.place_near(self.capsule.hwnd());
            panel.apply(&self.app.snapshot());
            panel.show();
        }
    }

    fn close_panel(&mut self) {
        if let Some(panel) = &self.panel {
            panel.hide();
        }
    }

    fn panel_visible(&self) -> bool {
        self.panel.as_ref().is_some_and(|panel| panel.visible())
    }

    fn panel_dialog(&self) -> Option<HWND> {
        self.panel.as_ref().and_then(|panel| panel.dialog_handle())
    }

    /// Run one action requested from the panel window. Commands arrive through the
    /// owner window, never inside the panel's own procedure, so application polling
    /// and native menus keep working.
    fn panel_command(&mut self, command: usize) {
        match command {
            panel_window::COMMAND_COPY => self.copy_result(),
            panel_window::COMMAND_RETRY => self.app.dispatch(Control::Retry),
            panel_window::COMMAND_CLEANUP => self.app.dispatch(Control::CleanupAudio),
            panel_window::COMMAND_REFINE => self.app.dispatch(Control::Refine),
            panel_window::COMMAND_CLOSE => self.close_panel(),
            _ => {}
        }
        unsafe {
            self.refresh();
        }
    }

    /// Clipboard only: never pastes, never sends, never touches a target. A
    /// platform refusal is reported through the snapshot, so the panel and
    /// `status.txt` say the clipboard was not replaced.
    fn copy_result(&mut self) {
        self.app.copy_result(paste::copy_to_clipboard);
    }

    unsafe fn command(&mut self, id: usize) {
        unsafe {
            match id {
                1 => self.app.dispatch(Control::Start),
                2 => self.app.dispatch(Control::Stop),
                3 | ID_TOGGLE => self.app.dispatch(Control::Toggle),
                6 | ID_RETRY => self.app.dispatch(Control::Retry),
                7 | ID_CLEANUP => self.app.dispatch(Control::CleanupAudio),
                // `show` opens the panel, `panel` toggles it and `copy` copies the
                // current result; none of them takes focus.
                4 | ID_SHOW => self.open_panel(),
                8 | ID_PANEL => self.toggle_panel(),
                9 | ID_COPY_RESULT => self.copy_result(),
                10 => self.app.dispatch(Control::Refine),
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

/// The capsule click handler. It only toggles the panel and refreshes the status
/// files; it never activates the panel or the capsule.
fn toggle_panel_on_click() {
    UI.with(|ui| {
        // The click is delivered from the capsule window procedure, so a busy slot
        // (a native menu or a panel command re-entering) simply defers the click.
        let Ok(mut slot) = ui.try_borrow_mut() else {
            return;
        };
        if let Some(state) = slot.as_mut() {
            state.toggle_panel();
            unsafe {
                state.refresh();
            }
        }
    });
}

unsafe fn show_tray_menu(hwnd: HWND, state: &Snapshot) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        // The tray and the panel share this availability rule, so a menu item and
        // the matching panel button are always enabled together.
        let retry_available = panel::recovery_available(state);
        let availability = |enabled: bool| {
            if enabled {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            }
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("打开结果面板"));
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, w!("开始 / 停止  Ctrl+Alt+R"));
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
                panel_window::COMMAND_MESSAGE => {
                    state.panel_command(wparam.0);
                    true
                }
                TRAY_MESSAGE => {
                    if matches!(lparam.0 as u32, WM_LBUTTONUP | WM_LBUTTONDBLCLK) {
                        // A tray left click opens the expanded result panel.
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

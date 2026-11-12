//! The expanded result panel window (展开结果面板).
//!
//! The capsule shows no text at all, so this second, ordinary top-level window is
//! the only place Listen displays characters. It opens on a capsule click, from the
//! tray menu or via `--control show|panel`, and closes again on a capsule click,
//! Esc, the caption's close button or the panel's own 关闭 button.
//!
//! The window never activates itself: opening uses `SW_SHOWNOACTIVATE`, so neither
//! a poll-driven content update nor an automatic open (a fallback result or a
//! failure) can take focus from the user. It only takes focus when the user clicks
//! it, which is also how selecting and copying works.
//!
//! Both text areas are read-only multiline `EDIT` controls; the panel never edits
//! the 转写文本. Buttons only copy to the clipboard or dispatch the existing
//! application controls; nothing here pastes, sends Enter or submits a message.

use crate::panel;
use listen::Snapshot;
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET,
            DEFAULT_PITCH, DeleteObject, FF_DONTCARE, FW_NORMAL, GetMonitorInfoW, HBRUSH, HFONT,
            HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
            MonitorFromWindow, OUT_DEFAULT_PRECIS,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                EM_GETFIRSTVISIBLELINE, EM_GETSEL, EM_LINESCROLL, EM_SETLIMITTEXT, EM_SETSEL,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::EnableWindow,
            WindowsAndMessaging::{
                BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOVSCROLL,
                ES_MULTILINE, ES_READONLY, GW_OWNER, GetClientRect, GetDlgItem, GetPropW,
                GetSystemMetrics, GetWindow, GetWindowRect, GetWindowTextLengthW, HMENU,
                HWND_TOPMOST, IDC_ARROW, IDCANCEL, IsWindowVisible, LoadCursorW, MoveWindow,
                PostMessageW, RegisterClassW, RemovePropW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
                SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetPropW,
                SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
                WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_SETFONT, WM_SIZE, WNDCLASSW,
                WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
            },
        },
    },
    core::{PCWSTR, w},
};

/// Class of the expanded panel. Like `ListenStatusCapsuleV1` this name is private to
/// the implementation: acceptance drives the panel through `--control`, `status.txt`
/// and `panel.txt`, not through this class or its control IDs.
const PANEL_CLASS: PCWSTR = w!("ListenResultPanelV1");

/// Posted to the hidden owner window when a panel button is pressed or the panel is
/// closed by Esc / the caption. The owner value carries one of the `COMMAND_*`
/// constants below.
pub const COMMAND_MESSAGE: u32 = WM_APP + 12;
pub const COMMAND_COPY: usize = 1;
pub const COMMAND_RETRY: usize = 2;
pub const COMMAND_CLEANUP: usize = 3;
pub const COMMAND_CLOSE: usize = 4;
/// 精修: ask the application to refine the complete current result.
pub const COMMAND_REFINE: usize = 5;

// Panel control IDs. They are private; scripts use `--control` instead.
const ID_STATUS: i32 = 211;
const ID_RESULT: i32 = 212;
const ID_COPY: usize = 201;
const ID_RETRY: usize = 202;
const ID_CLEANUP: usize = 203;
const ID_REFINE: usize = 204;
const ID_CLOSE: usize = 205;

/// Window property holding the panel's font, so the window procedure can relayout
/// and recreate it on a DPI change without any global state.
const FONT_PROP: PCWSTR = w!("ListenPanelFont");
/// `EM_SETLIMITTEXT`: the default 64K edit limit is too small for a long session.
const EDIT_LIMIT: usize = 0x7fff_fffe;

/// One live panel window. The GUI owns at most one; dropping it destroys the
/// window. Content and button state are pushed in with [`Panel::apply`].
pub struct Panel {
    hwnd: HWND,
    status: HWND,
    result: HWND,
    copy: HWND,
    retry: HWND,
    cleanup: HWND,
    refine: HWND,
    last_status: String,
    last_result: String,
    last_refine_label: String,
}

impl Panel {
    /// Create the panel window hidden, owned by `owner` (the hidden message-loop
    /// window) and positioned next to `capsule`. The caller shows it with
    /// [`Panel::show`] after filling it with [`Panel::apply`].
    pub fn create(capsule: HWND, owner: HWND) -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: PANEL_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).map_err(|error| error.to_string())?,
                hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
                ..Default::default()
            };
            // A zero return only matters when the class is missing, which
            // CreateWindowExW reports below.
            let _ = RegisterClassW(&class);

            let scale = scale_for(capsule);
            let (width, height) = panel::size(scale);
            let (x, y) = position_for(capsule, width, height, scale);
            let title = wide("Listen — 结果面板");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0),
                PANEL_CLASS,
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
                x,
                y,
                width,
                height,
                Some(owner),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|error| format!("Cannot create the result panel: {error}"))?;

            match create_controls(hwnd, instance) {
                Ok((status, result, copy, retry, cleanup, refine)) => Ok(Self {
                    hwnd,
                    status,
                    result,
                    copy,
                    retry,
                    cleanup,
                    refine,
                    last_status: String::new(),
                    last_result: String::new(),
                    last_refine_label: String::new(),
                }),
                Err(error) => {
                    let _ = DestroyWindow(hwnd);
                    Err(error)
                }
            }
        }
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// The handle to offer `IsDialogMessageW` while the panel is visible. Esc and
    /// Tab are handled by the dialog keyboard processing in the message loop.
    pub fn dialog_handle(&self) -> Option<HWND> {
        self.visible().then_some(self.hwnd)
    }

    /// Put the panel next to the capsule on the capsule's monitor. Called before
    /// every show so a moved capsule or an unplugged monitor cannot leave the panel
    /// off-screen.
    pub fn place_near(&self, capsule: HWND) {
        unsafe {
            let scale = scale_for(capsule);
            let (width, height) = panel::size(scale);
            let (x, y) = position_for(capsule, width, height, scale);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            layout(self.hwnd);
        }
    }

    /// Show the panel without taking focus. The user can still click it.
    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            // Raise it to the topmost band without moving or resizing it.
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Push the snapshot into the two read-only text controls and the buttons.
    /// Text is only written when it actually changed, so an update never clears a
    /// selection the user made while reading.
    pub fn apply(&mut self, snapshot: &Snapshot) {
        let status = panel::status_text(snapshot);
        let result = panel::result_text(snapshot);
        let actions = panel::actions(snapshot);
        unsafe {
            if status != self.last_status {
                set_edit_text(self.status, &status);
                self.last_status = status;
            }
            if result != self.last_result {
                set_edit_text(self.result, &result);
                self.last_result = result;
            }
            enable(self.copy, actions.copy);
            enable(self.retry, actions.retry);
            enable(self.cleanup, actions.cleanup);
            // 精修: enabled only for a complete result the application can refine;
            // the label makes the running and unavailable states readable.
            enable(self.refine, actions.refine);
            let refine_label = panel::refine_label(snapshot);
            if refine_label != self.last_refine_label {
                let buffer = wide(refine_label);
                let _ = SetWindowTextW(self.refine, PCWSTR(buffer.as_ptr()));
                self.last_refine_label = refine_label.into();
            }
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

type Controls = (HWND, HWND, HWND, HWND, HWND, HWND);

unsafe fn create_controls(hwnd: HWND, instance: HMODULE) -> Result<Controls, String> {
    unsafe {
        let edit_style = WS_BORDER
            | WS_VSCROLL
            | WS_TABSTOP
            | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32);
        let button_style = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
        let status = child(hwnd, instance, w!("EDIT"), "", edit_style, ID_STATUS)?;
        let result = child(hwnd, instance, w!("EDIT"), "", edit_style, ID_RESULT)?;
        for control in [status, result] {
            let _ = SendMessageW(control, EM_SETLIMITTEXT, Some(WPARAM(EDIT_LIMIT)), None);
        }
        let copy = child(
            hwnd,
            instance,
            w!("BUTTON"),
            "复制完整结果",
            button_style,
            ID_COPY as i32,
        )?;
        let retry = child(
            hwnd,
            instance,
            w!("BUTTON"),
            "重试本地识别",
            button_style,
            ID_RETRY as i32,
        )?;
        let cleanup = child(
            hwnd,
            instance,
            w!("BUTTON"),
            "清理失败音频",
            button_style,
            ID_CLEANUP as i32,
        )?;
        let refine = child(
            hwnd,
            instance,
            w!("BUTTON"),
            "精修（即将可用）",
            button_style,
            ID_REFINE as i32,
        )?;
        child(
            hwnd,
            instance,
            w!("BUTTON"),
            "关闭",
            button_style,
            ID_CLOSE as i32,
        )?;
        layout(hwnd);
        Ok((status, result, copy, retry, cleanup, refine))
    }
}

unsafe fn child(
    parent: HWND,
    instance: HMODULE,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    id: i32,
) -> Result<HWND, String> {
    unsafe {
        let buffer = wide(text);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(buffer.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            1,
            1,
            Some(parent),
            Some(HMENU(id as isize as *mut _)),
            Some(instance.into()),
            None,
        )
        .map_err(|error| format!("Cannot create the result panel control {id}: {error}"))
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn scale_for(hwnd: HWND) -> f32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }
}

fn rect(value: RECT) -> panel::Rect {
    panel::Rect::new(value.left, value.top, value.right, value.bottom)
}

/// Device-pixel position of the panel: centred under the capsule, above it when the
/// monitor's work area has no room below, and always clamped into that work area.
unsafe fn position_for(capsule: HWND, width: i32, height: i32, scale: f32) -> (i32, i32) {
    unsafe {
        let mut capsule_rect = RECT::default();
        let capsule_ok = GetWindowRect(capsule, &mut capsule_rect).is_ok();
        let work = monitor_work(capsule);
        let gap = (panel::LOGICAL_GAP * scale).round() as i32;
        if !capsule_ok {
            // Without the capsule rectangle the panel still has a defined place: the
            // top left of the capsule monitor's work area.
            return (work.left, work.top);
        }
        panel::position(rect(capsule_rect), work, width, height, gap)
    }
}

/// The work area of the monitor the capsule is on. If the monitor cannot be
/// queried, the virtual screen is used, which is still better than an unclamped
/// position.
unsafe fn monitor_work(hwnd: HWND) -> panel::Rect {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };
        if !monitor.is_invalid() && GetMonitorInfoW(monitor, &mut info).as_bool() {
            return rect(info.rcWork);
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        if !monitor.is_invalid() && GetMonitorInfoW(monitor, &mut info).as_bool() {
            return rect(info.rcWork);
        }
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        panel::Rect::new(
            left,
            top,
            left + GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            top + GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

/// Lay the children out for the current client size and DPI. Pure geometry; the
/// same function runs on `WM_SIZE` and after a DPI change.
unsafe fn layout(hwnd: HWND) {
    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return;
        }
        let scale = scale_for(hwnd);
        let s = |value: f32| (value * scale).round() as i32;
        let font = ensure_font(hwnd, scale);
        let margin = s(12.0);
        let gap = s(8.0);
        let status_height = s(104.0);
        let button_height = s(30.0);
        let width = (client.right - 2 * margin).max(1);
        let button_y = (client.bottom - margin - button_height).max(margin);
        let result_y = margin + status_height + gap;
        let result_height = (button_y - gap - result_y).max(s(60.0));

        place(hwnd, ID_STATUS, margin, margin, width, status_height, font);
        place(
            hwnd,
            ID_RESULT,
            margin,
            result_y,
            width,
            result_height,
            font,
        );

        let mut x = margin;
        for (id, logical_width) in [
            (ID_COPY, 120.0),
            (ID_RETRY, 120.0),
            (ID_CLEANUP, 120.0),
            (ID_REFINE, 136.0),
        ] {
            let control_width = s(logical_width);
            place(
                hwnd,
                id as i32,
                x,
                button_y,
                control_width,
                button_height,
                font,
            );
            x += control_width + gap;
        }
        let close_width = s(72.0);
        let close_x = (client.right - margin - close_width).max(x);
        place(
            hwnd,
            ID_CLOSE as i32,
            close_x,
            button_y,
            close_width,
            button_height,
            font,
        );
    }
}

unsafe fn place(parent: HWND, id: i32, x: i32, y: i32, width: i32, height: i32, font: HFONT) {
    unsafe {
        if let Ok(control) = GetDlgItem(Some(parent), id) {
            let _ = MoveWindow(control, x, y, width, height, true);
            let _ = SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
}

/// Reuse the window's font, creating it on first use. A failed `CreateFontW`
/// leaves the controls with the system default font rather than failing the panel.
unsafe fn ensure_font(hwnd: HWND, scale: f32) -> HFONT {
    unsafe {
        let existing = GetPropW(hwnd, FONT_PROP);
        if !existing.is_invalid() {
            return HFONT(existing.0);
        }
        let height = -((14.0 * scale).round() as i32).max(9);
        let font = CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Microsoft YaHei UI"),
        );
        if font.is_invalid() {
            return HFONT(std::ptr::null_mut());
        }
        let _ = SetPropW(hwnd, FONT_PROP, Some(HANDLE(font.0)));
        font
    }
}

/// Drop the font so the next layout creates it at the new DPI.
unsafe fn release_font(hwnd: HWND) {
    unsafe {
        let font = GetPropW(hwnd, FONT_PROP);
        if !font.is_invalid() {
            let _ = RemovePropW(hwnd, FONT_PROP);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
}

/// Replace a read-only edit's text without losing the selection or the scroll
/// position. The panel is updated while the user may be reading it (the capture
/// progress line changes every poll), so a naive `SetWindowTextW` would jump back
/// to the top and clear a selection.
unsafe fn set_edit_text(hwnd: HWND, text: &str) {
    unsafe {
        let mut start: u32 = 0;
        let mut end: u32 = 0;
        let _ = SendMessageW(
            hwnd,
            EM_GETSEL,
            Some(WPARAM(&mut start as *mut u32 as usize)),
            Some(LPARAM(&mut end as *mut u32 as isize)),
        );
        let top = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, None, None).0 as i32;
        let buffer = wide(&text.replace('\n', "\r\n"));
        let _ = SetWindowTextW(hwnd, PCWSTR(buffer.as_ptr()));
        let length = GetWindowTextLengthW(hwnd).max(0) as u32;
        let start = start.min(length);
        let end = end.min(length).max(start);
        let _ = SendMessageW(
            hwnd,
            EM_SETSEL,
            Some(WPARAM(start as usize)),
            Some(LPARAM(end as isize)),
        );
        let current = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, None, None).0 as i32;
        let _ = SendMessageW(
            hwnd,
            EM_LINESCROLL,
            Some(WPARAM(0)),
            Some(LPARAM((top - current) as isize)),
        );
    }
}

fn enable(hwnd: HWND, enabled: bool) {
    unsafe {
        let _ = EnableWindow(hwnd, enabled);
    }
}

/// Tell the hidden owner window that the panel wants the application to act. This
/// is posted rather than called so a click inside the panel never runs application
/// code inside the panel's own window procedure.
fn notify(hwnd: HWND, command: usize) {
    unsafe {
        if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
            let _ = PostMessageW(Some(owner), COMMAND_MESSAGE, WPARAM(command), LPARAM(0));
        }
    }
}

unsafe fn hide_and_notify(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    notify(hwnd, COMMAND_CLOSE);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let suggested = *(lparam.0 as *const RECT);
                release_font(hwnd);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                layout(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                hide_and_notify(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xffff) as usize;
                if id == IDCANCEL.0 as usize {
                    // Esc, translated by IsDialogMessageW in the message loop.
                    hide_and_notify(hwnd);
                } else if let Some(command) = command_for_control(id) {
                    if command == COMMAND_CLOSE {
                        hide_and_notify(hwnd);
                    } else {
                        // The application decides what each command means; the panel
                        // only reports that its button was pressed.
                        notify(hwnd, command);
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                release_font(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Map the private control IDs to the owner commands the GUI handles.
pub fn command_for_control(id: usize) -> Option<usize> {
    match id {
        ID_COPY => Some(COMMAND_COPY),
        ID_RETRY => Some(COMMAND_RETRY),
        ID_CLEANUP => Some(COMMAND_CLEANUP),
        ID_REFINE => Some(COMMAND_REFINE),
        ID_CLOSE => Some(COMMAND_CLOSE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_documented_controls_produce_owner_commands() {
        assert_eq!(command_for_control(ID_COPY), Some(COMMAND_COPY));
        assert_eq!(command_for_control(ID_RETRY), Some(COMMAND_RETRY));
        assert_eq!(command_for_control(ID_CLEANUP), Some(COMMAND_CLEANUP));
        assert_eq!(command_for_control(ID_REFINE), Some(COMMAND_REFINE));
        assert_eq!(command_for_control(ID_CLOSE), Some(COMMAND_CLOSE));
        assert_eq!(command_for_control(ID_STATUS as usize), None);
        assert_eq!(command_for_control(ID_RESULT as usize), None);
    }
}

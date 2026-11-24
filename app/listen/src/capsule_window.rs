//! The resident status capsule window (状态胶囊).
//!
//! A long, rounded, always-on-top bar drawn entirely by us into a 32-bit
//! premultiplied ARGB DIB section and presented with `UpdateLayeredWindow`. It
//! never shows text, numbers or live transcription: colour, shape and motion are
//! the whole status. It never activates (no focus, no `SetForegroundWindow`), never
//! enters the taskbar, and is excluded from screen capture with
//! `WDA_EXCLUDEFROMCAPTURE`.
//!
//! The visual state comes from [`crate::capsule::capsule_state`]; this module only
//! turns that state into pixels, drag behaviour and a remembered position.

use crate::capsule::CapsuleState;
use std::{
    cell::RefCell,
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
    slice,
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            ClientToScreen, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
            DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            SystemInformation::GetSystemTimeAsFileTime,
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
                SetProcessDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics, GetWindowRect,
                HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, MA_NOACTIVATE, RegisterClassW,
                SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
                SWP_NOZORDER, SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
                ULW_ALPHA, UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_CAPTURECHANGED,
                WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

const CAPSULE_CLASS: PCWSTR = w!("ListenStatusCapsuleV1");
const POSITION_FILE: &str = "capsule-position.txt";
/// Inspection hook for the desktop verification script. The capsule is excluded
/// from every screen capture by design, so `scripts\verify-capsule.ps1` cannot
/// screenshot it; sending this message makes the capsule write the exact frame it
/// is presenting to `capsule-frame.bmp` in the runtime directory. Nothing in the
/// product sends it, and it never changes the window's appearance or focus.
pub const CAPSULE_FRAME_MESSAGE: u32 = WM_APP + 20;
pub const FRAME_FILE: &str = "capsule-frame.bmp";
const TIMER_ID: usize = 1;
/// Fast cadence while a state animates, slow cadence when the capsule is a static
/// status light. Repaints only happen when the frame actually changes.
const TIMER_ANIMATED_MS: u32 = 16;
const TIMER_IDLE_MS: u32 = 250;
/// Logical (96 DPI) size of the bar.
const LOGICAL_WIDTH: f32 = 220.0;
const LOGICAL_HEIGHT: f32 = 26.0;
/// A press that moves less than this many device pixels is a click, not a drag.
const DRAG_THRESHOLD: i32 = 3;
/// `MK_LBUTTON`: the left mouse button was down when a mouse message was posted.
/// The `windows` crate keeps the constant behind a feature this build does not use.
const MK_LBUTTON: usize = 0x0001;

thread_local! {
    static CAPSULE: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// The public handle the application keeps. The window draws itself from its own
/// timer; this handle only pushes state and answers questions about the window.
pub struct Capsule {
    hwnd: HWND,
    capture_excluded: bool,
    /// Why the platform refused to hide the capsule from screen capture, so the
    /// application can tell the user instead of only reporting a zero status key.
    capture_exclusion_error: Option<String>,
    startup_ms: u64,
}

impl Capsule {
    /// Whether `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` succeeded. False
    /// means the capsule is visible in screen sharing, which `status.txt` reports.
    pub fn capture_excluded(&self) -> bool {
        self.capture_excluded
    }

    /// The platform's reason when the capsule could not be excluded from capture.
    pub fn capture_exclusion_error(&self) -> Option<&str> {
        self.capture_exclusion_error.as_deref()
    }

    /// Measured process start to first presented capsule frame.
    pub fn startup_ms(&self) -> u64 {
        self.startup_ms
    }

    /// The capsule's window handle. The expanded panel uses it to find the capsule
    /// monitor and to place itself next to the bar; nothing else needs it.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Change the visual state. The window repaints immediately if the picture
    /// changes and picks the matching timer cadence.
    pub fn set_state(&self, state: CapsuleState) {
        let hwnd = self.hwnd;
        with_window(hwnd, |window| window.set_state(state));
    }

    /// Make the capsule visible and topmost again without taking focus or moving
    /// it. The expanded panel opens through here so the capsule cannot be left
    /// behind another window.
    pub fn reveal(&self) {
        let hwnd = self.hwnd;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}

impl Drop for Capsule {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Opt the whole process into per-monitor DPI awareness v2. Must run before the
/// first window is created so the capsule and its saved position scale correctly.
/// An error only means the process is already DPI aware, which is acceptable.
pub fn enable_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Create the capsule at its remembered position (clamped into the current virtual
/// screen) and present the first frame before returning.
pub fn create(runtime: &Path, state: CapsuleState) -> Result<Capsule, String> {
    let started = Instant::now();
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CAPSULE_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|error| error.to_string())?,
            ..Default::default()
        };
        // A zero return is only fatal if the class is missing, which the
        // CreateWindowExW call below reports.
        let _ = RegisterClassW(&class);

        let scale = GetDpiForSystem() as f32 / 96.0;
        let (width, height) = pixel_size(scale);
        let (x, y) = initial_position(runtime, width, height);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            CAPSULE_CLASS,
            // The capsule never carries a title: `GetWindowTextLengthW` must be 0.
            w!(""),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|error| format!("Cannot create the status capsule: {error}"))?;
        let capture_exclusion = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        let capture_excluded = capture_exclusion.is_ok();
        let capture_exclusion_error = capture_exclusion.err().map(|error| error.to_string());

        let window = Window::new(hwnd, runtime.to_path_buf(), state, scale)?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let startup_ms = since_process_start_ms()
            .filter(|value| *value > 0)
            .unwrap_or_else(|| started.elapsed().as_millis() as u64);
        CAPSULE.with(|slot| *slot.borrow_mut() = Some(window));
        Ok(Capsule {
            hwnd,
            capture_excluded,
            capture_exclusion_error,
            startup_ms,
        })
    }
}

/// Click hook for the expanded panel. `gui` registers the toggle handler once at
/// startup; a click (as opposed to a drag) calls it. This must never take focus
/// itself, so it must not call SetForegroundWindow, SetActiveWindow or SetFocus.
pub fn on_click() {
    CLICK_HANDLER.with(|slot| {
        if let Some(handler) = slot.get() {
            handler();
        }
    });
}

thread_local! {
    /// The panel toggle registered by `gui`. A plain function pointer keeps the
    /// capsule window independent of the application state it toggles.
    static CLICK_HANDLER: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
}

/// Register the action a capsule click performs. Called once by `gui` before the
/// message loop starts.
pub fn set_click_handler(handler: fn()) {
    CLICK_HANDLER.with(|slot| slot.set(Some(handler)));
}

fn with_window(hwnd: HWND, action: impl FnOnce(&mut Window)) {
    CAPSULE.with(|slot| {
        // SetWindowPos/UpdateLayeredWindow may synchronously reenter the window
        // procedure; a busy slot simply means the current paint wins.
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        if let Some(window) = slot.as_mut() {
            if window.hwnd == hwnd {
                action(window);
            }
        }
    });
}

struct Window {
    hwnd: HWND,
    runtime: PathBuf,
    state: CapsuleState,
    scale: f32,
    width: i32,
    height: i32,
    screen: HDC,
    surface: Surface,
    started: Instant,
    last_frame_ms: Option<u64>,
    pressed: bool,
    dragged: bool,
    press_cursor: POINT,
    press_window: POINT,
}

impl Window {
    unsafe fn new(
        hwnd: HWND,
        runtime: PathBuf,
        state: CapsuleState,
        scale: f32,
    ) -> Result<Self, String> {
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return Err("Cannot read the screen for the status capsule".into());
            }
            let (width, height) = pixel_size(scale);
            let surface = Surface::new(screen, width, height)?;
            let mut window = Self {
                hwnd,
                runtime,
                state,
                scale,
                width,
                height,
                screen,
                surface,
                started: Instant::now(),
                last_frame_ms: None,
                pressed: false,
                dragged: false,
                press_cursor: POINT::default(),
                press_window: POINT::default(),
            };
            window.start_timer();
            window.paint(0)?;
            Ok(window)
        }
    }

    fn start_timer(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ID);
            let interval = if animates(self.state) {
                TIMER_ANIMATED_MS
            } else {
                TIMER_IDLE_MS
            };
            // Without a timer the capsule still repaints on every state change;
            // animations would simply stop until the next change.
            let _ = SetTimer(Some(self.hwnd), TIMER_ID, interval, None);
        }
    }

    fn set_state(&mut self, state: CapsuleState) {
        if self.state == state {
            return;
        }
        let cadence_changed = animates(self.state) != animates(state);
        self.state = state;
        if cadence_changed {
            self.start_timer();
        }
        let elapsed = self.started.elapsed().as_millis() as u64;
        unsafe {
            let _ = self.paint(elapsed);
        }
    }

    fn tick(&mut self) {
        if !animates(self.state) {
            // Idle states are static: nothing to repaint until the state changes.
            return;
        }
        let elapsed = self.started.elapsed().as_millis() as u64;
        if self.last_frame_ms == Some(elapsed) {
            return;
        }
        unsafe {
            let _ = self.paint(elapsed);
        }
    }

    unsafe fn paint(&mut self, elapsed_ms: u64) -> Result<(), String> {
        unsafe {
            let pixels = self.surface.pixels();
            paint_pixels(
                pixels,
                self.width,
                self.height,
                self.scale,
                self.state,
                elapsed_ms as f32 / 1000.0,
            );
            self.surface.present(self.hwnd, self.screen)?;
            self.last_frame_ms = Some(elapsed_ms);
            Ok(())
        }
    }

    /// Write the currently presented frame as a 32-bit top-down BMP. BMP needs no
    /// image codec dependency; `scripts\verify-capsule.ps1` converts it to PNG.
    /// Only reachable through `CAPSULE_FRAME_MESSAGE`.
    unsafe fn save_frame(&mut self) -> Result<(), String> {
        unsafe {
            let pixel_bytes = self.width as usize * self.height as usize * 4;
            let file_size = 54 + pixel_bytes as u32;
            let mut bytes = Vec::with_capacity(file_size as usize);
            bytes.extend_from_slice(b"BM");
            bytes.extend_from_slice(&file_size.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&54u32.to_le_bytes());
            bytes.extend_from_slice(&40u32.to_le_bytes());
            bytes.extend_from_slice(&self.width.to_le_bytes());
            bytes.extend_from_slice(&(-self.height).to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&32u16.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
            bytes.extend_from_slice(&2835i32.to_le_bytes());
            bytes.extend_from_slice(&2835i32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            for pixel in self.surface.pixels() {
                bytes.extend_from_slice(&pixel.to_le_bytes());
            }
            fs::write(self.runtime.join(FRAME_FILE), bytes)
                .map_err(|error| format!("Cannot save the capsule frame: {error}"))
        }
    }

    /// Start a drag from the press point carried by the mouse message. The point is
    /// converted to screen coordinates because the window moves during the drag;
    /// mouse messages are used instead of polling the cursor so a drag follows the
    /// exact input that started it.
    unsafe fn begin_drag(&mut self, lparam: LPARAM) {
        unsafe {
            let mut point = POINT {
                x: loword(lparam),
                y: hiword(lparam),
            };
            let mut rect = RECT::default();
            if ClientToScreen(self.hwnd, &mut point).0 == 0
                || GetWindowRect(self.hwnd, &mut rect).is_err()
            {
                return;
            }
            self.pressed = true;
            self.dragged = false;
            self.press_cursor = point;
            self.press_window = POINT {
                x: rect.left,
                y: rect.top,
            };
            SetCapture(self.hwnd);
        }
    }

    unsafe fn drag(&mut self, wparam: WPARAM, lparam: LPARAM) {
        unsafe {
            if !self.pressed || (wparam.0 & MK_LBUTTON) == 0 {
                return;
            }
            let mut point = POINT {
                x: loword(lparam),
                y: hiword(lparam),
            };
            if ClientToScreen(self.hwnd, &mut point).0 == 0 {
                return;
            }
            let dx = point.x - self.press_cursor.x;
            let dy = point.y - self.press_cursor.y;
            if !self.dragged && (dx.abs() >= DRAG_THRESHOLD || dy.abs() >= DRAG_THRESHOLD) {
                self.dragged = true;
            }
            if self.dragged {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    self.press_window.x + dx,
                    self.press_window.y + dy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
        }
    }

    unsafe fn end_drag(&mut self) {
        unsafe {
            let _ = ReleaseCapture();
        }
        if !self.pressed {
            return;
        }
        self.pressed = false;
        if self.dragged {
            self.dragged = false;
            self.clamp_into_screen();
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(self.hwnd, &mut rect) }.is_ok() {
                save_position(&self.runtime, rect.left, rect.top);
            }
        } else {
            on_click();
        }
    }

    fn cancel_drag(&mut self) {
        self.pressed = false;
        self.dragged = false;
    }

    fn clamp_into_screen(&mut self) {
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(self.hwnd, &mut rect).is_err() {
                return;
            }
            let (left, top, right, bottom) = virtual_screen();
            let x = rect.left.clamp(left, (right - self.width).max(left));
            let y = rect.top.clamp(top, (bottom - self.height).max(top));
            if x != rect.left || y != rect.top {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }
    }

    unsafe fn handle_dpi_change(&mut self, lparam: LPARAM) {
        unsafe {
            let suggested = lparam.0 as *const RECT;
            if suggested.is_null() {
                return;
            }
            let suggested = *suggested;
            let dpi = GetDpiForWindow(self.hwnd);
            let scale = if dpi == 0 {
                self.scale
            } else {
                dpi as f32 / 96.0
            };
            let (width, height) = pixel_size(scale);
            match Surface::new(self.screen, width, height) {
                Ok(surface) => {
                    self.surface = surface;
                    self.scale = scale;
                    self.width = width;
                    self.height = height;
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    self.clamp_into_screen();
                    let elapsed = self.started.elapsed().as_millis() as u64;
                    let _ = self.paint(elapsed);
                }
                Err(_) => {
                    // Keep the old surface rather than drawing at the wrong size.
                }
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ID);
            ReleaseDC(None, self.screen);
        }
    }
}

/// A top-down 32-bit premultiplied ARGB DIB section selected into a memory DC.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u32,
    width: i32,
    height: i32,
}

impl Surface {
    unsafe fn new(screen: HDC, width: i32, height: i32) -> Result<Self, String> {
        unsafe {
            let dc = CreateCompatibleDC(Some(screen));
            if dc.is_invalid() {
                return Err("Cannot create the capsule bitmap context".into());
            }
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .map_err(|error| format!("Cannot create the capsule bitmap: {error}"))?;
            if bits.is_null() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                return Err("The capsule bitmap has no pixels".into());
            }
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            Ok(Self {
                dc,
                bitmap,
                previous,
                pixels: bits.cast::<u32>(),
                width,
                height,
            })
        }
    }

    unsafe fn pixels(&mut self) -> &mut [u32] {
        unsafe { slice::from_raw_parts_mut(self.pixels, (self.width * self.height) as usize) }
    }

    unsafe fn present(&mut self, hwnd: HWND, screen: HDC) -> Result<(), String> {
        unsafe {
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect)
                .map_err(|error| format!("Cannot read the capsule position: {error}"))?;
            let destination = POINT {
                x: rect.left,
                y: rect.top,
            };
            let size = SIZE {
                cx: self.width,
                cy: self.height,
            };
            let source = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&destination as *const POINT),
                Some(&size as *const SIZE),
                Some(self.dc),
                Some(&source as *const POINT),
                COLORREF(0),
                Some(&blend as *const BLENDFUNCTION),
                ULW_ALPHA,
            )
            .map_err(|error| format!("Cannot present the capsule frame: {error}"))
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            CAPSULE_FRAME_MESSAGE => {
                // Returns 1 so the caller can tell a saved frame from a timeout.
                let saved = CAPSULE.with(|slot| {
                    let Ok(mut slot) = slot.try_borrow_mut() else {
                        return false;
                    };
                    match slot.as_mut() {
                        Some(window) if window.hwnd == hwnd => window.save_frame().is_ok(),
                        _ => false,
                    }
                });
                return LRESULT(if saved { 1 } else { 0 });
            }
            WM_DESTROY => {
                CAPSULE.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut() {
                        *slot = None;
                    }
                });
                return LRESULT(0);
            }
            _ => {}
        }
        let handled = CAPSULE.with(|slot| {
            // Painting and window updates may synchronously reenter this procedure.
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return false;
            };
            let Some(window) = slot.as_mut() else {
                return false;
            };
            if window.hwnd != hwnd {
                return false;
            }
            match message {
                WM_TIMER if wparam.0 == TIMER_ID => {
                    window.tick();
                    true
                }
                WM_LBUTTONDOWN => {
                    window.begin_drag(lparam);
                    true
                }
                WM_MOUSEMOVE => {
                    window.drag(wparam, lparam);
                    true
                }
                WM_LBUTTONUP => {
                    window.end_drag();
                    true
                }
                WM_CAPTURECHANGED => {
                    window.cancel_drag();
                    true
                }
                WM_DPICHANGED => {
                    window.handle_dpi_change(lparam);
                    true
                }
                WM_DISPLAYCHANGE => {
                    window.clamp_into_screen();
                    true
                }
                _ => false,
            }
        });
        if handled {
            LRESULT(0)
        } else {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}

fn animates(state: CapsuleState) -> bool {
    matches!(
        state,
        CapsuleState::Preparing | CapsuleState::Capturing | CapsuleState::Processing
    )
}

fn pixel_size(scale: f32) -> (i32, i32) {
    (
        (LOGICAL_WIDTH * scale).round().max(1.0) as i32,
        (LOGICAL_HEIGHT * scale).round().max(1.0) as i32,
    )
}

unsafe fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        (left, top, left + width, top + height)
    }
}

fn default_position(width: i32) -> (i32, i32) {
    unsafe {
        let (left, top, right, _bottom) = virtual_screen();
        let x = left + ((right - left - width) / 2).max(0);
        (x, top + 48)
    }
}

fn initial_position(runtime: &Path, width: i32, height: i32) -> (i32, i32) {
    let saved = fs::read_to_string(runtime.join(POSITION_FILE))
        .ok()
        .and_then(|text| parse_position(&text));
    let (left, top, right, bottom) = unsafe { virtual_screen() };
    let (x, y) = saved.unwrap_or_else(|| default_position(width));
    (
        x.clamp(left, (right - width).max(left)),
        y.clamp(top, (bottom - height).max(top)),
    )
}

fn parse_position(text: &str) -> Option<(i32, i32)> {
    let mut x = None;
    let mut y = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("x=") {
            x = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("y=") {
            y = value.trim().parse().ok();
        }
    }
    Some((x?, y?))
}

fn save_position(runtime: &Path, x: i32, y: i32) {
    let _ = fs::write(runtime.join(POSITION_FILE), format!("x={x}\ny={y}\n"));
}

/// The signed x coordinate packed into a mouse message's `lparam`.
fn loword(lparam: LPARAM) -> i32 {
    ((lparam.0 as u32 & 0xffff) as i16) as i32
}

/// The signed y coordinate packed into a mouse message's `lparam`.
fn hiword(lparam: LPARAM) -> i32 {
    (((lparam.0 as u32 >> 16) & 0xffff) as i16) as i32
}

fn since_process_start_ms() -> Option<u64> {
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
        .ok()?;
        let now = GetSystemTimeAsFileTime();
        let created = filetime_ticks(creation);
        let current = filetime_ticks(now);
        // FILETIME counts 100 ns intervals.
        Some(current.saturating_sub(created) / 10_000)
    }
}

fn filetime_ticks(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

/// Rasterise one frame of the capsule. Pure except for writing `pixels`; no GDI
/// text and no external dependencies. The rounded bar is a stadium shape sampled
/// analytically with a one-device-pixel anti-aliasing ramp, and the state only
/// changes colour, brightness and horizontal motion.
fn paint_pixels(
    pixels: &mut [u32],
    width: i32,
    height: i32,
    scale: f32,
    state: CapsuleState,
    time: f32,
) {
    let logical_width = width as f32 / scale;
    let logical_height = height as f32 / scale;
    let half_width = logical_width * 0.5;
    let half_height = logical_height * 0.5;
    let radius = half_height;
    for y in 0..height {
        for x in 0..width {
            let px = (x as f32 + 0.5) / scale;
            let py = (y as f32 + 0.5) / scale;
            let coverage = capsule_coverage(px, py, half_width, half_height, radius, scale);
            let index = (y * width + x) as usize;
            if coverage <= 0.0 {
                pixels[index] = 0;
                continue;
            }
            let (red, green, blue, alpha) =
                state_layer(state, px, py, logical_width, logical_height, time);
            let alpha = (alpha * coverage).clamp(0.0, 1.0);
            let a = (alpha * 255.0).round() as u32;
            let r = (red.clamp(0.0, 1.0) * a as f32).round() as u32;
            let g = (green.clamp(0.0, 1.0) * a as f32).round() as u32;
            let b = (blue.clamp(0.0, 1.0) * a as f32).round() as u32;
            pixels[index] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
}

fn capsule_coverage(
    x: f32,
    y: f32,
    half_width: f32,
    half_height: f32,
    radius: f32,
    scale: f32,
) -> f32 {
    let qx = (x - half_width).abs() - (half_width - radius);
    let qy = (y - half_height).abs() - (half_height - radius);
    let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    let inside = qx.max(qy).min(0.0);
    let distance = outside + inside - radius;
    (0.5 - distance * scale).clamp(0.0, 1.0)
}

/// Colour and opacity for one logical point. `time` is in seconds since the
/// capsule appeared; only animated states use it.
fn state_layer(
    state: CapsuleState,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    time: f32,
) -> (f32, f32, f32, f32) {
    let half_height = (height * 0.5).max(1.0);
    let vertical = (1.0 - ((y - half_height) / half_height).abs()).clamp(0.0, 1.0);
    match state {
        // Calm and dim: the tool is idle, nothing needs attention.
        CapsuleState::Ready => (0.36, 0.62, 0.55, 0.32 + 0.14 * vertical),
        // Dim and dashed with a slow breath: not ready yet, do not expect it.
        CapsuleState::Preparing => {
            let dash = if ((x / 14.0) as i32) % 2 == 0 {
                1.0
            } else {
                0.16
            };
            let pulse = 0.5 + 0.5 * (std::f32::consts::TAU * time / 1.8).sin();
            (
                0.58,
                0.64,
                0.74,
                (0.22 + 0.18 * vertical) * dash * (0.55 + 0.45 * pulse),
            )
        }
        // Live capture: saturated green that breathes steadily.
        CapsuleState::Capturing => {
            let pulse = 0.5 + 0.5 * (std::f32::consts::TAU * time / 1.1).sin();
            (
                0.10,
                0.86,
                0.58,
                (0.50 + 0.32 * vertical) * (0.80 + 0.20 * pulse),
            )
        }
        // Working: a bright band sweeps across a dim blue body.
        CapsuleState::Processing => {
            let sweep = ((time / 1.15) % 1.0) * (width + 56.0) - 28.0;
            let offset = (x - sweep) / 13.0;
            let band = (-(offset * offset)).exp();
            (
                0.38 + 0.45 * band,
                0.52 + 0.32 * band,
                1.0,
                (0.18 + 0.24 * vertical + 0.55 * band).min(0.94),
            )
        }
        // Failure: red and static, the only warm colour the capsule uses.
        CapsuleState::Failure => (0.93, 0.24, 0.27, 0.52 + 0.32 * vertical),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capsule_is_transparent_outside_its_rounded_bar() {
        let (width, height) = (220, 26);
        let mut pixels = vec![0u32; (width * height) as usize];
        paint_pixels(&mut pixels, width, height, 1.0, CapsuleState::Ready, 0.0);
        // The corners are outside the stadium shape; the middle is fully covered.
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[(width - 1) as usize], 0);
        assert_ne!(pixels[((height / 2) * width + width / 2) as usize], 0);
    }
}

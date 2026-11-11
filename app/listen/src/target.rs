//! Native editors selected from the real foreground thread's keyboard focus.
//! No clipboard, keyboard injection, focus changes, or submit patterns are used.

use crate::{AppResult, Target, TargetSelection};
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND},
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        },
        UI::{
            Accessibility::{
                CUIAutomation, HWINEVENTHOOK, IUIAutomation, IUIAutomationElement,
                IUIAutomationValuePattern, SetWinEventHook, UIA_EditControlTypeId,
                UIA_ValuePatternId, UnhookWinEvent,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{BSTR, PCWSTR},
};

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub struct NativeTargets;

struct Focus {
    hwnd: usize,
    root: usize,
    process: u32,
    thread: u32,
    identity: Identity,
}

static NEXT_SELECTION: AtomicUsize = AtomicUsize::new(1);

/// A per-selection window property closes the gap before the MTA binds UIA.
/// A replacement HWND has no matching property, even when its numeric value is reused.
struct Identity {
    hwnd: usize,
    name: Vec<u16>,
    token: usize,
}

impl Identity {
    fn stamp(hwnd: HWND) -> AppResult<Self> {
        let token = NEXT_SELECTION.fetch_add(1, Ordering::Relaxed);
        let name = format!("Listen.NativeTarget.{}.{token}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            SetPropW(hwnd, PCWSTR(name.as_ptr()), Some(HANDLE(token as *mut _)))
                .map_err(|e| format!("Cannot prove the selected target's identity: {e}"))?;
        }
        Ok(Self {
            hwnd: hwnd.0 as usize,
            name,
            token,
        })
    }

    fn matches(&self) -> bool {
        unsafe {
            GetPropW(HWND(self.hwnd as *mut _), PCWSTR(self.name.as_ptr())).0 as usize == self.token
        }
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        if self.matches() {
            // Remove only this selection's property, never a replacement window's data.
            unsafe {
                let _ = RemovePropW(HWND(self.hwnd as *mut _), PCWSTR(self.name.as_ptr()));
            }
        }
    }
}

impl TargetSelection for NativeTargets {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let Some(focus) = foreground_edit()? else {
            return Ok(None);
        };
        let (requests, receive) = mpsc::channel();
        let (ready, initialized) = mpsc::channel();
        thread::spawn(move || {
            // UIA objects are created, used and released in this one windowless MTA.
            let result = NativeEditor::bind(focus);
            let editor = match result {
                Ok(editor) => editor,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                return;
            }
            loop {
                pump_events();
                match receive.recv_timeout(Duration::from_millis(20)) {
                    Ok(Request::Read(reply)) => {
                        let _ = reply.send(editor.content());
                    }
                    Ok(Request::Replace {
                        expected,
                        replacement,
                        reply,
                    }) => {
                        let _ = reply.send(editor.replace(&expected, &replacement));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        response(initialized)?;
        Ok(Some(Box::new(NativeTarget { requests })))
    }
}

fn foreground_edit() -> AppResult<Option<Focus>> {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.0.is_null() {
            return Ok(None);
        }
        let mut process = 0;
        let thread = GetWindowThreadProcessId(foreground, Some(&mut process));
        if thread == 0 || process == std::process::id() {
            return Ok(None);
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread, &mut info)
            .map_err(|e| format!("Cannot inspect the foreground selection: {e}"))?;
        let focus = info.hwndFocus;
        let root = GetAncestor(foreground, GA_ROOT);
        if focus.0.is_null() || GetAncestor(focus, GA_ROOT) != root {
            return Ok(None);
        }
        let mut focused_process = 0;
        let focused_thread = GetWindowThreadProcessId(focus, Some(&mut focused_process));
        if focused_process != process || focused_thread != thread {
            return Ok(None);
        }
        let mut class = [0; 256];
        let length = GetClassNameW(focus, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..length]).to_ascii_lowercase();
        // These are control capabilities, not application types selected by the user.
        if class != "edit" && !class.starts_with("richedit") {
            return Ok(None);
        }
        let identity = Identity::stamp(focus)?;
        GetGUIThreadInfo(thread, &mut info)
            .map_err(|e| format!("Cannot confirm the foreground selection: {e}"))?;
        if GetForegroundWindow() != foreground || info.hwndFocus != focus || !identity.matches() {
            return Ok(None);
        }
        Ok(Some(Focus {
            hwnd: focus.0 as usize,
            root: root.0 as usize,
            process,
            thread,
            identity,
        }))
    }
}

enum Request {
    Read(mpsc::Sender<AppResult<String>>),
    Replace {
        expected: String,
        replacement: String,
        reply: mpsc::Sender<AppResult<()>>,
    },
}

struct NativeTarget {
    requests: mpsc::Sender<Request>,
}

fn response<T>(receive: mpsc::Receiver<AppResult<T>>) -> AppResult<T> {
    receive
        .recv_timeout(REPLY_TIMEOUT)
        .map_err(|e| format!("The target did not confirm the operation: {e}"))?
}

impl Target for NativeTarget {
    fn content(&mut self) -> AppResult<String> {
        let (reply, receive) = mpsc::channel();
        self.requests
            .send(Request::Read(reply))
            .map_err(|e| format!("The original target is unavailable: {e}"))?;
        response(receive)
    }

    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let (reply, receive) = mpsc::channel();
        self.requests
            .send(Request::Replace {
                expected: expected.into(),
                replacement: replacement.into(),
                reply,
            })
            .map_err(|e| format!("The original target is unavailable: {e}"))?;
        response(receive)
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

struct LifetimeHook(HWINEVENTHOOK);
impl Drop for LifetimeHook {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.0);
        }
    }
}

thread_local! {
    // Native HWND and UIA runtime IDs can be reused after destruction. Never rebind.
    static LIFETIME: Cell<(usize, usize, bool)> = const { Cell::new((0, 0, false)) };
}

unsafe extern "system" fn destroyed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object == OBJID_WINDOW.0 && child == CHILDID_SELF as i32 {
        LIFETIME.with(|state| {
            let (target, root, invalid) = state.get();
            state.set((
                target,
                root,
                invalid || hwnd.0 as usize == target || hwnd.0 as usize == root,
            ));
        });
    }
}

fn pump_events() {
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

struct NativeEditor {
    focus: Focus,
    element: IUIAutomationElement,
    pattern: IUIAutomationValuePattern,
    // Field order releases UIA references and the hook before leaving COM.
    _hook: LifetimeHook,
    _com: Com,
}

impl NativeEditor {
    fn bind(focus: Focus) -> AppResult<Self> {
        unsafe {
            if !focus.identity.matches() {
                return Err("The selected target was destroyed before binding".into());
            }
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| e.to_string())?;
            let com = Com;
            LIFETIME.with(|state| state.set((focus.hwnd, focus.root, false)));
            let hook = SetWinEventHook(
                EVENT_OBJECT_DESTROY,
                EVENT_OBJECT_DESTROY,
                None,
                Some(destroyed),
                focus.process,
                focus.thread,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.0.is_null() {
                return Err("Cannot track the lifetime of the selected native target".into());
            }
            let hook = LifetimeHook(hook);
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
                .map_err(|e| format!("Cannot initialize native target access: {e}"))?;
            let element = automation
                .ElementFromHandle(HWND(focus.hwnd as *mut _))
                .map_err(|e| format!("Cannot bind the selected native editor: {e}"))?;
            let pattern = element
                .GetCurrentPatternAs(UIA_ValuePatternId)
                .map_err(|e| format!("The selected editor has no supported value writer: {e}"))?;
            let editor = Self {
                focus,
                element,
                pattern,
                _hook: hook,
                _com: com,
            };
            editor.validate()?;
            Ok(editor)
        }
    }

    fn validate(&self) -> AppResult<()> {
        pump_events();
        if LIFETIME.with(|state| state.get().2) || !self.focus.identity.matches() {
            return Err(
                "The original target was destroyed; replacement controls are not used".into(),
            );
        }
        unsafe {
            let hwnd = HWND(self.focus.hwnd as *mut _);
            let mut process = 0;
            let thread = GetWindowThreadProcessId(hwnd, Some(&mut process));
            if !IsWindow(Some(hwnd)).as_bool()
                || process != self.focus.process
                || thread != self.focus.thread
            {
                return Err("The original target is no longer available".into());
            }
            let result = (|| -> windows::core::Result<bool> {
                Ok(self.element.CurrentNativeWindowHandle()? == hwnd
                    && self.element.CurrentProcessId()? as u32 == self.focus.process
                    && self.element.CurrentControlType()? == UIA_EditControlTypeId
                    && self.element.CurrentIsEnabled()?.as_bool()
                    && !self.element.CurrentIsPassword()?.as_bool()
                    && !self.pattern.CurrentIsReadOnly()?.as_bool())
            })();
            match result {
                Ok(true) => Ok(()),
                Ok(false) => {
                    Err("The original target is not an enabled writable native editor".into())
                }
                Err(error) => Err(format!("Cannot validate the original target: {error}")),
            }
        }
    }

    fn content(&self) -> AppResult<String> {
        self.validate()?;
        unsafe {
            self.pattern
                .CurrentValue()
                .map(|value| value.to_string())
                .map_err(|e| format!("Cannot read the latest target draft: {e}"))
        }
    }

    fn replace(&self, expected: &str, replacement: &str) -> AppResult<()> {
        if self.content()? != expected {
            return Err(
                "The target draft changed during output; no replacement was attempted".into(),
            );
        }
        let normalized = replacement.replace("\r\n", "\n").replace('\n', "\r\n");
        self.validate()?;
        unsafe {
            self.pattern
                .SetValue(&BSTR::from(normalized.as_str()))
                .map_err(|e| format!("Target output failed or is unconfirmed: {e}"))?;
        }
        let observed = self.content()?;
        if observed.replace("\r\n", "\n") != replacement.replace("\r\n", "\n") {
            return Err("The complete target output could not be confirmed".into());
        }
        Ok(())
    }
}

//! Editors selected from the real foreground thread through UI Automation.
//! Selection is fixed when capture starts. Output uses only `ValuePattern.SetValue`;
//! no clipboard, keyboard injection, focus changes, or submit patterns are used.

use crate::{AppResult, Target, TargetSelection};
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND},
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
            },
            Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound},
        },
        UI::{
            Accessibility::{
                CUIAutomation8, HWINEVENTHOOK, IUIAutomation, IUIAutomation2, IUIAutomationElement,
                IUIAutomationTextPattern, IUIAutomationValuePattern, SetWinEventHook,
                UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextPatternId,
                UIA_ValuePatternId, UnhookWinEvent,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{BSTR, Interface, PCWSTR},
};

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Chromium applies `ValuePattern.SetValue` on its UI thread after the call returns.
/// Confirmation waits for the new accessible value; the write itself is never repeated.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
const CONFIRM_INTERVAL: Duration = Duration::from_millis(20);
const MAX_ANCESTORS: usize = 64;

pub struct NativeTargets;

/// The foreground window, its root, and the keyboard focus observed when capture started.
/// UIA is only touched from the dedicated MTA thread that receives this value.
struct Focus {
    window: usize,
    root: usize,
    process: u32,
    thread: u32,
    focus: usize,
    identity: RootIdentity,
}

static NEXT_SELECTION: AtomicUsize = AtomicUsize::new(1);

/// A per-selection property on the root window supplements the UIA runtime identity.
/// It proves that a numerically reused window is not the selected window. A browser's
/// whole DOM lives under one HWND, so this is never the only identity check.
struct RootIdentity {
    hwnd: usize,
    name: Vec<u16>,
    token: Option<usize>,
}

impl RootIdentity {
    fn stamp(hwnd: HWND) -> Self {
        let token = NEXT_SELECTION.fetch_add(1, Ordering::Relaxed);
        let name = format!("Listen.NativeTarget.{}.{token}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // Cross-process properties can be refused by UIPI. The runtime ID, window
        // ownership and ancestry checks still identify the focused element in that case.
        let applied =
            unsafe { SetPropW(hwnd, PCWSTR(name.as_ptr()), Some(HANDLE(token as *mut _))).is_ok() };
        Self {
            hwnd: hwnd.0 as usize,
            name,
            token: applied.then_some(token),
        }
    }

    fn matches(&self) -> bool {
        let Some(token) = self.token else {
            return true;
        };
        unsafe {
            GetPropW(HWND(self.hwnd as *mut _), PCWSTR(self.name.as_ptr())).0 as usize == token
        }
    }
}

impl Drop for RootIdentity {
    fn drop(&mut self) {
        if self.token.is_some() && self.matches() {
            // Remove only this selection's property, never a replacement window's data.
            unsafe {
                let _ = RemovePropW(HWND(self.hwnd as *mut _), PCWSTR(self.name.as_ptr()));
            }
        }
    }
}

impl TargetSelection for NativeTargets {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let Some(focus) = foreground_focus()? else {
            return Ok(None);
        };
        let (requests, receive) = mpsc::channel();
        let (ready, initialized) = mpsc::channel();
        thread::spawn(move || {
            // UIA objects are created, used and released in this one windowless MTA.
            let editor = match NativeEditor::bind(focus) {
                Ok(Some(editor)) => editor,
                Ok(None) => {
                    let _ = ready.send(Ok(None));
                    return;
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            if ready.send(Ok(Some(()))).is_err() {
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
        match response(initialized)? {
            Some(()) => Ok(Some(Box::new(NativeTarget { requests }))),
            None => Ok(None),
        }
    }
}

/// UIA reports a native multiline EDIT (and some rich editors) as `Document`, while
/// single-line edits, browser text areas and contenteditable regions are `Edit`.
/// Both can carry a writable Value pattern, so writability is decided by the pattern
/// and its read-only state, never by the control type class alone.
fn writable_editor_type(control_type: i32) -> bool {
    control_type == UIA_EditControlTypeId.0 || control_type == UIA_DocumentControlTypeId.0
}

/// Reads the actual foreground window and its keyboard focus without changing either.
/// No control class filter is applied: UIA decides whether the focused element is a
/// writable editor. The selected foreground window is fixed here and never re-selected.
fn foreground_focus() -> AppResult<Option<Focus>> {
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
        let root = GetAncestor(foreground, GA_ROOT);
        if root.0.is_null() {
            return Ok(None);
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread, &mut info)
            .map_err(|e| format!("Cannot inspect the foreground selection: {e}"))?;
        let focus = info.hwndFocus;
        if focus.0.is_null() || GetAncestor(focus, GA_ROOT) != root {
            return Ok(None);
        }
        let mut focused_process = 0;
        let focused_thread = GetWindowThreadProcessId(focus, Some(&mut focused_process));
        if focused_thread == 0 || focused_process != process {
            return Ok(None);
        }
        let identity = RootIdentity::stamp(root);
        GetGUIThreadInfo(thread, &mut info)
            .map_err(|e| format!("Cannot confirm the foreground selection: {e}"))?;
        if GetForegroundWindow() != foreground || info.hwndFocus != focus {
            return Ok(None);
        }
        Ok(Some(Focus {
            window: foreground.0 as usize,
            root: root.0 as usize,
            process,
            thread,
            focus: focus.0 as usize,
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

#[derive(Clone, Copy)]
struct LifetimeState {
    target: usize,
    root: usize,
    invalid: bool,
}

thread_local! {
    // Native HWND and UIA runtime IDs can be reused after destruction. Never rebind.
    static LIFETIME: Cell<LifetimeState> = const { Cell::new(LifetimeState {
        target: 0,
        root: 0,
        invalid: false,
    }) };
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
            let mut lifetime = state.get();
            lifetime.invalid = lifetime.invalid
                || hwnd.0 as usize == lifetime.target
                || hwnd.0 as usize == lifetime.root;
            state.set(lifetime);
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
    value: IUIAutomationValuePattern,
    text: Option<IUIAutomationTextPattern>,
    automation: IUIAutomation,
    runtime_id: Vec<i32>,
    // Field order releases UIA references and the hook before leaving COM.
    _hook: LifetimeHook,
    _com: Com,
}

impl NativeEditor {
    fn bind(focus: Focus) -> AppResult<Option<Self>> {
        unsafe {
            if !focus.identity.matches() {
                return Err("The selected target window was replaced before binding".into());
            }
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| e.to_string())?;
            let com = Com;
            LIFETIME.with(|state| {
                state.set(LifetimeState {
                    target: focus.focus,
                    root: focus.root,
                    invalid: false,
                });
            });
            let hook = SetWinEventHook(
                EVENT_OBJECT_DESTROY,
                EVENT_OBJECT_DESTROY,
                None,
                Some(destroyed),
                focus.process,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.0.is_null() {
                return Err("Cannot track the lifetime of the selected target".into());
            }
            let hook = LifetimeHook(hook);
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_ALL)
                .map_err(|e| format!("Cannot initialize target access: {e}"))?;
            // UIA pattern methods set focus by default. Listen must not move focus away
            // from whatever the user selected or typed into after capture started.
            let automation2: IUIAutomation2 = automation
                .cast()
                .map_err(|e| format!("Cannot configure target access: {e}"))?;
            automation2
                .SetAutoSetFocus(false)
                .map_err(|e| format!("Cannot configure target access: {e}"))?;

            // The selection is only valid while the same foreground thread still owns
            // keyboard focus. A later window/focus change never re-selects a target.
            if GetForegroundWindow() != HWND(focus.window as *mut _) {
                return Ok(None);
            }
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            GetGUIThreadInfo(focus.thread, &mut info)
                .map_err(|e| format!("Cannot confirm the selected target: {e}"))?;
            if info.hwndFocus != HWND(focus.focus as *mut _)
                || GetAncestor(info.hwndFocus, GA_ROOT) != HWND(focus.root as *mut _)
            {
                return Ok(None);
            }

            let element = match automation.GetFocusedElement() {
                Ok(element) => element,
                Err(_) => return Ok(None),
            };
            if element.CurrentProcessId().map_err(|e| e.to_string())? as u32 != focus.process {
                return Ok(None);
            }
            if !writable_editor_type(element.CurrentControlType().map_err(|e| e.to_string())?.0) {
                // A focused document that is not an editor, a button, an address bar or
                // another control must fall back to the copy box.
                return Ok(None);
            }
            belongs_to_root(&automation, &element, &focus)?;
            let value = match element.GetCurrentPatternAs(UIA_ValuePatternId) {
                Ok(pattern) => pattern,
                Err(error) => {
                    return Err(format!(
                        "The selected editor has no supported value writer: {error}"
                    ));
                }
            };
            let text = element.GetCurrentPatternAs(UIA_TextPatternId).ok();
            let runtime_id = runtime_id(&element)?;
            let editor = Self {
                focus,
                element,
                value,
                text,
                automation,
                runtime_id,
                _hook: hook,
                _com: com,
            };
            editor.validate()?;
            Ok(Some(editor))
        }
    }

    fn validate(&self) -> AppResult<()> {
        pump_events();
        if LIFETIME.with(|state| state.get().invalid) {
            return Err(
                "The original target was destroyed; replacement controls are not used".into(),
            );
        }
        if !self.focus.identity.matches() {
            return Err(
                "The original target window was replaced; replacement controls are not used".into(),
            );
        }
        unsafe {
            let root = HWND(self.focus.root as *mut _);
            let mut process = 0;
            let thread = GetWindowThreadProcessId(root, Some(&mut process));
            if !IsWindow(Some(root)).as_bool()
                || thread != self.focus.thread
                || process != self.focus.process
            {
                return Err("The original target window is no longer available".into());
            }
            let focus = HWND(self.focus.focus as *mut _);
            let mut focused_process = 0;
            let focused_thread = GetWindowThreadProcessId(focus, Some(&mut focused_process));
            if !IsWindow(Some(focus)).as_bool()
                || focused_thread == 0
                || focused_process != self.focus.process
            {
                return Err(
                    "The original target was destroyed; replacement controls are not used".into(),
                );
            }
        }
        let observed = runtime_id(&self.element)
            .map_err(|_| "The original target is no longer available".to_string())?;
        if observed != self.runtime_id {
            return Err(
                "The original target was rebuilt; replacement controls are not used".into(),
            );
        }
        let state = unsafe {
            let control_type = self
                .element
                .CurrentControlType()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?;
            let enabled = self
                .element
                .CurrentIsEnabled()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool();
            let password = self
                .element
                .CurrentIsPassword()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool();
            let read_only = self
                .value
                .CurrentIsReadOnly()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool();
            let process = self
                .element
                .CurrentProcessId()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                as u32;
            (
                writable_editor_type(control_type.0),
                enabled,
                password,
                read_only,
                process,
            )
        };
        let (editable, enabled, password, read_only, process) = state;
        if !editable || !enabled || password || read_only || process != self.focus.process {
            return Err("The original target is not an enabled writable editor".into());
        }
        belongs_to_root(&self.automation, &self.element, &self.focus)
    }

    fn content(&self) -> AppResult<String> {
        self.validate()?;
        unsafe {
            match self.value.CurrentValue() {
                Ok(value) => Ok(value.to_string()),
                Err(value_error) => {
                    let Some(text) = &self.text else {
                        return Err(format!(
                            "Cannot read the latest target draft: {value_error}"
                        ));
                    };
                    let range = text
                        .DocumentRange()
                        .map_err(|e| format!("Cannot read the latest target draft: {e}"))?;
                    range
                        .GetText(-1)
                        .map(|value| value.to_string())
                        .map_err(|e| format!("Cannot read the latest target draft: {e}"))
                }
            }
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
            self.value
                .SetValue(&BSTR::from(normalized.as_str()))
                .map_err(|e| format!("Target output failed or is unconfirmed: {e}"))?;
        }
        let expected = replacement.replace("\r\n", "\n");
        let deadline = Instant::now() + CONFIRM_TIMEOUT;
        loop {
            let observed = self.content()?.replace("\r\n", "\n");
            if observed == expected {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("The complete target output could not be confirmed".into());
            }
            thread::sleep(CONFIRM_INTERVAL);
        }
    }
}

/// Confirms that the focused element lives inside the window selected at capture start.
/// Chromium exposes page editors without a native window handle, so the nearest ancestor
/// window is checked instead of relying on the HWND that hosts the whole page.
fn belongs_to_root(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    focus: &Focus,
) -> AppResult<()> {
    unsafe {
        let walker = automation
            .ControlViewWalker()
            .map_err(|e| format!("Cannot inspect the selected target: {e}"))?;
        let mut current = element.clone();
        for _ in 0..MAX_ANCESTORS {
            match current.CurrentNativeWindowHandle() {
                Ok(hwnd) if !hwnd.0.is_null() => {
                    let mut process = 0;
                    let thread = GetWindowThreadProcessId(hwnd, Some(&mut process));
                    if thread == 0 || !IsWindow(Some(hwnd)).as_bool() {
                        return Err("The original target window is no longer available".into());
                    }
                    if process != focus.process
                        || GetAncestor(hwnd, GA_ROOT) != HWND(focus.root as *mut _)
                    {
                        return Err(
                            "The selected editor no longer belongs to the foreground window".into(),
                        );
                    }
                    return Ok(());
                }
                _ => {}
            }
            match walker.GetParentElement(&current) {
                Ok(parent) => current = parent,
                Err(_) => break,
            }
        }
    }
    Err("Cannot confirm that the selected editor belongs to the foreground window".into())
}

/// UIA runtime IDs identify a live element, not a window whose native handle value may be
/// reused. The provider hands out a one-dimensional SAFEARRAY of integers per element.
fn runtime_id(element: &IUIAutomationElement) -> AppResult<Vec<i32>> {
    unsafe {
        let array = element
            .GetRuntimeId()
            .map_err(|e| format!("Cannot identify the selected editor: {e}"))?;
        if array.is_null() {
            return Err("The selected editor has no runtime identity".into());
        }
        let result = (|| -> windows::core::Result<Vec<i32>> {
            let lower = SafeArrayGetLBound(array, 1)?;
            let upper = SafeArrayGetUBound(array, 1)?;
            let mut values = Vec::new();
            for index in lower..=upper {
                let mut value = 0_i32;
                SafeArrayGetElement(array, &index, &mut value as *mut _ as *mut _)?;
                values.push(value);
            }
            Ok(values)
        })();
        let _ = SafeArrayDestroy(array);
        let values = result.map_err(|e| format!("Cannot identify the selected editor: {e}"))?;
        if values.is_empty() {
            return Err("The selected editor has no runtime identity".into());
        }
        Ok(values)
    }
}

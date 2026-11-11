//! Editors selected from the real foreground thread through UI Automation.
//! Selection is fixed when capture starts. Output uses `ValuePattern.SetValue`, and
//! only when a provider silently drops that write does it fall back to an appending
//! clipboard paste that produces real editing events (see `crate::paste`). Both paths
//! read the draft immediately before writing, confirm the result, and never send Enter.

use crate::{AppResult, Target, TargetSelection, paste};
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

/// Chromium applies `ValuePattern.SetValue` on its UI thread after the call returns.
/// Confirmation waits for the new accessible value; the write itself is never repeated.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
const CONFIRM_INTERVAL: Duration = Duration::from_millis(20);
/// The caller's wait covers the confirmation window plus the fallback paste sequence.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ANCESTORS: usize = 64;

/// Outcome of waiting for the accessible value to show a completed write.
enum Confirmation {
    Confirmed,
    Truncated(String),
    Unconfirmed,
}

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
            editor.validate_writable()?;
            Ok(Some(editor))
        }
    }

    /// Identity and lifetime of the editor selected at capture start. Reading remains
    /// possible after the provider reports the control read-only or disabled, because
    /// that is how a failed delivery can report whether the target changed.
    fn validate_identity(&self) -> AppResult<()> {
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
        let process = unsafe {
            self.element
                .CurrentProcessId()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                as u32
        };
        if process != self.focus.process {
            return Err(
                "The original target was rebuilt; replacement controls are not used".into(),
            );
        }
        belongs_to_root(&self.automation, &self.element, &self.focus)
    }

    /// Output is only attempted while the selected editor is still the same live,
    /// enabled, non-password and writable editor of the selected window.
    fn validate_writable(&self) -> AppResult<()> {
        self.validate_identity()?;
        let control_type = unsafe {
            self.element
                .CurrentControlType()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
        };
        if !writable_editor_type(control_type.0) {
            return Err("The original target is no longer a supported editor".into());
        }
        let enabled = unsafe {
            self.element
                .CurrentIsEnabled()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool()
        };
        if !enabled {
            return Err("The original target is disabled".into());
        }
        let password = unsafe {
            self.element
                .CurrentIsPassword()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool()
        };
        if password {
            return Err("The original target is a password field".into());
        }
        let read_only = unsafe {
            self.value
                .CurrentIsReadOnly()
                .map_err(|e| format!("Cannot validate the original target: {e}"))?
                .as_bool()
        };
        if read_only {
            return Err("The original target is read-only".into());
        }
        Ok(())
    }

    fn content(&self) -> AppResult<String> {
        self.validate_identity()?;
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
        self.validate_writable()?;
        // One write attempt only. A provider may accept it, drop it, or store a
        // truncated prefix when the editor has a smaller capacity.
        let written = unsafe {
            self.value
                .SetValue(&BSTR::from(normalized.as_str()))
                .is_ok()
        };
        if written {
            match self.confirmed(expected, replacement)? {
                Confirmation::Confirmed => return Ok(()),
                // Never paste into an editor that just reported a capacity limit: the
                // fallback would append through the same limit.
                Confirmation::Truncated(detail) => return Err(detail),
                Confirmation::Unconfirmed => {}
            }
        }
        // Some providers expose a Value pattern but drop the write (a controlled
        // rich-text composer restores its own state). Fall back to a real paste,
        // which produces ordinary editing events, while the target still owns the
        // foreground keyboard focus.
        self.paste_output(expected, replacement)
    }

    /// Waits for the accessible value to show the complete replacement, or proves a
    /// stable truncated prefix. The write itself is never repeated; only reads are.
    fn confirmed(&self, previous: &str, replacement: &str) -> AppResult<Confirmation> {
        let intended = replacement.replace("\r\n", "\n");
        let previous = previous.replace("\r\n", "\n");
        let deadline = Instant::now() + CONFIRM_TIMEOUT;
        // A provider may accept the write but store only a prefix when the editor has
        // a smaller capacity. Requiring a stable prefix avoids mistaking a provider
        // that still shows the old draft, or a write that is still being applied, for
        // truncation.
        let mut truncated: Option<(usize, u8)> = None;
        loop {
            let observed = self.content()?.replace("\r\n", "\n");
            if observed == intended {
                return Ok(Confirmation::Confirmed);
            }
            if !observed.is_empty() && observed != previous && intended.starts_with(&observed) {
                truncated = Some(match truncated {
                    Some((length, polls)) if length == observed.len() => (length, polls + 1),
                    _ => (observed.len(), 1),
                });
                if let Some((_, polls)) = truncated {
                    if polls >= 3 {
                        return Ok(Confirmation::Truncated(format!(
                            "The original target accepted only {} of {} characters; the appended text was truncated",
                            observed.chars().count(),
                            intended.chars().count()
                        )));
                    }
                }
            } else {
                truncated = None;
            }
            if Instant::now() >= deadline {
                return Ok(Confirmation::Unconfirmed);
            }
            thread::sleep(CONFIRM_INTERVAL);
        }
    }

    /// The paste fallback only appends, so a draft that cannot be read through UIA
    /// is preserved instead of being replaced. Nothing is written unless the draft
    /// can be read first, and the editor is read back before the result is trusted.
    fn paste_output(&self, expected: &str, replacement: &str) -> AppResult<()> {
        self.validate_identity()?;
        self.require_foreground_focus()?;
        // A provider may have applied the earlier `SetValue` after the confirmation
        // window closed. Re-read before touching the clipboard or sending any key.
        if normalized(&self.content()?).trim_end() == normalized(replacement).trim_end() {
            return Ok(());
        }
        // Never clear a clipboard whose previous content cannot be restored.
        let backup = paste::ClipboardBackup::save()?;
        let guard = || self.require_foreground_focus();
        let draft = normalized(&paste::read_draft(&guard)?);
        // The clipboard read used Ctrl+A, so leave the user with a caret even when
        // no paste is needed. Every key event re-validates the bound target.
        paste::collapse_to_end(&guard)?;
        match plan_paste(expected, replacement, &draft)? {
            // The real draft already contains the complete intended text (for
            // example a late or normalized provider write). Pasting again would
            // duplicate the transcript.
            PastePlan::AlreadyComplete | PastePlan::NothingToAppend => {
                drop(backup);
                return Ok(());
            }
            PastePlan::Append { separator, text } => {
                let payload = format!("{separator}{text}");
                paste::paste(&payload, &guard)?;
                let observed = normalized(&paste::read_back(&guard)?);
                paste::collapse_to_end(&guard)?;
                drop(backup);
                let wanted = format!("{draft}{payload}");
                if observed.trim_end() != wanted.trim_end() {
                    return Err(
                        "The complete target output could not be confirmed after the fallback write"
                            .into(),
                    );
                }
                Ok(())
            }
        }
    }
    /// The fallback writer sends real key events, so it may only run while the bound
    /// editor itself still owns the foreground keyboard focus.
    fn require_foreground_focus(&self) -> AppResult<()> {
        unsafe {
            if GetForegroundWindow() != HWND(self.focus.window as *mut _) {
                return Err(
                    "The original target is no longer the foreground window, so the fallback writer was not used"
                        .into(),
                );
            }
        }
        let focused = unsafe { self.automation.GetFocusedElement() }
            .map_err(|e| format!("Cannot confirm the focused target: {e}"))?;
        if runtime_id(&focused)? != self.runtime_id {
            return Err(
                "The focused control is not the original target, so the fallback writer was not used"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Line endings are not part of the delivered text's meaning. Providers can report
/// the same content as CRLF or LF, so every comparison and the payload calculation
/// use this form.
fn normalized(value: &str) -> String {
    value.replace("\r\n", "\n")
}

/// What the fallback writer must do after reading the real draft. `expected` is
/// the draft read before the UIA write, `replacement` is the complete intended
/// value, and `draft` is the live value read through the clipboard.
#[derive(Debug, PartialEq, Eq)]
enum PastePlan {
    /// The real draft already holds the complete intended value.
    AlreadyComplete,
    /// There is no transcript to append.
    NothingToAppend,
    /// Append `text` after the current draft, separated by `separator`.
    Append {
        separator: &'static str,
        text: String,
    },
}

/// Plans the fallback paste without trusting the un-normalized `expected` draft.
/// The transcript is always recovered by stripping the *normalized* expected draft
/// and its separator from the intended value; the live draft only decides whether a
/// write already happened and which separator to use. This never appends the whole
/// intended value when the target draft is not the expected one.
fn plan_paste(expected: &str, replacement: &str, draft: &str) -> AppResult<PastePlan> {
    let expected = normalized(expected);
    let replacement = normalized(replacement);
    let draft = normalized(draft);
    if replacement.trim_end() == expected.trim_end() {
        return Ok(PastePlan::NothingToAppend);
    }
    let appended = if expected.is_empty() {
        replacement.as_str()
    } else {
        let prefix = format!("{expected}\n");
        replacement.strip_prefix(&prefix).ok_or_else(|| {
            "The intended target value does not start with the draft read at stop; the fallback writer refused to paste to avoid duplicating the original draft"
                .to_string()
        })?
    };
    if appended.trim().is_empty() {
        return Ok(PastePlan::NothingToAppend);
    }
    if draft.trim_end() == replacement.trim_end() {
        return Ok(PastePlan::AlreadyComplete);
    }
    let live = draft.trim_end();
    let expected_trimmed = expected.trim_end();
    if live == expected_trimmed {
        return Ok(PastePlan::Append {
            separator: if draft.is_empty() { "" } else { "\n" },
            text: appended.to_string(),
        });
    }
    // A capacity-limited provider can store a longer prefix of the intended value
    // after the confirmation window closed. Appending after that partial value
    // would exceed the same limit, so report it instead of pasting.
    if live.len() > expected_trimmed.len() && replacement.trim_end().starts_with(live) {
        return Err(format!(
            "The original target accepted only {} of {} characters; the appended text was truncated",
            live.chars().count(),
            replacement.trim_end().chars().count()
        ));
    }
    // The user or provider changed the draft after it was read. Keep that changed
    // draft and append only the transcript, never a stale copy of the old draft.
    Ok(PastePlan::Append {
        separator: if draft.is_empty() { "" } else { "\n" },
        text: appended.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_expected_never_repeats_the_original_draft() {
        // A native multiline EDIT reports CRLF while the intended value is built
        // from the same draft with a '\n' separator. The payload must be only the
        // transcript, never the whole draft again.
        let expected = "第一行\r\n第二行";
        let replacement = "第一行\r\n第二行\n今天轉寫";
        let draft = "第一行\r\n第二行";
        assert_eq!(
            plan_paste(expected, replacement, draft).unwrap(),
            PastePlan::Append {
                separator: "\n",
                text: "今天轉寫".into()
            }
        );
    }

    #[test]
    fn a_late_complete_write_is_confirmed_without_pasting_again() {
        let expected = "原有草稿";
        let replacement = "原有草稿\n今天轉寫";
        // The provider applied the write after the 2 second confirmation window.
        let draft = "原有草稿\r\n今天轉寫\r\n";
        assert_eq!(
            plan_paste(expected, replacement, draft).unwrap(),
            PastePlan::AlreadyComplete
        );
    }

    #[test]
    fn an_empty_draft_gets_no_leading_separator() {
        assert_eq!(
            plan_paste("", "今天轉寫", "").unwrap(),
            PastePlan::Append {
                separator: "",
                text: "今天轉寫".into()
            }
        );
    }

    #[test]
    fn a_changed_draft_is_preserved_and_only_the_transcript_is_appended() {
        // The user kept typing during the stop-to-result delay. The live draft is
        // not the expected one; appending the stale expected draft would duplicate
        // and would overwrite the edit.
        assert_eq!(
            plan_paste("原有草稿", "原有草稿\n今天轉寫", "原有草稿 plus later edit").unwrap(),
            PastePlan::Append {
                separator: "\n",
                text: "今天轉寫".into()
            }
        );
    }

    #[test]
    fn a_late_truncated_write_is_reported_instead_of_pasted() {
        let error = plan_paste(
            "capacity draft",
            "capacity draft\nthe appended transcript",
            "capacity draft\nthe app",
        )
        .unwrap_err();
        assert!(error.contains("truncated"), "{error}");
        assert!(error.contains("of"), "{error}");
    }

    #[test]
    fn an_intended_value_that_does_not_contain_the_expected_draft_is_refused() {
        let error =
            plan_paste("expected draft", "unrelated replacement", "expected draft").unwrap_err();
        assert!(error.contains("refused"), "{error}");
    }

    #[test]
    fn line_endings_do_not_change_completion_judgement() {
        assert_eq!(
            plan_paste("a\r\nb", "a\r\nb\nc", "a\nb\r\nc").unwrap(),
            PastePlan::AlreadyComplete
        );
        assert_eq!(
            plan_paste("a\r\nb", "a\r\nb\nc", "a\r\nb").unwrap(),
            PastePlan::Append {
                separator: "\n",
                text: "c".into()
            }
        );
    }

    #[test]
    fn an_unconfirmed_write_that_left_the_previous_content_unchanged_appends_the_transcript() {
        assert_eq!(
            plan_paste("old", "old\nnew", "old").unwrap(),
            PastePlan::Append {
                separator: "\n",
                text: "new".into()
            }
        );
    }
}

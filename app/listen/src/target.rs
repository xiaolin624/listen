//! Editors selected from the real foreground thread through UI Automation.
//! Selection is fixed when capture starts. Output uses `ValuePattern.SetValue`, and
//! only when a provider silently drops that write does it fall back to an appending
//! clipboard paste that produces real editing events (see `crate::paste`). Both paths
//! read the draft immediately before writing, confirm the result, and never send Enter.
//!
//! The first delivery to an editor keeps that order. If its fallback really pasted,
//! the session remembers that this editor drops the direct write, and the next
//! deliveries to the same editor go straight to the paste path: no write is attempted,
//! so the two-second confirmation window and the late-apply race it guards against
//! cannot happen again. The memory is keyed on the editor (process id plus UIA runtime
//! id), never on the process alone, so another editor in the same application keeps
//! the direct path.
//!
//! The fallback is built for hosts that do not behave like a native control. Its
//! confirmation is bounded but not single-shot: the accessible value keeps the same
//! short window, and a paste whose text did not appear there is read back through the
//! clipboard with a bounded retry. When the bound editor lost the foreground focus, the
//! bound window and element are restored once before the fallback is refused. A UIA
//! node that a re-render replaced is still accepted as the same editor when its
//! process, window, control type, class/AutomationId and text prove it (see
//! `same_logical_editor`). A composer that reports an emptied input as one line break
//! is planned from its real clipboard draft, so the append and the confirmation always
//! describe the same text. None of this weakens "never write to another control": the
//! checks run before every single key.

use crate::{
    AppResult, Target, TargetSelection, paste,
    target_write::{appended_part, separator},
};
use std::{
    cell::Cell,
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
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
/// How long a pasted value may take to appear in the accessible value before the
/// slower clipboard read-back is used instead. Every waiting path is bounded.
///
/// This window includes the time the older code spent in fixed sleeps after the
/// paste (`KEY_SETTLE` + 220 ms), so the target still has the same total time to
/// publish the pasted text before the clipboard read-back is used. What changed
/// (#38) is that a responsive editor is confirmed as soon as its accessible value
/// matches instead of always waiting out the fixed sleeps.
const PASTE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(520);
/// How long the clipboard read-back may keep retrying once the accessible value did
/// not publish the pasted text inside `PASTE_CONFIRM_TIMEOUT`. One read-back is not
/// proof on a CEF/Electron host: the paste can be applied a moment after Ctrl+V, and
/// the first Ctrl+A/Ctrl+C then copies the previous draft. The retry is bounded in
/// wall time; each read-back is itself bounded by its own key settles.
const PASTE_READ_BACK_TIMEOUT: Duration = Duration::from_secs(1);
/// The pause between two clipboard read-backs inside that window. One read-back costs
/// two key settles, so the observed cadence is slower than this minimum.
const PASTE_READ_BACK_INTERVAL: Duration = Duration::from_millis(150);
/// How long a restored focus may take to become the observable foreground focus, and
/// how often the check is repeated while waiting. Restoring is only attempted once.
const FOCUS_RECOVERY_TIMEOUT: Duration = Duration::from_millis(300);
const FOCUS_RECOVERY_INTERVAL: Duration = Duration::from_millis(20);
/// The caller's wait covers the confirmation window plus the fallback paste sequence.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ANCESTORS: usize = 64;

/// What the paste fallback did. The caller learns from this whether the editor
/// really needed real editing keys, without guessing from a refusal or a late write.
#[derive(Debug, PartialEq, Eq)]
enum PasteOutcome {
    /// The editor already held the complete intended value; no key was sent.
    NotNeeded,
    /// The transcript was appended with real editing keys and read back.
    Appended,
}

/// Identity of one bound editor across the captures of a Listen session: the
/// process id plus the UIA runtime id. The process id alone is not enough — a
/// browser hosts a plain text field and a rich-text composer in the same process —
/// so another editor never inherits this editor's paste-only path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct EditorKey {
    process: u32,
    runtime: Vec<i32>,
}

impl EditorKey {
    fn label(&self) -> String {
        let runtime = self
            .runtime
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(".");
        format!("{}/{runtime}", self.process)
    }
}

/// What this Listen session learned about the editors it bound.
///
/// Only an editor that actually appended through the keyboard fallback is
/// remembered as dropping `ValuePattern.SetValue`. A refused write, a late write
/// that landed inside the confirmation window, and a truncation teach nothing, so
/// they keep the direct-write path.
#[derive(Default)]
struct DeliveryMemory {
    dropped: Mutex<HashSet<EditorKey>>,
}

/// How many dropping editors one session remembers. A session that binds more than
/// this many different dropping editors starts over instead of growing without
/// bound; starting over only means the direct write is tried again (the safe side).
const MAX_REMEMBERED_EDITORS: usize = 128;

impl DeliveryMemory {
    fn known_to_drop_set_value(&self, key: &EditorKey) -> bool {
        self.dropped
            .lock()
            .map(|known| known.contains(key))
            .unwrap_or(false)
    }

    fn remember_dropped(&self, key: &EditorKey) {
        if let Ok(mut known) = self.dropped.lock() {
            if known.len() >= MAX_REMEMBERED_EDITORS {
                known.clear();
            }
            known.insert(key.clone());
        }
    }
}

/// The editor operations one delivery needs. `NativeEditor` implements them with
/// real UIA and real key events; the unit tests implement them with a recording
/// editor, so the write decision (direct write vs paste-only) can be tested
/// without owning a desktop.
trait WritableEditor {
    /// The live draft as the editor's provider publishes it.
    fn read_value(&mut self) -> AppResult<String>;
    /// The same identity, enabled, writable and non-password checks the direct
    /// write has always done before any output is attempted.
    fn validate(&mut self) -> AppResult<()>;
    /// One write attempt; `true` when the provider accepted the call.
    fn write_value(&mut self, value: &str) -> bool;
    /// Waits for the accessible value to show the complete replacement or a stable
    /// truncated prefix. The write itself is never repeated.
    fn await_confirmation(&mut self, previous: &str, intended: &str) -> AppResult<Confirmation>;
    /// Appends the transcript with real editing keys and reads the result back.
    /// `known_draft` is the live draft the caller just read, when this delivery
    /// attempted no write, so the fallback does not read it a second time.
    fn paste_append(
        &mut self,
        expected: &str,
        replacement: &str,
        known_draft: Option<&str>,
        trace: &mut DeliveryTrace,
    ) -> AppResult<PasteOutcome>;
}

/// Output for one delivery. The direct write is tried once; only an editor this
/// session already saw appending through real editing keys goes straight to the
/// paste fallback, and only a fallback that really pasted teaches that fact.
fn replace_content<E: WritableEditor>(
    editor: &mut E,
    memory: &DeliveryMemory,
    key: &EditorKey,
    expected: &str,
    replacement: &str,
) -> AppResult<()> {
    let mut trace = DeliveryTrace::begin(key.label());
    let draft = editor.read_value()?;
    if draft != expected {
        return Err("The target draft changed during output; no replacement was attempted".into());
    }
    trace.lap("read");
    if memory.known_to_drop_set_value(key) {
        // This editor dropped the write earlier in this session, so no write is
        // attempted at all: there is nothing that could apply late, and the draft
        // just read is still the live draft. The fallback still reads the target
        // again for its own plan and confirms the appended result.
        editor.validate()?;
        trace.lap("validate");
        trace.style("paste_only");
        let outcome = editor.paste_append(expected, replacement, Some(&draft), &mut trace)?;
        trace.outcome(match outcome {
            PasteOutcome::NotNeeded => "already_complete",
            PasteOutcome::Appended => "appended_after_paste_only",
        });
        return Ok(());
    }
    let normalized = replacement.replace("\r\n", "\n").replace('\n', "\r\n");
    editor.validate()?;
    trace.lap("validate");
    // One write attempt only. A provider may accept it, drop it, or store a
    // truncated prefix when the editor has a smaller capacity.
    let written = editor.write_value(&normalized);
    trace.lap("write");
    if written {
        match editor.await_confirmation(expected, replacement)? {
            Confirmation::Confirmed => {
                trace.outcome("written");
                return Ok(());
            }
            // Never paste into an editor that just reported a capacity limit: the
            // fallback would append through the same limit.
            Confirmation::Truncated(detail) => {
                trace.outcome("truncated");
                return Err(detail);
            }
            Confirmation::Unconfirmed => {}
        }
        trace.lap("confirm");
    }
    // Some providers expose a Value pattern but drop the write (a controlled
    // rich-text composer restores its own state). Fall back to a real paste, which
    // produces ordinary editing events, while the target still owns the foreground
    // keyboard focus. Because a write was attempted, the fallback re-reads the draft
    // through the clipboard before sending any key.
    let outcome = editor.paste_append(expected, replacement, None, &mut trace)?;
    if outcome == PasteOutcome::Appended {
        // Only a fallback that really pasted proves that this editor drops the
        // write. `NotNeeded` means the late write landed, which keeps the fast path.
        memory.remember_dropped(key);
    }
    trace.outcome(match outcome {
        PasteOutcome::NotNeeded => "already_complete",
        PasteOutcome::Appended => "appended",
    });
    Ok(())
}

/// Outcome of waiting for the accessible value to show a completed write.
enum Confirmation {
    Confirmed,
    Truncated(String),
    Unconfirmed,
}

/// One delivery attempt's stage timings, appended as a JSON line to
/// `LISTEN_DELIVERY_TRACE` when that environment variable names a file.
///
/// It exists to measure backfill latency on a real target: the line records where
/// the delivery wait went (draft read, write attempt, confirmation polling,
/// clipboard backup, draft read, paste, read-back) plus the editor's process id and
/// UIA runtime id. Durations and numeric identities only; never the draft or the
/// transcript. Without the variable it costs one environment lookup and no I/O.
struct DeliveryTrace {
    path: Option<PathBuf>,
    editor: String,
    started: Instant,
    last: Instant,
    stages: Vec<(&'static str, u64)>,
    style: &'static str,
    outcome: &'static str,
}

impl DeliveryTrace {
    fn begin(editor: String) -> Self {
        let now = Instant::now();
        Self {
            path: std::env::var_os("LISTEN_DELIVERY_TRACE").map(PathBuf::from),
            editor,
            started: now,
            last: now,
            stages: Vec::new(),
            style: "direct",
            outcome: "failed",
        }
    }

    /// Records the time since the previous mark (or since the attempt started).
    fn lap(&mut self, stage: &'static str) {
        if self.path.is_none() {
            return;
        }
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        self.stages.push((stage, elapsed));
        self.last = now;
    }

    /// Why this attempt took the direct write path or the paste-only path.
    fn style(&mut self, style: &'static str) {
        self.style = style;
    }

    fn outcome(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

impl Drop for DeliveryTrace {
    fn drop(&mut self) {
        use std::io::Write;
        let Some(path) = self.path.take() else {
            return;
        };
        let total = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let stages = self
            .stages
            .iter()
            .map(|(stage, milliseconds)| format!("\"{stage}\":{milliseconds}"))
            .collect::<Vec<_>>()
            .join(",");
        let stages = if stages.is_empty() {
            String::new()
        } else {
            format!(",{stages}")
        };
        let line = format!(
            "{{\"editor\":\"{}\",\"style\":\"{}\",\"outcome\":\"{}\",\"total_ms\":{total}{stages}}}\n",
            self.editor, self.style, self.outcome
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

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

/// The foreground editor adapter. The selection is stateless; the one thing that
/// survives across captures is the per-editor memory of which editors drop
/// `ValuePattern.SetValue`, so repeated deliveries to the same target can skip the
/// futile write and its confirmation window.
pub struct NativeTargets {
    memory: Arc<DeliveryMemory>,
}

impl Default for NativeTargets {
    fn default() -> Self {
        Self {
            memory: Arc::new(DeliveryMemory::default()),
        }
    }
}

impl NativeTargets {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TargetSelection for NativeTargets {
    fn focused_target(&self) -> AppResult<Option<Box<dyn Target>>> {
        let Some(focus) = foreground_focus()? else {
            return Ok(None);
        };
        let memory = self.memory.clone();
        let (requests, receive) = mpsc::channel();
        let (ready, initialized) = mpsc::channel();
        thread::spawn(move || {
            // UIA objects are created, used and released in this one windowless MTA.
            let mut editor = match NativeEditor::bind(focus, memory) {
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
    /// The re-render-stable identity read when this editor was bound. A provider can
    /// replace its UIA node on a re-render; the runtime id alone then refuses the same
    /// visible editor.
    identity: ElementIdentity,
    /// The session-wide memory of editors that drop the direct write.
    memory: Arc<DeliveryMemory>,
    /// This editor's identity in that memory.
    key: EditorKey,
    // Field order releases UIA references and the hook before leaving COM.
    _hook: LifetimeHook,
    _com: Com,
}

impl NativeEditor {
    fn bind(focus: Focus, memory: Arc<DeliveryMemory>) -> AppResult<Option<Self>> {
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
            let control_type = element.CurrentControlType().map_err(|e| e.to_string())?;
            if !writable_editor_type(control_type.0) {
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
            // The re-render fallback in `is_bound_editor` only works when the provider
            // publishes a stable class or AutomationId. A missing property is not an
            // error at bind: the editor keeps the strict runtime-id identity instead.
            let identity = ElementIdentity {
                process: focus.process,
                root: focus.root,
                control_type: control_type.0,
                class_name: element
                    .CurrentClassName()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
                automation_id: element
                    .CurrentAutomationId()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            };
            let key = EditorKey {
                process: focus.process,
                runtime: runtime_id.clone(),
            };
            let editor = Self {
                focus,
                element,
                value,
                text,
                automation,
                runtime_id,
                identity,
                memory,
                key,
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

    fn replace(&mut self, expected: &str, replacement: &str) -> AppResult<()> {
        let memory = self.memory.clone();
        let key = self.key.clone();
        replace_content(self, &memory, &key, expected, replacement)
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
    ///
    /// `known_draft` is the live draft the caller already read on a delivery that
    /// attempted no write. It replaces the clipboard draft read, because no late
    /// provider write can race this fallback, and only the caret still has to be
    /// moved after the current text. A draft made only of line breaks is the one
    /// exception: a CEF/Electron composer reports an emptied input that way through
    /// UIA, while its clipboard copy is empty, so that draft is read through the
    /// clipboard before planning.
    fn paste_output(
        &self,
        expected: &str,
        replacement: &str,
        known_draft: Option<&str>,
        trace: &mut DeliveryTrace,
    ) -> AppResult<PasteOutcome> {
        self.validate_identity()?;
        // The draft this delivery appends to is also the value proof a re-rendered
        // focused node must still carry. A paste-only delivery already read it.
        let draft_hint = known_draft.unwrap_or(expected);
        // A lost focus gets one bounded restore of the bound window and element before
        // the fallback is refused; the per-key guard stays strict afterwards.
        self.ensure_bound_focus(draft_hint, replacement)?;
        let (draft, backup) = match known_draft {
            Some(known) => {
                trace.lap("preflight");
                // Never clear a clipboard whose previous content cannot be restored.
                let backup = paste::ClipboardBackup::save()?;
                trace.lap("backup");
                let guard = || self.require_foreground_focus(draft_hint, replacement);
                if line_breaks_only(known) {
                    // Planning from the line break alone would add a separator the
                    // editor cannot materialize after the paste, so the confirmation
                    // would report a landed paste as uncertain. Read the real draft
                    // (empty for an emptied box) instead; this one round trip only
                    // happens for a draft that is nothing but line breaks.
                    let draft = normalized(&paste::read_draft(&guard)?);
                    trace.lap("draft_read");
                    (draft, backup)
                } else {
                    // No write was attempted, so the draft the caller just read is the
                    // live draft. Select and collapse it to the end so the append lands
                    // after the current text, exactly like the clipboard read did.
                    paste::select_all(&guard)?;
                    trace.lap("select_all");
                    paste::collapse_to_end(&guard)?;
                    trace.lap("collapse");
                    (normalized(known), backup)
                }
            }
            None => {
                // A provider may have applied the earlier `SetValue` after the
                // confirmation window closed. Re-read before touching the clipboard
                // or sending any key.
                if normalized(&self.content()?).trim_end() == normalized(replacement).trim_end() {
                    trace.lap("preflight");
                    return Ok(PasteOutcome::NotNeeded);
                }
                trace.lap("preflight");
                let backup = paste::ClipboardBackup::save()?;
                trace.lap("backup");
                let guard = || self.require_foreground_focus(draft_hint, replacement);
                let draft = normalized(&paste::read_draft(&guard)?);
                trace.lap("draft_read");
                // The clipboard read used Ctrl+A and already collapsed the caret to
                // the end, so collapsing again would send the same key twice for no
                // observable difference.
                (draft, backup)
            }
        };
        match plan_paste(expected, replacement, &draft)? {
            // The real draft already contains the complete intended text (for
            // example a late or normalized provider write). Pasting again would
            // duplicate the transcript.
            PastePlan::AlreadyComplete | PastePlan::NothingToAppend => {
                drop(backup);
                Ok(PasteOutcome::NotNeeded)
            }
            PastePlan::Append { separator, text } => {
                let payload = format!("{separator}{text}");
                let wanted = format!("{draft}{payload}");
                // The guard now expects the value this very paste produces, so a
                // re-rendered node that already holds the appended text still proves
                // it is this delivery's editor before the read-back keys are sent.
                let guard = || self.require_foreground_focus(&draft, &wanted);
                paste::paste(&payload, &guard)?;
                trace.lap("paste");
                // The accessible value is the confirmation channel the direct-write
                // path already trusts. An exact match proves the complete append; a
                // provider that does not publish the pasted text in time falls back
                // to the clipboard read-back below.
                if self.confirm_pasted(&wanted, PASTE_CONFIRM_TIMEOUT)? {
                    trace.lap("read_back_accessibility");
                    drop(backup);
                    return Ok(PasteOutcome::Appended);
                }
                trace.lap("read_back_accessibility");
                // One clipboard reading is not proof: a Chromium (CEF/Electron) target
                // can apply the paste after the first Ctrl+A/Ctrl+C already copied the
                // previous draft. Retry inside a bounded window instead of declaring a
                // successful append uncertain on the first early reading.
                let confirmed = confirm_read_back(
                    &wanted,
                    PASTE_READ_BACK_TIMEOUT,
                    PASTE_READ_BACK_INTERVAL,
                    Instant::now,
                    || paste::read_back(&guard),
                    thread::sleep,
                )?;
                trace.lap("read_back");
                paste::collapse_to_end(&guard)?;
                trace.lap("collapse");
                drop(backup);
                if !confirmed {
                    return Err(
                        "The complete target output could not be confirmed after the fallback write"
                            .into(),
                    );
                }
                Ok(PasteOutcome::Appended)
            }
        }
    }

    /// Confirms a paste through the accessible value. Returns `false` when the
    /// provider does not publish the complete pasted text inside `timeout`; the
    /// caller then uses the clipboard read-back, which is the slow but universally
    /// available channel. A read error also returns `false` so the caller reports
    /// through the same read-back path.
    fn confirm_pasted(&self, wanted: &str, timeout: Duration) -> AppResult<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.content() {
                Ok(observed) if normalized(&observed).trim_end() == wanted.trim_end() => {
                    return Ok(true);
                }
                Ok(_) => {}
                Err(_) => return Ok(false),
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(CONFIRM_INTERVAL);
        }
    }

    /// The fallback writer sends real key events, so it may only run while the bound
    /// editor itself still owns the foreground keyboard focus.
    fn require_foreground_focus(&self, draft: &str, intended: &str) -> AppResult<()> {
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
        if self.is_bound_editor(&focused, draft, intended)? {
            return Ok(());
        }
        Err(
            "The focused control is not the original target, so the fallback writer was not used"
                .into(),
        )
    }

    /// Whether one UIA element may be treated as the bound editor. The runtime id is
    /// exact while the provider keeps the node alive. A different runtime id is only
    /// accepted when the element is the same logical editor (see
    /// [`same_logical_editor`]): a CEF/Electron host replaces the UIA node on a
    /// re-render while the visible input, its window, class and text stay the same.
    fn is_bound_editor(
        &self,
        focused: &IUIAutomationElement,
        draft: &str,
        intended: &str,
    ) -> AppResult<bool> {
        if runtime_id(focused)? == self.runtime_id {
            return Ok(true);
        }
        let candidate = element_identity(&self.automation, focused)?;
        let value = element_value(focused)?;
        Ok(same_logical_editor(
            &self.identity,
            &candidate,
            draft,
            intended,
            &value,
        ))
    }

    /// The one bounded attempt to give the bound editor its own foreground focus back
    /// before the fallback is refused. Only the window and element selected at capture
    /// start are touched: no other control is guessed, no other window is activated.
    fn restore_foreground_focus(&self) -> AppResult<()> {
        unsafe {
            let window = HWND(self.focus.window as *mut _);
            if GetForegroundWindow() != window {
                let _ = SetForegroundWindow(window);
            }
        }
        unsafe { self.element.SetFocus() }
            .map_err(|e| format!("Cannot restore the focus of the original target: {e}"))
    }

    /// Repeats the strict focus check after a restore attempt until the restored focus
    /// becomes observable or the bounded wait ends. Focus changes are asynchronous, so
    /// the first reading after `SetFocus` may still show the previous control.
    fn await_foreground_focus(&self, draft: &str, intended: &str) -> AppResult<()> {
        let deadline = Instant::now() + FOCUS_RECOVERY_TIMEOUT;
        loop {
            match self.require_foreground_focus(draft, intended) {
                Ok(()) => return Ok(()),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => thread::sleep(FOCUS_RECOVERY_INTERVAL),
            }
        }
    }

    /// The entry check of the fallback: verify the bound focus, restore it once when it
    /// was lost, and verify again. The per-key guard below keeps using the strict check,
    /// so a focus change after the fallback started still aborts before the next key
    /// instead of pulling the user's focus back mid-sequence.
    fn ensure_bound_focus(&self, draft: &str, intended: &str) -> AppResult<()> {
        ensure_foreground_focus(
            || self.require_foreground_focus(draft, intended),
            || self.restore_foreground_focus(),
            || self.await_foreground_focus(draft, intended),
        )
    }
}

/// Real UIA and key operations for `replace_content`.
impl WritableEditor for NativeEditor {
    fn read_value(&mut self) -> AppResult<String> {
        self.content()
    }

    fn validate(&mut self) -> AppResult<()> {
        self.validate_writable()
    }

    fn write_value(&mut self, value: &str) -> bool {
        unsafe { self.value.SetValue(&BSTR::from(value)).is_ok() }
    }

    fn await_confirmation(&mut self, previous: &str, intended: &str) -> AppResult<Confirmation> {
        self.confirmed(previous, intended)
    }

    fn paste_append(
        &mut self,
        expected: &str,
        replacement: &str,
        known_draft: Option<&str>,
        trace: &mut DeliveryTrace,
    ) -> AppResult<PasteOutcome> {
        self.paste_output(expected, replacement, known_draft, trace)
    }
}

/// Line endings are not part of the delivered text's meaning. Providers can report
/// the same content as CRLF or LF, so every comparison and the payload calculation
/// use this form.
fn normalized(value: &str) -> String {
    value.replace("\r\n", "\n")
}

/// Whether a draft consists only of line breaks. A CEF/Electron composer reports an
/// emptied input that way through UIA (the DOM keeps one empty paragraph), while
/// Ctrl+A/Ctrl+C copies no text at all and a paste lands as if the box were empty.
/// Planning from the line break would add a separator that the editor cannot
/// materialize, and the exact confirmation would then call a landed paste uncertain.
/// Only line breaks are special-cased: a draft holding spaces or text keeps its exact
/// value and the normal separator rule.
fn line_breaks_only(draft: &str) -> bool {
    !draft.is_empty() && draft.chars().all(|c| c == '\n' || c == '\r')
}

/// Repeats the clipboard read-back until it proves the complete appended value, or
/// until the bounded window that started with the first read-back closes. The paste
/// fallback used to read the editor back exactly once; on a CEF/Electron host the
/// paste can be applied after that read, so one early reading only shows the previous
/// draft and would turn a successful append into `uncertain`. Every attempt still goes
/// through the caller's focus guard, so a focus change stops the retry before the next
/// key. A window in which every read-back failed returns the last clipboard error, so
/// the caller reports the real cause instead of a bare "not confirmed".
///
/// `now` and `pause` are injected so the retry policy is tested without waiting.
fn confirm_read_back(
    wanted: &str,
    timeout: Duration,
    interval: Duration,
    now: impl Fn() -> Instant,
    mut read: impl FnMut() -> AppResult<String>,
    mut pause: impl FnMut(Duration),
) -> AppResult<bool> {
    let started = now();
    let mut last_error: Option<String>;
    loop {
        match read() {
            Ok(observed) if normalized(&observed).trim_end() == normalized(wanted).trim_end() => {
                return Ok(true);
            }
            Ok(_) => last_error = None,
            Err(error) => last_error = Some(error),
        }
        if now().duration_since(started) >= timeout {
            break;
        }
        pause(interval);
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(false),
    }
}

/// Check the bound editor's foreground keyboard focus before the fallback sends any
/// key. When the check fails, the bound window and the bound element are restored
/// once, and the check is repeated. Restoring never guesses another control: the
/// closures only know the target selected at capture start. A failed restore, or a
/// repeated check that still fails, keeps the first refusal's wording, so the user is
/// told what was wrong with the original target instead of reading about a recovery
/// attempt that is not the reason the fallback was refused.
fn ensure_foreground_focus(
    check: impl Fn() -> AppResult<()>,
    restore: impl Fn() -> AppResult<()>,
    recheck: impl Fn() -> AppResult<()>,
) -> AppResult<()> {
    match check() {
        Ok(()) => Ok(()),
        Err(refusal) => {
            // The restore is best effort: its own error is not the reason the
            // fallback is refused, so it never replaces the original wording.
            let _ = restore();
            recheck().map_err(|_| refusal)
        }
    }
}

/// The part of a UIA element's identity that survives a provider re-render. The
/// runtime id is a handle on one live UIA node: Chromium (CEF/Electron) and other
/// React hosts replace that node when the composer re-renders, so the same visible
/// input comes back with a new runtime id while its process, window, control type,
/// class, AutomationId and text are unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ElementIdentity {
    process: u32,
    /// The top-level window hosting the element. A browser keeps its whole DOM under
    /// one HWND, so this is necessary but never sufficient on its own.
    root: usize,
    control_type: i32,
    class_name: String,
    automation_id: String,
}

/// Whether `candidate` is the same logical editor as `bound` even though its UIA
/// runtime id changed. Every clause must hold, so a re-render can only be accepted
/// when nothing about the control is guessed:
///
/// * the same process and the same top-level window;
/// * the same UIA control type (`Edit`/`Document`), so an edit never stands in for
///   another kind of control;
/// * a published class name or AutomationId that matches, with at least one of them
///   non-empty. An anonymous control (both empty) keeps the strict runtime-id rule,
///   because nothing distinguishes it from a sibling of the same type;
/// * a published property never contradicts the bound one: if both sides publish a
///   class name (or an AutomationId), they must be equal;
/// * the candidate's live value still belongs to this delivery: it is the expected
///   draft, it continues that draft (the user typed after it), or it is the complete
///   intended value that the paste just produced. A value that diverged from both is
///   someone else's content, so no key is sent.
fn same_logical_editor(
    bound: &ElementIdentity,
    candidate: &ElementIdentity,
    draft: &str,
    intended: &str,
    candidate_value: &str,
) -> bool {
    if bound.process != candidate.process || bound.root != candidate.root {
        return false;
    }
    if bound.control_type != candidate.control_type {
        return false;
    }
    let same_class = !bound.class_name.is_empty() && bound.class_name == candidate.class_name;
    let same_id = !bound.automation_id.is_empty() && bound.automation_id == candidate.automation_id;
    if !same_class && !same_id {
        return false;
    }
    let contradicts = (!bound.class_name.is_empty()
        && !candidate.class_name.is_empty()
        && bound.class_name != candidate.class_name)
        || (!bound.automation_id.is_empty()
            && !candidate.automation_id.is_empty()
            && bound.automation_id != candidate.automation_id);
    if contradicts {
        return false;
    }
    let draft = normalized(draft);
    let draft = draft.trim_end();
    let intended = normalized(intended);
    let intended = intended.trim_end();
    let value = normalized(candidate_value);
    let value = value.trim_end();
    value == draft || (!draft.is_empty() && value.starts_with(draft)) || value == intended
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
    let appended = appended_part(&expected, &replacement).ok_or_else(|| {
        "The intended target value does not start with the draft read at stop; the fallback writer refused to paste to avoid duplicating the original draft"
            .to_string()
    })?;
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
            separator: separator(&draft),
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
        separator: separator(&draft),
        text: appended.to_string(),
    })
}

/// The nearest native window that hosts a UIA element, resolved to its root. Chromium
/// exposes page editors without a native window handle of their own, so this parent
/// window is the only window identity they have.
struct HostWindow {
    root: usize,
    process: u32,
}

fn host_window(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
) -> AppResult<HostWindow> {
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
                    return Ok(HostWindow {
                        root: GetAncestor(hwnd, GA_ROOT).0 as usize,
                        process,
                    });
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

/// Confirms that the focused element lives inside the window selected at capture start.
/// Chromium exposes page editors without a native window handle, so the nearest ancestor
/// window is checked instead of relying on the HWND that hosts the whole page.
fn belongs_to_root(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    focus: &Focus,
) -> AppResult<()> {
    let host = host_window(automation, element)?;
    if host.process != focus.process || host.root != focus.root || host.root == 0 {
        return Err("The selected editor no longer belongs to the foreground window".into());
    }
    Ok(())
}

/// The re-render-stable identity of one UIA element. Only the class name and the
/// AutomationId are read as strings; a property the provider does not publish stays
/// empty, and an empty property never proves identity on its own.
fn element_identity(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
) -> AppResult<ElementIdentity> {
    let host = host_window(automation, element)?;
    unsafe {
        let process = element
            .CurrentProcessId()
            .map_err(|e| format!("Cannot identify the focused control: {e}"))?
            as u32;
        let control_type = element
            .CurrentControlType()
            .map_err(|e| format!("Cannot identify the focused control: {e}"))?;
        Ok(ElementIdentity {
            process,
            root: host.root,
            control_type: control_type.0,
            class_name: element
                .CurrentClassName()
                .map(|name| name.to_string())
                .unwrap_or_default(),
            automation_id: element
                .CurrentAutomationId()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        })
    }
}

/// The live value of one focused element. A control whose value cannot be read cannot
/// prove that it still holds this delivery's draft, so the caller refuses it instead.
fn element_value(element: &IUIAutomationElement) -> AppResult<String> {
    unsafe {
        let value: IUIAutomationValuePattern = element
            .GetCurrentPatternAs(UIA_ValuePatternId)
            .map_err(|e| format!("Cannot read the focused control's value: {e}"))?;
        value
            .CurrentValue()
            .map(|value| value.to_string())
            .map_err(|e| format!("Cannot read the focused control's value: {e}"))
    }
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
    fn the_direct_write_and_the_paste_fallback_use_the_one_append_value() {
        use crate::target_write::{append_value, appended_part, separator};
        for (draft, text) in [
            ("", "今天轉寫"),
            ("原有草稿", "今天轉寫"),
            ("第一行\r\n第二行", "今天轉寫"),
        ] {
            let replacement = append_value(draft, text);
            // The value one direct write stores is exactly the value the paste
            // fallback would paste from the same draft: one separator, one rule.
            assert_eq!(
                plan_paste(draft, &replacement, draft).unwrap(),
                PastePlan::Append {
                    separator: separator(draft),
                    text: text.into(),
                }
            );
            // The appended part is recovered from that same value, so the two paths
            // cannot disagree about where the transcript starts.
            assert_eq!(appended_part(draft, &replacement), Some(text));
        }
    }

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

    /// What one direct write attempt does to the recording editor.
    #[derive(Clone, Copy)]
    enum FakeWrite {
        /// The provider stores the complete new value.
        Applied,
        /// The provider reports success but keeps its own draft.
        Dropped,
        /// The provider applies the value during the confirmation wait.
        AppliedLate,
        /// The provider stores only a prefix of the value (capacity limit).
        Truncated(usize),
    }

    /// A recording editor for the write-decision tests. It mimics what the real
    /// adapter does — read the draft, accept or drop one write, wait for the write,
    /// then append through the fallback — without owning a desktop.
    struct FakeEditor {
        draft: String,
        write: FakeWrite,
        pending: Option<String>,
        calls: Vec<&'static str>,
        writes: usize,
        pastes: usize,
        clipboard_draft_reads: usize,
    }

    impl FakeEditor {
        fn new(draft: &str, write: FakeWrite) -> Self {
            Self {
                draft: draft.into(),
                write,
                pending: None,
                calls: Vec::new(),
                writes: 0,
                pastes: 0,
                clipboard_draft_reads: 0,
            }
        }
    }

    impl WritableEditor for FakeEditor {
        fn read_value(&mut self) -> AppResult<String> {
            self.calls.push("read");
            Ok(self.draft.clone())
        }

        fn validate(&mut self) -> AppResult<()> {
            self.calls.push("validate");
            Ok(())
        }

        fn write_value(&mut self, value: &str) -> bool {
            self.calls.push("write");
            self.writes += 1;
            match self.write {
                FakeWrite::Applied => self.draft = normalized(value),
                FakeWrite::Dropped => {}
                FakeWrite::AppliedLate => self.pending = Some(normalized(value)),
                FakeWrite::Truncated(limit) => {
                    self.draft = normalized(value).chars().take(limit).collect();
                }
            }
            true
        }

        fn await_confirmation(
            &mut self,
            _previous: &str,
            intended: &str,
        ) -> AppResult<Confirmation> {
            self.calls.push("confirm");
            match self.write {
                FakeWrite::Applied | FakeWrite::AppliedLate => {
                    if let Some(pending) = self.pending.take() {
                        self.draft = pending;
                    }
                    Ok(Confirmation::Confirmed)
                }
                FakeWrite::Dropped => Ok(Confirmation::Unconfirmed),
                FakeWrite::Truncated(_) => Ok(Confirmation::Truncated(format!(
                    "The original target accepted only {} of {} characters; the appended text was truncated",
                    self.draft.chars().count(),
                    normalized(intended).chars().count()
                ))),
            }
        }

        fn paste_append(
            &mut self,
            expected: &str,
            replacement: &str,
            known_draft: Option<&str>,
            _trace: &mut DeliveryTrace,
        ) -> AppResult<PasteOutcome> {
            self.calls.push("paste");
            let draft = match known_draft {
                // The delivery that attempted a write re-reads the draft through the
                // clipboard; a paste-only delivery reuses the draft it just read.
                Some(known) => normalized(known),
                None => {
                    self.clipboard_draft_reads += 1;
                    normalized(&self.draft)
                }
            };
            match plan_paste(expected, replacement, &draft)? {
                PastePlan::AlreadyComplete | PastePlan::NothingToAppend => {
                    Ok(PasteOutcome::NotNeeded)
                }
                PastePlan::Append { separator, text } => {
                    self.draft = format!("{}{separator}{text}", self.draft);
                    self.pastes += 1;
                    Ok(PasteOutcome::Appended)
                }
            }
        }
    }

    fn editor_key() -> EditorKey {
        EditorKey {
            process: 4242,
            runtime: vec![42, 9001],
        }
    }

    #[test]
    fn the_first_delivery_to_a_target_that_drops_the_write_tries_once_then_pastes() {
        // #25: the first delivery keeps the existing behaviour. One direct write is
        // attempted, its confirmation window passes, and the real fallback appends.
        let mut editor = FakeEditor::new("原有草稿", FakeWrite::Dropped);
        let memory = DeliveryMemory::default();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿",
            "原有草稿\n第一行",
        )
        .unwrap();
        assert_eq!(
            editor.calls,
            ["read", "validate", "write", "confirm", "paste"]
        );
        assert_eq!(editor.draft, "原有草稿\n第一行");
        assert_eq!(editor.writes, 1);
        assert_eq!(editor.pastes, 1);
        assert!(
            memory.known_to_drop_set_value(&editor_key()),
            "a fallback that really pasted is what teaches the session"
        );
    }

    #[test]
    fn a_second_delivery_to_the_same_dropping_target_skips_the_write() {
        // #25: once this session has seen the editor append through real editing
        // keys, the next delivery goes straight to the fallback, so the 2 s
        // confirmation window is never entered again.
        let mut editor = FakeEditor::new("原有草稿", FakeWrite::Dropped);
        let memory = DeliveryMemory::default();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿",
            "原有草稿\n第一行",
        )
        .unwrap();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿\n第一行",
            "原有草稿\n第一行\n第二行",
        )
        .unwrap();
        assert_eq!(
            editor.calls,
            [
                "read", "validate", "write", "confirm", "paste", "read", "validate", "paste"
            ],
            "the second delivery must not attempt the direct write"
        );
        assert_eq!(editor.writes, 1);
        assert_eq!(editor.pastes, 2);
        assert_eq!(
            editor.clipboard_draft_reads, 1,
            "only the delivery that attempted a write needs the clipboard draft read"
        );
        assert_eq!(editor.draft, "原有草稿\n第一行\n第二行");
        assert_eq!(
            editor.draft.matches("第一行").count(),
            1,
            "no part of a transcript may be written twice"
        );
    }

    #[test]
    fn a_target_that_writes_directly_keeps_the_direct_path_on_every_delivery() {
        let mut editor = FakeEditor::new("原有草稿", FakeWrite::Applied);
        let memory = DeliveryMemory::default();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿",
            "原有草稿\n第一行",
        )
        .unwrap();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿\n第一行",
            "原有草稿\n第一行\n第二行",
        )
        .unwrap();
        assert_eq!(
            editor.calls,
            [
                "read", "validate", "write", "confirm", "read", "validate", "write", "confirm"
            ]
        );
        assert_eq!(editor.writes, 2);
        assert_eq!(editor.pastes, 0);
        assert!(!memory.known_to_drop_set_value(&editor_key()));
    }

    #[test]
    fn a_late_write_is_confirmed_without_a_paste_and_teaches_nothing() {
        // A provider that applies the write during the confirmation window is not a
        // dropping provider. Pasting would duplicate the transcript, and remembering
        // it would needlessly move a direct-write target onto the paste path.
        let mut editor = FakeEditor::new("原有草稿", FakeWrite::AppliedLate);
        let memory = DeliveryMemory::default();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿",
            "原有草稿\n第一行",
        )
        .unwrap();
        assert_eq!(editor.pastes, 0);
        assert_eq!(editor.draft, "原有草稿\n第一行");
        assert!(!memory.known_to_drop_set_value(&editor_key()));
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿\n第一行",
            "原有草稿\n第一行\n第二行",
        )
        .unwrap();
        assert_eq!(
            editor.writes, 2,
            "the next delivery still uses the direct write"
        );
    }

    #[test]
    fn a_truncated_write_is_reported_without_pasting_and_is_not_remembered() {
        let mut editor = FakeEditor::new("capacity draft", FakeWrite::Truncated(20));
        let memory = DeliveryMemory::default();
        let error = replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "capacity draft",
            "capacity draft\nthe appended transcript",
        )
        .unwrap_err();
        assert!(error.contains("truncated"), "{error}");
        assert_eq!(
            editor.pastes, 0,
            "never paste into an editor that reported a capacity limit"
        );
        assert!(!memory.known_to_drop_set_value(&editor_key()));
    }

    #[test]
    fn a_changed_draft_blocks_a_repeat_delivery_before_any_key_is_sent() {
        let mut editor = FakeEditor::new("原有草稿", FakeWrite::Dropped);
        let memory = DeliveryMemory::default();
        replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "原有草稿",
            "原有草稿\n第一行",
        )
        .unwrap();
        let error = replace_content(
            &mut editor,
            &memory,
            &editor_key(),
            "a stale draft",
            "a stale draft\n第二行",
        )
        .unwrap_err();
        assert!(error.contains("changed"), "{error}");
        assert_eq!(
            editor.pastes, 1,
            "nothing may be appended when the draft is not the expected one"
        );
        assert_eq!(editor.draft, "原有草稿\n第一行");
    }

    #[test]
    fn only_line_breaks_are_treated_as_an_empty_composer_representation() {
        // The CEF composer reports one empty paragraph as a line break through UIA.
        assert!(line_breaks_only("\n"));
        assert!(line_breaks_only("\r\n"));
        // An actually empty draft already takes the clipboard-read path.
        assert!(!line_breaks_only(""));
        // Spaces and text keep their exact value and the normal separator rule.
        assert!(!line_breaks_only(" "));
        assert!(!line_breaks_only("draft\n"));
    }

    #[test]
    fn a_read_back_that_is_too_early_is_retried_until_the_pasted_text_appears() {
        use std::cell::Cell;
        let base = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0);
        let confirmed = confirm_read_back(
            "原有草稿\n今天轉寫",
            Duration::from_secs(1),
            Duration::from_millis(150),
            || base + elapsed.get(),
            || {
                attempts.set(attempts.get() + 1);
                // The first clipboard reads race the paste and still hold the draft.
                match attempts.get() {
                    1 | 2 => Ok("原有草稿".to_string()),
                    _ => Ok("原有草稿\r\n今天轉寫".to_string()),
                }
            },
            |interval| elapsed.set(elapsed.get() + interval),
        )
        .unwrap();
        assert!(confirmed);
        assert_eq!(
            attempts.get(),
            3,
            "the read-back is retried instead of being trusted once"
        );
    }

    #[test]
    fn the_read_back_retries_stop_inside_the_bounded_window() {
        use std::cell::Cell;
        let base = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0);
        let confirmed = confirm_read_back(
            "原有草稿\n今天轉寫",
            Duration::from_secs(1),
            Duration::from_millis(150),
            || base + elapsed.get(),
            || {
                attempts.set(attempts.get() + 1);
                // One Ctrl+A/Ctrl+C read-back costs two key settles, about 300 ms.
                elapsed.set(elapsed.get() + Duration::from_millis(300));
                Ok("原有草稿".to_string())
            },
            |interval| elapsed.set(elapsed.get() + interval),
        )
        .unwrap();
        assert!(
            !confirmed,
            "an unchanged target is never reported as confirmed"
        );
        assert_eq!(
            attempts.get(),
            3,
            "the retry stops once the one second window has passed"
        );
    }

    #[test]
    fn a_read_back_that_cannot_read_the_clipboard_fails_with_the_last_error() {
        use std::cell::Cell;
        let base = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = Cell::new(0);
        let error = confirm_read_back(
            "wanted",
            Duration::from_secs(1),
            Duration::from_millis(150),
            || base + elapsed.get(),
            || {
                attempts.set(attempts.get() + 1);
                Err("Cannot open the clipboard: busy".to_string())
            },
            |interval| elapsed.set(elapsed.get() + interval),
        )
        .unwrap_err();
        assert!(error.contains("busy"), "{error}");
        assert!(
            attempts.get() > 1,
            "a transient clipboard error is retried inside the window"
        );
    }

    #[test]
    fn a_bound_editor_that_still_owns_the_focus_is_not_restored() {
        use std::cell::Cell;
        let restores = Cell::new(0);
        ensure_foreground_focus(
            || Ok(()),
            || {
                restores.set(restores.get() + 1);
                Ok(())
            },
            || panic!("a passing focus check must not be repeated"),
        )
        .unwrap();
        assert_eq!(restores.get(), 0);
    }

    #[test]
    fn a_lost_focus_is_restored_once_and_only_after_it_was_lost() {
        use std::cell::Cell;
        let checks = Cell::new(0);
        let restores = Cell::new(0);
        ensure_foreground_focus(
            || {
                checks.set(checks.get() + 1);
                Err("The focused control is not the original target".to_string())
            },
            || {
                restores.set(restores.get() + 1);
                Ok(())
            },
            || {
                checks.set(checks.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(checks.get(), 2, "the restore attempt is verified once");
        assert_eq!(restores.get(), 1, "the bound target is restored once");
    }

    #[test]
    fn a_focus_that_cannot_be_restored_keeps_the_original_refusal() {
        let original =
            "The focused control is not the original target, so the fallback writer was not used";
        // A restore attempt that fails is not the refusal; the original wording stays.
        let error = ensure_foreground_focus(
            || Err(original.to_string()),
            || Err("Cannot restore the original target's focus".to_string()),
            || Err("The original target is no longer the foreground window".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, original);
        // A successful restore whose verification still fails is the same refusal.
        let error = ensure_foreground_focus(
            || Err(original.to_string()),
            || Ok(()),
            || Err("The focused control is not the original target".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, original);
    }

    fn bound_identity() -> ElementIdentity {
        ElementIdentity {
            process: 4242,
            root: 0x1_0000,
            control_type: UIA_EditControlTypeId.0,
            class_name: "GRgSLq_input".into(),
            automation_id: String::new(),
        }
    }

    #[test]
    fn a_rerendered_uia_node_of_the_same_editor_is_the_same_target() {
        let bound = bound_identity();
        let candidate = bound.clone();
        assert!(same_logical_editor(
            &bound,
            &candidate,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿"
        ));
        // The user typed after the draft between the read and the fallback.
        assert!(same_logical_editor(
            &bound,
            &candidate,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿 plus later typing"
        ));
        // After the paste the same node holds the complete appended value.
        assert!(same_logical_editor(
            &bound,
            &candidate,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿\r\n今天轉寫"
        ));
        // A clean input box: before the paste it is empty, after it holds the text.
        assert!(same_logical_editor(&bound, &candidate, "", "今天轉寫", ""));
        assert!(same_logical_editor(
            &bound,
            &candidate,
            "",
            "今天轉寫",
            "今天轉寫"
        ));
    }

    #[test]
    fn a_control_in_another_window_or_process_is_never_the_same_target() {
        let bound = bound_identity();
        let mut other_window = bound.clone();
        other_window.root += 0x1000;
        assert!(!same_logical_editor(
            &bound,
            &other_window,
            "",
            "今天轉寫",
            "今天轉寫"
        ));
        let mut other_process = bound.clone();
        other_process.process += 1;
        assert!(!same_logical_editor(
            &bound,
            &other_process,
            "",
            "今天轉寫",
            "今天轉寫"
        ));
    }

    #[test]
    fn a_control_of_another_type_class_or_automation_id_is_never_the_same_target() {
        let bound = bound_identity();
        let mut other_type = bound.clone();
        other_type.control_type = UIA_DocumentControlTypeId.0;
        assert!(!same_logical_editor(
            &bound,
            &other_type,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿"
        ));
        let mut other_class = bound.clone();
        other_class.class_name = "other_input".into();
        assert!(!same_logical_editor(
            &bound,
            &other_class,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿"
        ));
        let with_id = ElementIdentity {
            automation_id: "composer".into(),
            ..bound.clone()
        };
        let mut other_id = with_id.clone();
        other_id.automation_id = "search".into();
        assert!(!same_logical_editor(
            &with_id,
            &other_id,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "原有草稿"
        ));
    }

    #[test]
    fn an_anonymous_control_keeps_the_strict_runtime_identity() {
        let anonymous = ElementIdentity {
            class_name: String::new(),
            automation_id: String::new(),
            ..bound_identity()
        };
        // Nothing but the runtime id tells this control apart from a sibling.
        assert!(!same_logical_editor(
            &anonymous,
            &anonymous,
            "",
            "今天轉寫",
            "今天轉寫"
        ));
    }

    #[test]
    fn a_candidate_whose_value_left_this_delivery_is_refused() {
        let bound = bound_identity();
        assert!(!same_logical_editor(
            &bound,
            &bound,
            "原有草稿",
            "原有草稿\n今天轉寫",
            "另一个输入框的内容"
        ));
        assert!(!same_logical_editor(
            &bound,
            &bound,
            "",
            "今天轉寫",
            "另一个输入框的内容"
        ));
    }
}

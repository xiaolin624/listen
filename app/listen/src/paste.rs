//! Clipboard and keyboard fallback for editors whose UIA value writer silently
//! drops the write (for example a controlled rich-text composer that exposes a
//! Value pattern but replaces its own state on the next render).
//!
//! The fallback never replaces the user's draft: it reads the real draft, pastes
//! the new text at the end, confirms by reading the editor back, and restores the
//! previous clipboard text. Only selection, paste and copy keys are sent — never
//! Enter, so nothing can be submitted.
//!
//! Every key event is preceded by the caller's target guard (foreground window and
//! focused element identity). A guard failure aborts the sequence before the next
//! key, so a focus change during the roughly one-second fallback cannot send the
//! remaining chords to another control.
//!
//! The clipboard is only borrowed when its previous content can be restored. A
//! non-text clip (files, images) or an unreadable clip aborts the fallback rather
//! than clearing content the user copied.

use crate::AppResult;
use std::{thread, time::Duration};
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
            IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
    },
    UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY, VK_A, VK_C, VK_CONTROL, VK_RIGHT, VK_V,
    },
};

/// CF_UNICODETEXT. The windows crate keeps the Win32 constant in another module.
const CF_UNICODETEXT: u32 = 13;

const KEY_SETTLE: Duration = Duration::from_millis(140);

struct Clipboard;

impl Clipboard {
    fn open() -> AppResult<Self> {
        unsafe { OpenClipboard(None) }.map_err(|e| format!("Cannot open the clipboard: {e}"))?;
        Ok(Self)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Reads the clipboard text. Callers must hold the clipboard open.
fn read_text_open() -> AppResult<Option<String>> {
    unsafe {
        let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
            return Err("Cannot read the clipboard text".into());
        };
        let global = HGLOBAL(handle.0);
        let pointer = GlobalLock(global) as *const u16;
        if pointer.is_null() {
            return Err("Cannot lock the clipboard text".into());
        }
        let units = GlobalSize(global) / 2;
        let mut length = 0;
        while length < units && *pointer.add(length) != 0 {
            length += 1;
        }
        let slice = std::slice::from_raw_parts(pointer, length);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(global);
        Ok(Some(text))
    }
}

fn read_text() -> AppResult<Option<String>> {
    let _open = Clipboard::open()?;
    if !unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_ok() {
        return Ok(None);
    }
    read_text_open()
}

fn write_text(text: &str) -> AppResult<()> {
    let _open = Clipboard::open()?;
    unsafe {
        EmptyClipboard().map_err(|e| format!("Cannot clear the clipboard: {e}"))?;
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = wide.len() * std::mem::size_of::<u16>();
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes)
            .map_err(|e| format!("Cannot allocate clipboard text: {e}"))?;
        let pointer = GlobalLock(global) as *mut u16;
        if pointer.is_null() {
            return Err("Cannot lock clipboard text".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
        let _ = GlobalUnlock(global);
        // After a successful SetClipboardData the system owns the allocation.
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(global.0)))
            .map_err(|e| format!("Cannot set the clipboard text: {e}"))?;
    }
    Ok(())
}

/// Copies `text` to the system clipboard for the expanded panel's
/// 复制完整结果 button. It only writes the clipboard: it never pastes, never sends
/// input and never touches a target, so the "append only, never send" rule holds.
pub fn copy_to_clipboard(text: &str) -> AppResult<()> {
    write_text(text)
}

fn clear() {
    if let Ok(_open) = Clipboard::open() {
        unsafe {
            let _ = EmptyClipboard();
        }
    }
}

/// The clipboard slot before the fallback writer ran. Restoring is only possible
/// for text (`Text`) or for an empty slot (`Empty`); any other content makes
/// `ClipboardBackup::save` fail before a key is sent.
#[derive(Debug, PartialEq, Eq)]
enum ClipboardContents {
    Empty,
    Text(String),
}

/// Decides whether the clipboard can be borrowed safely. `any_format` says the
/// slot holds at least one format; `text_format` says CF_UNICODETEXT is offered.
fn classify_clipboard(
    any_format: bool,
    text_format: bool,
    read: AppResult<Option<String>>,
) -> AppResult<ClipboardContents> {
    if !any_format {
        return Ok(ClipboardContents::Empty);
    }
    if !text_format {
        return Err(
            "The system clipboard holds non-text data (for example copied files or an image); the paste fallback was not used and that content was left untouched. Copy the complete result from the expanded result panel instead."
                .into(),
        );
    }
    match read {
        Ok(Some(text)) => Ok(ClipboardContents::Text(text)),
        // The text format disappeared or could not be read. Do not clear a slot
        // whose current content could not be captured.
        other => {
            let detail = match other {
                Err(error) => format!(" ({error})"),
                _ => String::new(),
            };
            Err(format!(
                "The system clipboard text could not be read{detail}, so the paste fallback was not used and the clipboard was left untouched. Copy the complete result from the expanded result panel instead."
            ))
        }
    }
}

pub struct ClipboardBackup(ClipboardContents);

impl ClipboardBackup {
    /// Captures the clipboard so it can be restored. Failure means the fallback
    /// must not run; the caller reports the complete result in the copy box.
    pub fn save() -> AppResult<Self> {
        let _open = Clipboard::open()?;
        let any_format = unsafe { EnumClipboardFormats(0) } != 0;
        let text_format = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_ok();
        let read = if text_format {
            read_text_open()
        } else {
            Ok(None)
        };
        Ok(Self(classify_clipboard(any_format, text_format, read)?))
    }
}

impl Drop for ClipboardBackup {
    fn drop(&mut self) {
        match &self.0 {
            ClipboardContents::Text(text) => {
                let _ = write_text(text);
            }
            ClipboardContents::Empty => clear(),
        }
    }
}

/// One key sink. The real implementation injects input; tests record events.
pub(crate) trait KeySink {
    /// Returns whether the OS accepted the input event.
    fn send(&mut self, virtual_key: VIRTUAL_KEY, release: bool) -> bool;
}

struct RealKeys;

impl KeySink for RealKeys {
    fn send(&mut self, virtual_key: VIRTUAL_KEY, release: bool) -> bool {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: virtual_key,
                    wScan: 0,
                    dwFlags: if release {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 1 }
    }
}

/// Sends keys only while `guard` still accepts the bound target. The guard is
/// evaluated immediately before every single key down/up, not once per chord.
pub(crate) struct Keys<'a> {
    sink: &'a mut dyn KeySink,
    guard: &'a dyn Fn() -> AppResult<()>,
}

impl Keys<'_> {
    fn key(&mut self, virtual_key: VIRTUAL_KEY, release: bool) -> AppResult<()> {
        (self.guard)()?;
        if !self.sink.send(virtual_key, release) {
            return Err(
                "The system blocked the fallback key input (for example because the secure desktop or another integrity level owns the input); the fallback writer was stopped"
                    .into(),
            );
        }
        Ok(())
    }

    fn chord(&mut self, modifier: VIRTUAL_KEY, key_code: VIRTUAL_KEY) -> AppResult<()> {
        self.key(modifier, false)?;
        self.key(key_code, false)?;
        self.key(key_code, true)?;
        self.key(modifier, true)?;
        thread::sleep(KEY_SETTLE);
        Ok(())
    }

    fn tap(&mut self, key_code: VIRTUAL_KEY) -> AppResult<()> {
        self.key(key_code, false)?;
        self.key(key_code, true)?;
        thread::sleep(KEY_SETTLE);
        Ok(())
    }
}

fn select_all_copy(keys: &mut Keys<'_>) -> AppResult<()> {
    keys.chord(VK_CONTROL, VK_A)?;
    keys.chord(VK_CONTROL, VK_C)
}

/// Selects the editor's whole content, copies it and collapses the caret to the
/// end, returning the real draft as the application sees it.
pub fn read_draft(guard: &dyn Fn() -> AppResult<()>) -> AppResult<String> {
    // Copying an empty selection leaves the previous clipboard content in place,
    // so the slot is cleared first and an empty editor reads back as empty. The
    // caller has already captured a restorable backup.
    clear();
    let mut sink = RealKeys;
    let mut keys = Keys {
        sink: &mut sink,
        guard,
    };
    select_all_copy(&mut keys)?;
    let draft = read_text()?.unwrap_or_default();
    keys.tap(VK_RIGHT)?;
    Ok(draft)
}

/// Pastes `text` at the current caret position.
pub fn paste(text: &str, guard: &dyn Fn() -> AppResult<()>) -> AppResult<()> {
    // Do not overwrite the clipboard if the target already lost focus.
    (guard)()?;
    write_text(text)?;
    thread::sleep(KEY_SETTLE);
    let mut sink = RealKeys;
    let mut keys = Keys {
        sink: &mut sink,
        guard,
    };
    keys.chord(VK_CONTROL, VK_V)?;
    thread::sleep(Duration::from_millis(220));
    Ok(())
}

/// Selects the editor's whole content and copies it, returning what is now there.
pub fn read_back(guard: &dyn Fn() -> AppResult<()>) -> AppResult<String> {
    clear();
    let mut sink = RealKeys;
    let mut keys = Keys {
        sink: &mut sink,
        guard,
    };
    select_all_copy(&mut keys)?;
    Ok(read_text()?.unwrap_or_default())
}

/// Collapses a full selection to its end so the user keeps a caret, not a selection.
pub fn collapse_to_end(guard: &dyn Fn() -> AppResult<()>) -> AppResult<()> {
    let mut sink = RealKeys;
    let mut keys = Keys {
        sink: &mut sink,
        guard,
    };
    keys.tap(VK_RIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Recording {
        events: Vec<(u16, bool)>,
    }

    impl KeySink for Recording {
        fn send(&mut self, virtual_key: VIRTUAL_KEY, release: bool) -> bool {
            self.events.push((virtual_key.0, release));
            true
        }
    }

    #[test]
    fn a_failed_guard_aborts_before_the_next_key_event() {
        let calls = AtomicUsize::new(0);
        let guard = || {
            // Accept the first key down, reject the second.
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err("focus left the original target".to_string())
            }
        };
        let mut sink = Recording::default();
        let mut keys = Keys {
            sink: &mut sink,
            guard: &guard,
        };
        let error = keys.chord(VK_CONTROL, VK_A).unwrap_err();
        assert!(error.contains("focus left"), "{error}");
        assert_eq!(
            sink.events,
            [(VK_CONTROL.0, false)],
            "only the key that passed the guard may be injected"
        );
    }

    #[test]
    fn every_key_event_is_guarded_while_the_guard_keeps_accepting() {
        let calls = AtomicUsize::new(0);
        let guard = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let mut sink = Recording::default();
        let mut keys = Keys {
            sink: &mut sink,
            guard: &guard,
        };
        keys.chord(VK_CONTROL, VK_A).unwrap();
        keys.tap(VK_RIGHT).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        assert_eq!(sink.events.len(), 6);
    }

    #[test]
    fn a_blocked_key_event_is_reported_instead_of_continuing() {
        struct Blocked;
        impl KeySink for Blocked {
            fn send(&mut self, _: VIRTUAL_KEY, _: bool) -> bool {
                false
            }
        }
        let guard = || Ok(());
        let mut sink = Blocked;
        let mut keys = Keys {
            sink: &mut sink,
            guard: &guard,
        };
        let error = keys.chord(VK_CONTROL, VK_A).unwrap_err();
        assert!(error.contains("blocked"), "{error}");
    }

    #[test]
    fn non_text_and_unreadable_clips_are_never_cleared() {
        let error = classify_clipboard(true, false, Ok(None)).unwrap_err();
        assert!(error.contains("non-text"), "{error}");
        assert!(error.contains("left untouched"), "{error}");

        let error = classify_clipboard(true, true, Err("clipboard locked".into())).unwrap_err();
        assert!(error.contains("could not be read"), "{error}");

        let error = classify_clipboard(true, true, Ok(None)).unwrap_err();
        assert!(error.contains("could not be read"), "{error}");
    }

    #[test]
    fn only_an_empty_or_text_clipboard_is_borrowed() {
        assert_eq!(
            classify_clipboard(false, false, Ok(None)).unwrap(),
            ClipboardContents::Empty
        );
        assert_eq!(
            classify_clipboard(true, true, Ok(Some("previous draft".into()))).unwrap(),
            ClipboardContents::Text("previous draft".into())
        );
    }
}

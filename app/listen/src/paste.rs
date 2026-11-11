//! Clipboard and keyboard fallback for editors whose UIA value writer silently
//! drops the write (for example a controlled rich-text composer that exposes a
//! Value pattern but replaces its own state on the next render).
//!
//! The fallback never replaces the user's draft: it reads the real draft, pastes
//! the new text at the end, confirms by reading the editor back, and restores the
//! previous clipboard text. Only selection, paste and copy keys are sent — never
//! Enter, so nothing can be submitted.

use crate::AppResult;
use std::{thread, time::Duration};
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
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

fn read_text() -> AppResult<Option<String>> {
    let _open = Clipboard::open()?;
    unsafe {
        let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
            return Ok(None);
        };
        let global = HGLOBAL(handle.0);
        let pointer = GlobalLock(global) as *const u16;
        if pointer.is_null() {
            return Err("Cannot read the clipboard text".into());
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

fn clear() {
    if let Ok(_open) = Clipboard::open() {
        unsafe {
            let _ = EmptyClipboard();
        }
    }
}

fn key(virtual_key: VIRTUAL_KEY, release: bool) {
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
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn chord(modifier: VIRTUAL_KEY, key_code: VIRTUAL_KEY) {
    key(modifier, false);
    key(key_code, false);
    key(key_code, true);
    key(modifier, true);
    thread::sleep(KEY_SETTLE);
}

/// Restores the clipboard text that existed before the fallback writer ran. The
/// clipboard is a single global slot, so a non-text clip cannot be restored; in
/// that case the slot is cleared instead of keeping the transcript.
pub struct ClipboardBackup(Option<String>);

impl ClipboardBackup {
    pub fn save() -> Self {
        Self(read_text().ok().flatten())
    }
}

impl Drop for ClipboardBackup {
    fn drop(&mut self) {
        match &self.0 {
            Some(text) => {
                let _ = write_text(text);
            }
            None => clear(),
        }
    }
}

/// Selects the editor's whole content, copies it and collapses the caret to the
/// end, returning the real draft as the application sees it.
pub fn read_draft() -> AppResult<String> {
    // Copying an empty selection leaves the previous clipboard content in place,
    // so the slot is cleared first and an empty editor reads back as empty.
    clear();
    chord(VK_CONTROL, VK_A);
    chord(VK_CONTROL, VK_C);
    let draft = read_text()?.unwrap_or_default();
    collapse_to_end();
    Ok(draft)
}

/// Pastes `text` at the current caret position.
pub fn paste(text: &str) -> AppResult<()> {
    write_text(text)?;
    thread::sleep(KEY_SETTLE);
    chord(VK_CONTROL, VK_V);
    thread::sleep(Duration::from_millis(220));
    Ok(())
}

/// Selects the editor's whole content and copies it, returning what is now there.
pub fn read_back() -> AppResult<String> {
    clear();
    chord(VK_CONTROL, VK_A);
    chord(VK_CONTROL, VK_C);
    Ok(read_text()?.unwrap_or_default())
}

/// Collapses a full selection to its end so the user keeps a caret, not a selection.
pub fn collapse_to_end() {
    key(VK_RIGHT, false);
    key(VK_RIGHT, true);
    thread::sleep(KEY_SETTLE);
}

//! Getting text into whatever window has focus.
//!
//! Two strategies, both measured as part of the north-star latency because the
//! user does not care where the milliseconds went:
//!
//! * Paste. Put the text on the clipboard, send Ctrl+V, put the old clipboard
//!   back. One keystroke regardless of length, so it is flat in the number of
//!   characters. This is the default.
//! * Type. Unicode `SendInput` per character. Works where paste is blocked or
//!   where the app treats a paste differently (some terminals, some editors
//!   with bracketed-paste handling), but cost grows with length.
//!
//! UI Automation `TextPattern` is deliberately not here: it is a cross-process
//! COM round trip per call and it is not reliably supported by the apps that
//! matter. Revisit only with measurements.

use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RCONTROL, VK_SHIFT, VK_V,
};

const CF_UNICODETEXT: u32 = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Paste,
    Type,
}

impl Mode {
    pub fn from_name(s: &str) -> Mode {
        match s.to_ascii_lowercase().as_str() {
            "type" | "keystrokes" => Mode::Type,
            _ => Mode::Paste,
        }
    }
}

pub fn insert(text: &str, mode: Mode) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    match mode {
        Mode::Paste => paste(text),
        Mode::Type => type_text(text),
    }
}

/// Clipboard paste with save and restore.
///
/// Only CF_UNICODETEXT is preserved. Restoring every format would mean
/// round-tripping arbitrary binary blobs, some of which are delay-rendered by
/// their owning process and cannot be copied at all; text is what people
/// actually lose. When the clipboard held something else, it is left cleared
/// rather than silently corrupted.
fn paste(text: &str) -> Result<(), String> {
    let saved = read_clipboard_text();

    write_clipboard_text(text)?;

    // The modifiers the user is physically holding would combine with our
    // Ctrl+V into something else entirely. Lift them first, restore after.
    let held = held_modifiers();
    for vk in &held {
        send_key(*vk, true);
    }

    send_key(VK_CONTROL, false);
    send_key(VK_V, false);
    send_key(VK_V, true);
    send_key(VK_CONTROL, true);

    for vk in &held {
        send_key(*vk, false);
    }

    // Give the target application time to service the paste before the
    // clipboard changes under it. This is off the user's critical path: the
    // characters are already on screen.
    if let Some(old) = saved {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let _ = write_clipboard_text(&old);
        });
    }
    Ok(())
}

/// Which of our own hotkey-adjacent modifiers are currently down.
fn held_modifiers() -> Vec<VIRTUAL_KEY> {
    let mut held = Vec::new();
    for vk in [VK_RCONTROL, VK_LCONTROL, VK_SHIFT, VK_MENU] {
        // GetAsyncKeyState: high bit set means the key is down now.
        if unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0 {
            held.push(vk);
        }
    }
    held
}

fn send_key(vk: VIRTUAL_KEY, up: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

/// Unicode keystrokes, surrogate pairs included, in one SendInput batch so the
/// target sees them as a single burst.
fn type_text(text: &str) -> Result<(), String> {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        for up in [false, true] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: if up {
                            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                        } else {
                            KEYEVENTF_UNICODE
                        },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    // SendInput takes a batch; anything above a few thousand events risks the
    // target dropping them, so chunk it.
    for batch in inputs.chunks(512) {
        let sent = unsafe { SendInput(batch, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != batch.len() {
            return Err("SendInput was blocked, most likely by UIPI".into());
        }
    }
    Ok(())
}

fn read_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(Some(HWND::default())).is_err() {
            return None;
        }
        let result = (|| {
            let handle: HANDLE = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let bytes = GlobalSize(hglobal);
            let max = bytes / 2;
            let mut len = 0usize;
            while len < max && *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(hglobal);
            Some(s)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_clipboard_text(text: &str) -> Result<(), String> {
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // The clipboard is a shared, contended resource: another process can
        // hold it open. Retry briefly rather than dropping the dictation.
        let mut opened = false;
        for _ in 0..20 {
            if OpenClipboard(Some(HWND::default())).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !opened {
            return Err("could not open the clipboard".into());
        }

        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard: {e}"))?;
            let bytes = utf16.len() * 2;
            let hglobal =
                GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| format!("GlobalAlloc: {e}"))?;
            let ptr = GlobalLock(hglobal) as *mut u16;
            if ptr.is_null() {
                return Err("GlobalLock returned null".into());
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
            let _ = GlobalUnlock(hglobal);
            // Ownership passes to the clipboard on success, so no free here.
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0)))
                .map_err(|e| format!("SetClipboardData: {e}"))?;
            Ok(())
        })();

        let _ = CloseClipboard();
        result
    }
}

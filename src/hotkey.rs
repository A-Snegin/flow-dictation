//! Global hold-to-dictate hotkey via a low-level keyboard hook.
//!
//! `RegisterHotKey` is not usable here: it reports presses, not releases, and
//! release is what ends the utterance. `WH_KEYBOARD_LL` gives both.
//!
//! The hook runs on the thread that installed it and Windows drops hooks that
//! take too long (`LowLevelHooksTimeout`, 300 ms by default), so the callback
//! does the absolute minimum: compare a virtual key, set a flag, post to a
//! channel, return. Everything else happens on other threads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
}

/// Virtual key codes worth binding. Right Ctrl is the default: one key, easy
/// to hold, and Ctrl combinations still pass through because we never swallow
/// the event.
pub mod vk {
    pub const RCONTROL: u32 = 0xA3;
    pub const LCONTROL: u32 = 0xA2;
    pub const RMENU: u32 = 0xA5; // Right Alt
    pub const RSHIFT: u32 = 0xA1;
    pub const F13: u32 = 0x7C;
    pub const CAPITAL: u32 = 0x14;

    pub fn from_name(name: &str) -> Option<u32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "rightctrl" | "rctrl" | "right_control" => RCONTROL,
            "leftctrl" | "lctrl" => LCONTROL,
            "rightalt" | "ralt" => RMENU,
            "rightshift" | "rshift" => RSHIFT,
            "f13" => F13,
            "capslock" => CAPITAL,
            _ => return None,
        })
    }
}

struct HookState {
    key: u32,
    tx: Sender<HotkeyEvent>,
    held: AtomicBool,
    /// Thread and message used to wake the message loop, so a key event is
    /// serviced immediately rather than at the next poll.
    thread_id: u32,
    wake_message: u32,
}

static STATE: OnceLock<HookState> = OnceLock::new();

/// Installs the hook on the calling thread. That thread must run a message
/// loop, or the hook never fires.
pub fn install(key: u32, tx: Sender<HotkeyEvent>, wake_message: u32) -> Result<HHOOK, String> {
    let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    STATE
        .set(HookState {
            key,
            tx,
            held: AtomicBool::new(false),
            thread_id,
            wake_message,
        })
        .map_err(|_| "hotkey hook already installed".to_string())?;

    unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
            .map_err(|e| format!("SetWindowsHookExW: {e}"))
    }
}

pub fn uninstall(hook: HHOOK) {
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
}

fn wake(state: &HookState) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
            state.thread_id,
            state.wake_message,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        if let Some(state) = STATE.get() {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if info.vkCode == state.key {
                match wparam.0 as u32 {
                    WM_KEYDOWN | WM_SYSKEYDOWN => {
                        // Auto-repeat fires this repeatedly; only the edge counts.
                        if !state.held.swap(true, Ordering::SeqCst) {
                            let _ = state.tx.send(HotkeyEvent::Down);
                            wake(state);
                        }
                    }
                    WM_KEYUP | WM_SYSKEYUP => {
                        if state.held.swap(false, Ordering::SeqCst) {
                            let _ = state.tx.send(HotkeyEvent::Up);
                            wake(state);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    // Never swallow the key. Right Ctrl must still work as Right Ctrl.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

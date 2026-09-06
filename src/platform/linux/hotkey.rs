//! Hold-key events on Linux. STUB: platform-core replaces this file.
//! Contract: `install(key, tx, waker)` starts a listener that pushes
//! `HotkeyEvent`s into `tx` and calls `waker.wake()` after each. Primary
//! backend is the control socket at $XDG_RUNTIME_DIR/flow/control.sock,
//! driven by `flow-core --key down|up|...` from Hyprland keybinds.

use std::sync::mpsc::Sender;

pub use crate::control::HotkeyEvent;
use crate::platform::sys::Waker;

pub mod vk {
    pub const RCONTROL: u32 = 97;
    pub const LCONTROL: u32 = 29;
    pub const RMENU: u32 = 100;
    pub const RSHIFT: u32 = 54;
    pub const F13: u32 = 183;
    pub const CAPITAL: u32 = 58;

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

pub struct Handle;

pub fn install(_key: u32, _tx: Sender<HotkeyEvent>, _waker: Waker) -> Result<Handle, String> {
    Err("hotkey listener not implemented on this platform yet".into())
}

pub fn rebind(_key: u32) {}

pub fn uninstall(_handle: Handle) {}

/// Client side: deliver one command to a running flow-core.
pub fn send_command(_cmd: &str) -> Result<(), String> {
    Err("control socket client not implemented yet".into())
}

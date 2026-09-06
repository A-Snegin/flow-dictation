//! Text insertion on Wayland. STUB: inject-target replaces this file.
//! Contract identical to platform/windows/inject.rs.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Paste,
    Type,
}

impl Mode {
    pub fn from_name(s: &str) -> Mode {
        match s.to_ascii_lowercase().as_str() {
            "paste" | "clipboard" => Mode::Paste,
            _ => Mode::Type,
        }
    }
}

pub fn insert(_text: &str, _mode: Mode) -> Result<(), String> {
    Err("insertion not implemented on this platform yet".into())
}

pub fn clipboard_text() -> Option<String> {
    None
}

pub fn set_clipboard_text(_text: &str) -> Result<(), String> {
    Err("clipboard not implemented on this platform yet".into())
}

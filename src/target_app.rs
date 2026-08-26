//! Which application is about to receive the text, and what that implies.
//!
//! Two things vary by application and both matter.
//!
//! Paste keybinding. Windows Terminal binds Ctrl+V to paste and modern conhost
//! does too, but the setting can be off, and some terminals and remote-desktop
//! clients swallow it. Synthesised Unicode keystrokes work everywhere a console
//! reads input, so terminals default to typing rather than pasting.
//!
//! Newlines. In an editor a line break is a line break. At a shell prompt it is
//! the Enter key, and dictating "new paragraph" would run whatever is on the
//! command line. Inside a terminal, line breaks are replaced rather than sent.

use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Executable name of the window that currently has focus, lowercased.
/// Empty when it cannot be determined, which callers treat as "not a terminal".
pub fn foreground_executable() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return String::new();
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
            .rsplit('\\')
            .next()
            .unwrap_or_default()
            .to_lowercase()
    }
}

/// Terminals and shells, where Enter runs things and paste is unreliable.
pub const DEFAULT_TERMINALS: &[&str] = &[
    "windowsterminal.exe",
    "wt.exe",
    "powershell.exe",
    "pwsh.exe",
    "cmd.exe",
    "conhost.exe",
    "openconsole.exe",
    "mintty.exe",
    "bash.exe",
    "git-bash.exe",
    "alacritty.exe",
    "wezterm-gui.exe",
    "hyper.exe",
    "putty.exe",
    "mstsc.exe",
];

pub fn is_terminal(exe: &str, extra: &[String]) -> bool {
    if exe.is_empty() {
        return false;
    }
    DEFAULT_TERMINALS.contains(&exe) || extra.iter().any(|e| e.to_lowercase() == exe)
}

/// Line breaks become this at a shell prompt. Never a real newline: that is
/// the Enter key and it would run the command.
pub fn strip_newlines(text: &str, replacement: &str) -> String {
    if !text.contains('\n') && !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pending = false;
    for c in text.chars() {
        match c {
            '\n' | '\r' => pending = true,
            _ => {
                if pending {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push_str(replacement);
                    }
                    pending = false;
                }
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_shells() {
        assert!(is_terminal("powershell.exe", &[]));
        assert!(is_terminal("windowsterminal.exe", &[]));
        assert!(!is_terminal("notepad.exe", &[]));
        assert!(!is_terminal("", &[]));
    }

    #[test]
    fn honours_extra_entries() {
        let extra = vec!["myshell.exe".to_string()];
        assert!(is_terminal("myshell.exe", &extra));
    }

    #[test]
    fn newlines_never_reach_a_prompt() {
        assert_eq!(strip_newlines("git status\nls -la", " "), "git status ls -la");
        assert_eq!(strip_newlines("one\n\ntwo", " "), "one two");
        assert_eq!(strip_newlines("trailing\n", " "), "trailing");
        assert_eq!(strip_newlines("no breaks here", " "), "no breaks here");
    }

    #[test]
    fn does_not_double_space() {
        assert_eq!(strip_newlines("done \nnext", " "), "done next");
    }
}

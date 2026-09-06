//! Getting text into whatever window has focus, on Wayland.
//!
//! Two strategies, same contract as the Windows module:
//!
//! * Type. `wtype` reads the text from stdin and replays it as a
//!   `zwp_virtual_keyboard_v1` sequence. Nothing touches argv, so the words
//!   never show up in `ps` or a shell history. Cost is a fork plus a fresh
//!   Wayland connection each call, and it scales with the number of
//!   characters, but there is no clipboard to save, corrupt or race. This is
//!   the **default** on Linux: unlike Windows, where SendInput can be blocked
//!   by UIPI and paste is the flat-cost fallback, `wtype` has no such
//!   restriction here, and avoiding the clipboard means dictation never shows
//!   up in a clipboard manager (cliphist and friends) unless the user opts
//!   into paste mode.
//! * Paste. Save the clipboard with `wl-paste`, put the text on it with
//!   `wl-copy`, send Ctrl+V with `wtype`, then restore the saved contents.
//!   One keystroke regardless of length, so it is flat in the number of
//!   characters, at the cost of a brief window where the system clipboard
//!   holds the dictated text and a clipboard history manager can capture it.
//!   Opt in with `insertion.mode = "paste"`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

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
/// Only the text/plain selection is preserved; a non-text selection (an
/// image, a file list) reads back empty from `wl-paste` and is treated the
/// same as an empty clipboard, so it is cleared rather than silently
/// corrupted. Same trade-off the Windows CF_UNICODETEXT-only save makes.
fn paste(text: &str) -> Result<(), String> {
    let saved = read_clipboard_text();

    write_clipboard_text(text)?;

    // Hyprland's release bind means the physical key is already up by the
    // time this runs, so there is no GetAsyncKeyState-style modifier lift to
    // do here: `-M ctrl v -m ctrl` presses and releases a virtual ctrl that
    // is independent of whatever the user's hands are doing.
    let output = Command::new("wtype")
        .args(["-M", "ctrl", "v", "-m", "ctrl"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run wtype for ctrl+v: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "wtype (ctrl+v) exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // Give the target application time to service the paste before the
    // clipboard changes under it. Off the user's critical path: the
    // characters are already on screen.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        match &saved {
            Some(old) => {
                let _ = write_clipboard_text(old);
            }
            None => {
                let _ = clear_clipboard();
            }
        }
    });
    Ok(())
}

/// Unicode keystrokes via wtype, text piped over stdin so it never touches
/// argv (no `ps` leak, no shell history entry).
fn type_text(text: &str) -> Result<(), String> {
    // wtype costs about 4.5 ms per character (a compositor round trip each),
    // so 60 characters take roughly 270 ms. That is why paste is the default
    // outside terminals; an in-process virtual keyboard is the eventual fix.
    let mut child = Command::new("wtype")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn wtype: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("failed to write to wtype: {e}"))?;
        // Dropped here, closing the pipe so wtype sees EOF and starts typing.
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for wtype: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "wtype exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Reading and writing the clipboard directly, exposed for the injection test.
pub fn clipboard_text() -> Option<String> {
    read_clipboard_text()
}

pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    write_clipboard_text(text)
}

/// `wl-paste` exits 1 when there is no text selection at all (nothing has
/// ever been copied, or the current selection is not text); an empty
/// selection reads back as empty stdout. Both mean "nothing to restore" and
/// are folded into `None`, the same shape as Windows' "no CF_UNICODETEXT".
fn read_clipboard_text() -> Option<String> {
    let output = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write_clipboard_text(text: &str) -> Result<(), String> {
    // wl-copy forks a background process to serve the selection and returns;
    // that daemon inherits any piped stdout/stderr fd and keeps it open, so
    // reading those pipes for output (`wait_with_output`) would block forever
    // waiting for an EOF that only comes when the daemon itself exits, which
    // is when the clipboard changes again. Only wait for the launcher process
    // to exit, and only look at its exit status.
    let mut child = Command::new("wl-copy")
        .args(["--type", "text/plain"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn wl-copy: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("failed to write to wl-copy: {e}"))?;
    }

    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for wl-copy: {e}"))?;
    if !status.success() {
        return Err(format!("wl-copy exited with {status}"));
    }
    Ok(())
}

fn clear_clipboard() -> Result<(), String> {
    let status = Command::new("wl-copy")
        .arg("--clear")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("failed to run wl-copy --clear: {e}"))?;
    if !status.success() {
        return Err(format!("wl-copy --clear exited with {status}"));
    }
    Ok(())
}

//! Focused window lookup on Hyprland.
//!
//! Hyprland already keeps this in memory and answers over a Unix socket, so
//! there is no need to shell out to `hyprctl` (a fork plus its own connect)
//! or add a JSON crate: the response is a handful of fixed top-level keys and
//! a small hand scan for one of them is all that is needed.

use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub const DEFAULT_TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "foot",
    "footclient",
    "ghostty",
    "com.mitchellh.ghostty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "org.gnome.terminal",
    "konsole",
    "xterm",
    "urxvt",
    "st",
];

/// Lowercased `class` (falling back to `initialClass`) of the focused window,
/// or "" when Hyprland cannot be reached, there is no focused window, or the
/// process is not running under Hyprland at all.
pub fn foreground_executable() -> String {
    query_active_window_class().unwrap_or_default().to_lowercase()
}

fn query_active_window_class() -> Option<String> {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").ok()?;
    let sig = env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let path = format!("{runtime_dir}/hypr/{sig}/.socket.sock");

    let mut stream = UnixStream::connect(&path).ok()?;
    let timeout = Some(Duration::from_millis(200));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    stream.write_all(b"j/activewindow").ok()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);

    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;

    json_string_field(&body, "class").or_else(|| json_string_field(&body, "initialClass"))
}

/// Pulls the value of `"key": "value"` out of a flat JSON object without
/// pulling in a JSON crate. Handles `\"` and `\\` escapes because window
/// titles (not used here, but classes occasionally) can contain them; good
/// enough for the one-level object Hyprland sends back.
fn json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_pos = json.find(&needle)?;
    let after_key = &json[key_pos + needle.len()..];
    let colon = after_key.find(':')?;
    let mut rest = after_key[colon + 1..].trim_start();
    rest = rest.strip_prefix('"')?;

    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => match chars.next() {
                Some('"') => value.push('"'),
                Some('\\') => value.push('\\'),
                Some(other) => value.push(other),
                None => break,
            },
            _ => value.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_class_out_of_the_hyprland_response() {
        let body = r#"{
    "address": "0x1",
    "class": "com.mitchellh.ghostty",
    "title": "a window",
    "initialClass": "com.mitchellh.ghostty"
}"#;
        assert_eq!(
            json_string_field(body, "class").as_deref(),
            Some("com.mitchellh.ghostty")
        );
    }

    #[test]
    fn falls_back_when_class_is_absent() {
        let body = r#"{"initialClass": "firefox"}"#;
        assert_eq!(json_string_field(body, "class"), None);
        assert_eq!(json_string_field(body, "initialClass").as_deref(), Some("firefox"));
    }

    #[test]
    fn no_focused_window_is_an_empty_object() {
        assert_eq!(json_string_field("{}", "class"), None);
    }

    #[test]
    fn unescapes_quotes_in_the_value() {
        let body = r#"{"class": "weird \"quoted\" class"}"#;
        assert_eq!(
            json_string_field(body, "class").as_deref(),
            Some(r#"weird "quoted" class"#)
        );
    }
}

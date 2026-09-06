//! Focused window lookup on Hyprland. STUB: inject-target replaces this file.
//! Contract: `foreground_executable()` returns the lowercased window class of
//! the focused window (Hyprland IPC `j/activewindow`), or "" when unknown.

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

pub fn foreground_executable() -> String {
    String::new()
}

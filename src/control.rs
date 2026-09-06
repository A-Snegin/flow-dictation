//! Events that reach the app from the outside: the hold key and, on Linux,
//! the control socket. Windows only ever produces `Down` and `Up`; the rest
//! replace the tray menu on platforms without one.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
    /// Discard the current utterance without inserting anything.
    Cancel,
    /// Pause or resume dictation (the tray's Toggle).
    Toggle,
    /// Re-read settings.toml now.
    Reload,
    /// Print the latency report to stdout.
    Report,
    Quit,
}

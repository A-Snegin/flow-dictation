//! Flow: speed-first local dictation. Windows and Linux (Wayland).

pub mod asr;
pub mod asr_service;
pub mod audio_convert;
pub mod control;
pub mod ffi;
pub mod format;
pub mod platform;
pub mod settings;
pub mod stats;
pub mod target_app;
pub mod trace;
pub mod wav;

// The platform modules keep their historical top-level names so `flow::audio`
// and friends resolve the same on every target.
pub use platform::{audio, hotkey, inject, overlay};
#[cfg(windows)]
pub use platform::{autostart, settings_ui, tray};

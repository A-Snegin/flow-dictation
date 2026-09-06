//! Everything that talks to the operating system, one directory per platform.
//! Each platform exposes the same module names with the same public surface:
//! `audio::Capture`, `hotkey::{install, rebind, uninstall, vk}`, `inject`,
//! `overlay::Overlay`, `sys::{Waker, raise_priority, now, freq}`, and
//! `target_app::foreground_executable`.

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

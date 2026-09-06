//! The pill on Wayland. STUB: the overlay agent replaces this file.
//! Contract identical to platform/windows/overlay.rs.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

pub use super::overlay_types::*;

pub struct Overlay {
    _p: (),
}

impl Overlay {
    pub fn disabled() -> Overlay {
        Overlay { _p: () }
    }
    pub fn create(_hint_key: &str) -> Result<Overlay, String> {
        Err("overlay not implemented on this platform yet".into())
    }
    pub fn is_visible(&self) -> bool {
        false
    }
    pub fn is_drawable(&self) -> bool {
        false
    }
    pub fn attach_level(&mut self, _source: Arc<AtomicU32>) {}
    pub fn set_level(&mut self, _peak: f32) {}
    pub fn set(&mut self, _state: OverlayState, _text: &str) {}
    pub fn tick(&mut self) -> Option<Duration> {
        None
    }
}

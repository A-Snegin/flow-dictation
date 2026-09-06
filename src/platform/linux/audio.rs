//! PipeWire microphone capture. STUB: platform-core replaces this file.
//! Contract (identical to platform/windows/audio.rs): the stream is created
//! and connected at startup but inactive; `arm()` activates it on key-down,
//! `disarm()` deactivates it on key-up; `sink` receives 16 kHz mono f32.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

pub const TARGET_SR: u32 = 16_000;

#[derive(Default)]
pub struct CaptureStats {
    pub packets: AtomicU32,
    pub glitches: AtomicU32,
    pub device_errors: AtomicU32,
    pub last_start_us: AtomicU32,
    pub last_reset_us: AtomicU32,
}

pub struct Capture {
    pub stats: Arc<CaptureStats>,
}

impl Capture {
    pub fn open<F>(_sink: F) -> Result<Capture, String>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        Err("audio capture not implemented on this platform yet".into())
    }

    pub fn arm(&self) {}
    pub fn disarm(&self) {}
}

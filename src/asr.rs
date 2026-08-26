//! Safe wrapper over the Moonshine streaming API.
//!
//! Ownership rules taken from the C header: transcript memory belongs to the
//! transcriber and is only valid until the next call on it, so every result is
//! copied out before we hand it back. Calls are thread-safe but serialise on
//! one transcriber, which is why the app runs exactly one ASR worker.

use crate::ffi;
use std::ffi::{CStr, CString};

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub text: String,
    pub id: u64,
    pub start_time: f32,
    pub duration: f32,
    pub is_complete: bool,
    pub is_updated: bool,
    pub is_new: bool,
    pub last_latency_ms: u32,
}

pub struct Transcriber {
    handle: i32,
}

// Every entry point is documented thread-safe; work on one transcriber is
// serialised internally by the library.
unsafe impl Send for Transcriber {}
unsafe impl Sync for Transcriber {}

impl Transcriber {
    pub fn load(model_dir: &str, arch: u32, options: &[(&str, &str)]) -> Result<Self, String> {
        let path = CString::new(model_dir).map_err(|e| e.to_string())?;
        // Keep the CStrings alive for the duration of the call.
        let owned: Vec<(CString, CString)> = options
            .iter()
            .map(|(k, v)| {
                (
                    CString::new(*k).expect("option name"),
                    CString::new(*v).expect("option value"),
                )
            })
            .collect();
        let raw: Vec<ffi::moonshine_option_t> = owned
            .iter()
            .map(|(k, v)| ffi::moonshine_option_t {
                name: k.as_ptr(),
                value: v.as_ptr(),
            })
            .collect();

        let handle = unsafe {
            ffi::moonshine_load_transcriber_from_files(
                path.as_ptr(),
                arch,
                if raw.is_empty() { std::ptr::null() } else { raw.as_ptr() },
                raw.len() as u64,
                ffi::MOONSHINE_HEADER_VERSION,
            )
        };
        if handle < 0 {
            return Err(format!(
                "loading {model_dir} (arch {arch}): {}",
                ffi::error_string(handle)
            ));
        }
        Ok(Transcriber { handle })
    }

    /// Bias the decoder towards a comma-separated term list. Empty turns it off.
    /// Upstream default boost of 2.0 is deliberate; raising it invents terms.
    pub fn set_keyterms(&self, terms: &str) -> Result<(), String> {
        let c = CString::new(terms).map_err(|e| e.to_string())?;
        let err = unsafe { ffi::moonshine_transcriber_set_keyterms(self.handle, c.as_ptr()) };
        check(err)
    }

    pub fn create_stream(&self) -> Result<Stream<'_>, String> {
        let handle = unsafe { ffi::moonshine_create_stream(self.handle, 0) };
        if handle < 0 {
            return Err(format!("create_stream: {}", ffi::error_string(handle)));
        }
        Ok(Stream { owner: self, handle })
    }

    /// Non-streaming path, used for very short holds and for fixtures.
    pub fn transcribe_once(&self, audio: &mut [f32], sample_rate: i32) -> Result<Vec<Line>, String> {
        let mut out: *mut ffi::transcript_t = std::ptr::null_mut();
        let err = unsafe {
            ffi::moonshine_transcribe_without_streaming(
                self.handle,
                audio.as_mut_ptr(),
                audio.len() as u64,
                sample_rate,
                0,
                &mut out,
            )
        };
        check(err)?;
        Ok(copy_lines(out))
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        unsafe { ffi::moonshine_free_transcriber(self.handle) };
    }
}

pub struct Stream<'a> {
    owner: &'a Transcriber,
    handle: i32,
}

impl Stream<'_> {
    pub fn start(&self) -> Result<(), String> {
        check(unsafe { ffi::moonshine_start_stream(self.owner.handle, self.handle) })
    }

    /// Ends input. Audio already added but not analysed is kept, so the
    /// next `transcribe` drains it and marks every line complete.
    pub fn stop(&self) -> Result<(), String> {
        check(unsafe { ffi::moonshine_stop_stream(self.owner.handle, self.handle) })
    }

    /// Buffers audio only. Documented safe from time-critical threads.
    pub fn add_audio(&self, audio: &[f32], sample_rate: i32) -> Result<(), String> {
        check(unsafe {
            ffi::moonshine_transcribe_add_audio_to_stream(
                self.owner.handle,
                self.handle,
                audio.as_ptr(),
                audio.len() as u64,
                sample_rate,
                0,
            )
        })
    }

    /// Runs the models. `force` bypasses the library's internal 200 ms
    /// minimum-new-audio throttle, at a CPU cost.
    pub fn transcribe(&self, force: bool) -> Result<Vec<Line>, String> {
        let mut out: *mut ffi::transcript_t = std::ptr::null_mut();
        let flags = if force { ffi::FLAG_FORCE_UPDATE } else { 0 };
        let err = unsafe {
            ffi::moonshine_transcribe_stream(self.owner.handle, self.handle, flags, &mut out)
        };
        check(err)?;
        Ok(copy_lines(out))
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        unsafe { ffi::moonshine_free_stream(self.owner.handle, self.handle) };
    }
}

fn check(err: i32) -> Result<(), String> {
    if err == ffi::ERROR_NONE {
        Ok(())
    } else {
        Err(ffi::error_string(err))
    }
}

fn copy_lines(t: *mut ffi::transcript_t) -> Vec<Line> {
    if t.is_null() {
        return Vec::new();
    }
    let count = unsafe { (*t).line_count } as usize;
    let base = unsafe { (*t).lines };
    let mut lines = Vec::with_capacity(count);
    for i in 0..count {
        let l = unsafe { &*base.add(i) };
        let text = if l.text.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(l.text) }.to_string_lossy().into_owned()
        };
        lines.push(Line {
            text,
            id: l.id,
            start_time: l.start_time,
            duration: l.duration,
            is_complete: l.is_complete != 0,
            is_updated: l.is_updated != 0,
            is_new: l.is_new != 0,
            last_latency_ms: l.last_transcription_latency_ms,
        });
    }
    lines
}

/// Join a transcript's lines the way the app inserts them.
pub fn join(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

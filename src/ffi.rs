//! Raw bindings to the Moonshine C API (`vendor/moonshine/include/moonshine-c-api.h`).
//!
//! Hand-written rather than bindgen: the surface we need is a dozen calls and
//! two structs, and pinning them here keeps the ABI contract visible.

#![allow(non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_float, c_int, c_uchar, c_void};

/// Must match the header the linked binary was built from.
pub const MOONSHINE_HEADER_VERSION: i32 = 30000;

pub const ARCH_TINY: u32 = 0;
pub const ARCH_BASE: u32 = 1;
pub const ARCH_TINY_STREAMING: u32 = 2;
pub const ARCH_BASE_STREAMING: u32 = 3;
pub const ARCH_SMALL_STREAMING: u32 = 4;
pub const ARCH_MEDIUM_STREAMING: u32 = 5;

pub const ERROR_NONE: i32 = 0;
pub const FLAG_FORCE_UPDATE: u32 = 1 << 0;
pub const FLAG_SPELLING_MODE: u32 = 1 << 1;

#[repr(C)]
pub struct moonshine_option_t {
    pub name: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct transcript_word_t {
    pub text: *const c_char,
    pub start: c_float,
    pub end: c_float,
    pub confidence: c_float,
}

#[repr(C)]
pub struct speaker_span_t {
    pub start_time: c_float,
    pub duration: c_float,
    pub speaker_id: u64,
    pub speaker_index: u32,
    pub start_char: u64,
    pub end_char: u64,
}

#[repr(C)]
pub struct transcript_line_t {
    pub text: *const c_char,
    pub audio_data: *const c_float,
    pub audio_data_count: usize,
    pub start_time: c_float,
    pub duration: c_float,
    pub id: u64,
    pub is_complete: i8,
    pub is_updated: i8,
    pub is_new: i8,
    pub has_text_changed: i8,
    pub have_speakers_changed: i8,
    pub speaker_spans: *const speaker_span_t,
    pub speaker_span_count: u64,
    pub last_transcription_latency_ms: u32,
    pub words: *const transcript_word_t,
    pub word_count: u64,
}

#[repr(C)]
pub struct transcript_t {
    pub lines: *mut transcript_line_t,
    pub line_count: u64,
}

extern "C" {
    pub fn moonshine_get_version() -> i32;
    pub fn moonshine_error_to_string(error: i32) -> *const c_char;
    pub fn moonshine_free_buffer(ptr: *mut c_void);

    pub fn moonshine_load_transcriber_from_files(
        path: *const c_char,
        model_arch: u32,
        options: *const moonshine_option_t,
        options_count: u64,
        moonshine_version: i32,
    ) -> i32;
    pub fn moonshine_free_transcriber(transcriber_handle: i32);

    pub fn moonshine_transcriber_set_keyterms(
        transcriber_handle: i32,
        keyterms: *const c_char,
    ) -> i32;
    pub fn moonshine_transcriber_set_context(
        transcriber_handle: i32,
        context: *const c_char,
    ) -> i32;

    pub fn moonshine_transcribe_without_streaming(
        transcriber_handle: i32,
        audio_data: *mut c_float,
        audio_length: u64,
        sample_rate: i32,
        flags: u32,
        out_transcript: *mut *mut transcript_t,
    ) -> i32;

    pub fn moonshine_create_stream(transcriber_handle: i32, flags: u32) -> i32;
    pub fn moonshine_free_stream(transcriber_handle: i32, stream_handle: i32) -> i32;
    pub fn moonshine_start_stream(transcriber_handle: i32, stream_handle: i32) -> i32;
    pub fn moonshine_stop_stream(transcriber_handle: i32, stream_handle: i32) -> i32;
    pub fn moonshine_transcribe_add_audio_to_stream(
        transcriber_handle: i32,
        stream_handle: i32,
        new_audio_data: *const c_float,
        audio_length: u64,
        sample_rate: i32,
        flags: u32,
    ) -> i32;
    pub fn moonshine_transcribe_stream(
        transcriber_handle: i32,
        stream_handle: i32,
        flags: u32,
        out_transcript: *mut *mut transcript_t,
    ) -> i32;
    pub fn moonshine_transcript_to_string(transcript: *const transcript_t) -> *const c_char;

    pub fn moonshine_get_stt_dependencies(
        language: *const c_char,
        options: *const moonshine_option_t,
        options_count: u64,
        out_dependencies_json: *mut *mut c_char,
    ) -> i32;
}

/// Turn a Moonshine error code into a readable string.
pub fn error_string(code: i32) -> String {
    if code == ERROR_NONE {
        return "ok".into();
    }
    unsafe {
        let p = moonshine_error_to_string(code);
        if p.is_null() {
            format!("unknown moonshine error {code}")
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

// Silence unused-import warnings on the aliases we keep for documentation value.
const _: Option<c_int> = None;
const _: Option<c_uchar> = None;

//! Phase 0 gate: prove the prebuilt Moonshine libraries link and run from Rust.

use flow::ffi;
use std::ffi::CString;

fn main() {
    let version = unsafe { ffi::moonshine_get_version() };
    println!("moonshine library version: {version}");
    println!("header version linked against: {}", ffi::MOONSHINE_HEADER_VERSION);
    assert_eq!(
        version,
        ffi::MOONSHINE_HEADER_VERSION,
        "linked library disagrees with the vendored header"
    );

    let model_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/small-streaming-en".to_string());
    let path = CString::new(model_dir.clone()).unwrap();

    let t0 = std::time::Instant::now();
    let handle = unsafe {
        ffi::moonshine_load_transcriber_from_files(
            path.as_ptr(),
            ffi::ARCH_SMALL_STREAMING,
            std::ptr::null(),
            0,
            ffi::MOONSHINE_HEADER_VERSION,
        )
    };
    if handle < 0 {
        eprintln!(
            "load failed for {model_dir}: {}",
            ffi::error_string(handle)
        );
        std::process::exit(1);
    }
    println!("transcriber loaded in {:?} (handle {handle})", t0.elapsed());

    // One-second silence warm-up: proves a full encode/decode round trip runs.
    let mut silence = vec![0.0f32; 16_000];
    let mut transcript: *mut ffi::transcript_t = std::ptr::null_mut();
    let t1 = std::time::Instant::now();
    let err = unsafe {
        ffi::moonshine_transcribe_without_streaming(
            handle,
            silence.as_mut_ptr(),
            silence.len() as u64,
            16_000,
            0,
            &mut transcript,
        )
    };
    if err != ffi::ERROR_NONE {
        eprintln!("transcribe failed: {}", ffi::error_string(err));
        std::process::exit(1);
    }
    let lines = unsafe { (*transcript).line_count };
    println!("warm-up transcribe ok in {:?}, {lines} line(s)", t1.elapsed());

    unsafe { ffi::moonshine_free_transcriber(handle) };
    println!("PHASE 0 GATE: PASS");
}

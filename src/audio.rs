//! WASAPI shared-mode, event-driven microphone capture.
//!
//! The device is opened and initialised once at startup and then held in a
//! stopped state. Key-down only calls `IAudioClient::Start`. Opening the device
//! from scratch measured 322 ms on the target machine, which would have clipped
//! the first word of every dictation; starting an already-initialised client is
//! a fraction of that.
//!
//! An initialised but stopped client is not capturing, so Windows shows no
//! microphone indicator and no samples exist to read. That keeps the privacy
//! property of opening on key-down while paying almost none of its cost.
//!
//! The capture thread does four things and nothing else: wait, copy, convert,
//! hand over. No allocation after warm-up, no logging, no locks held over work.
//!
//! Shared mode rather than exclusive: a dictation utility has to coexist with
//! Teams, Zoom and the browser rather than seize the device.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForMultipleObjects, INFINITE,
};

/// Everything downstream works in this format. The model wants 16 kHz mono.
pub const TARGET_SR: u32 = 16_000;

/// Counters the app surfaces when something goes wrong. A rising glitch count
/// while speaking says the pipeline is falling behind real time, which is a
/// better warning than any CPU percentage.
#[derive(Default)]
pub struct CaptureStats {
    pub packets: AtomicU32,
    pub glitches: AtomicU32,
    pub device_errors: AtomicU32,
}

struct Control {
    armed: AtomicBool,
    shutdown: AtomicBool,
    /// Signalled whenever `armed` or `shutdown` changes, so the capture thread
    /// reacts immediately instead of at the next audio event.
    wake: HANDLE,
}

// The only handle shared across threads is an event, which is thread-safe.
unsafe impl Send for Control {}
unsafe impl Sync for Control {}

pub struct Capture {
    control: Arc<Control>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub stats: Arc<CaptureStats>,
}

impl Capture {
    /// Opens and initialises the default capture device without starting it.
    /// `sink` runs on the capture thread with 16 kHz mono f32 samples, so it
    /// must be cheap: copy and return.
    pub fn open<F>(sink: F) -> Result<Capture, String>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let wake = unsafe {
            CreateEventW(None, false, false, PCWSTR::null())
                .map_err(|e| format!("CreateEventW: {e}"))?
        };
        let control = Arc::new(Control {
            armed: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            wake,
        });
        let stats = Arc::new(CaptureStats::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread = {
            let control = Arc::clone(&control);
            let stats = Arc::clone(&stats);
            std::thread::Builder::new()
                .name("flow-capture".into())
                .spawn(move || {
                    if let Err(e) = capture_thread(sink, &control, &stats, ready_tx) {
                        stats.device_errors.fetch_add(1, Ordering::Relaxed);
                        eprintln!("capture thread ended: {e}");
                    }
                })
                .map_err(|e| e.to_string())?
        };

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Capture {
                control,
                thread: Some(thread),
                stats,
            }),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("capture device did not open: {e}")),
        }
    }

    /// Key-down. Starts the stream; samples begin reaching the sink.
    pub fn arm(&self) {
        self.control.armed.store(true, Ordering::SeqCst);
        unsafe {
            let _ = SetEvent(self.control.wake);
        }
    }

    /// Key-up. Stops the stream and drops anything the device still holds, so
    /// the next dictation cannot begin with stale audio.
    pub fn disarm(&self) {
        self.control.armed.store(false, Ordering::SeqCst);
        unsafe {
            let _ = SetEvent(self.control.wake);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.control.shutdown.store(true, Ordering::SeqCst);
        unsafe {
            let _ = SetEvent(self.control.wake);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.control.wake);
        }
    }
}

/// Streaming downmix and resample into 16 kHz mono. Keeps a fractional
/// position across packets so packet boundaries neither click nor drift.
struct Converter {
    src_rate: u32,
    channels: usize,
    pos: f64,
    prev: f32,
    have_prev: bool,
    mono: Vec<f32>,
    out: Vec<f32>,
}

impl Converter {
    fn new(src_rate: u32, channels: usize) -> Converter {
        Converter {
            src_rate,
            channels,
            pos: 0.0,
            prev: 0.0,
            have_prev: false,
            mono: Vec::with_capacity(8192),
            out: Vec::with_capacity(8192),
        }
    }

    /// Forgets continuity state. Called when the stream restarts, so the first
    /// packet of a new utterance does not interpolate against the last packet
    /// of the previous one.
    fn reset(&mut self) {
        self.pos = 0.0;
        self.prev = 0.0;
        self.have_prev = false;
    }

    fn convert(&mut self, interleaved: &[f32]) -> &[f32] {
        self.mono.clear();
        if self.channels <= 1 {
            self.mono.extend_from_slice(interleaved);
        } else {
            let n = self.channels;
            self.mono.extend(
                interleaved
                    .chunks_exact(n)
                    .map(|f| f.iter().sum::<f32>() / n as f32),
            );
        }

        self.out.clear();
        if self.src_rate == TARGET_SR {
            self.out.extend_from_slice(&self.mono);
            return &self.out;
        }

        let step = self.src_rate as f64 / TARGET_SR as f64;
        let len = self.mono.len();
        if len == 0 {
            return &self.out;
        }
        // pos is relative to the start of this packet and may be negative,
        // meaning the sample falls between the previous packet and this one.
        let mut p = self.pos;
        while p < len as f64 {
            let i = p.floor();
            let frac = (p - i) as f32;
            let idx = i as isize;
            let a = if idx < 0 {
                if self.have_prev {
                    self.prev
                } else {
                    self.mono[0]
                }
            } else {
                self.mono[idx as usize]
            };
            let b = if idx + 1 < len as isize {
                self.mono[(idx + 1) as usize]
            } else {
                a
            };
            self.out.push(a + (b - a) * frac);
            p += step;
        }
        self.pos = p - len as f64;
        self.prev = self.mono[len - 1];
        self.have_prev = true;
        &self.out
    }
}

fn capture_thread<F>(
    mut sink: F,
    control: &Control,
    stats: &CaptureStats,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;
    }
    let _com = ComGuard;

    let result = (|| -> Result<(), String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("device enumerator: {e}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .map_err(|e| format!("no default capture device: {e}"))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("activate audio client: {e}"))?;

            let mix = client
                .GetMixFormat()
                .map_err(|e| format!("GetMixFormat: {e}"))?;
            let (src_rate, channels) = {
                let f = &*mix;
                (f.nSamplesPerSec, f.nChannels as usize)
            };

            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    0,
                    0,
                    mix,
                    None,
                )
                .map_err(|e| format!("IAudioClient::Initialize: {e}"))?;

            let audio_event: HANDLE = CreateEventW(None, false, false, PCWSTR::null())
                .map_err(|e| format!("CreateEventW: {e}"))?;
            client
                .SetEventHandle(audio_event)
                .map_err(|e| format!("SetEventHandle: {e}"))?;

            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| format!("IAudioCaptureClient: {e}"))?;

            let is_float = format_is_float(mix);
            let bits = (*mix).wBitsPerSample as usize;
            CoTaskMemFree(Some(mix as *const _));

            // Audio transport gets multimedia scheduling. Inference deliberately
            // does not: starving audio and UI would make latency worse.
            let mut task_index: u32 = 0;
            let mmcss =
                AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task_index).ok();

            let mut conv = Converter::new(src_rate, channels);
            let mut scratch: Vec<f32> = Vec::with_capacity(8192);
            // Touch the buffers once so the first real packet allocates nothing.
            scratch.resize(8192, 0.0);
            let _ = conv.convert(&scratch);
            scratch.clear();
            conv.reset();

            let _ = ready.send(Ok(()));

            let handles = [control.wake, audio_event];
            let mut running = false;

            loop {
                let signalled = WaitForMultipleObjects(&handles, false, INFINITE);
                let index = signalled.0.wrapping_sub(WAIT_OBJECT_0.0);

                if control.shutdown.load(Ordering::SeqCst) {
                    break;
                }

                let want = control.armed.load(Ordering::SeqCst);
                if want != running {
                    if want {
                        // Reset while stopped throws away whatever the device
                        // buffered before the user asked to dictate.
                        let _ = client.Reset();
                        conv.reset();
                        if client.Start().is_err() {
                            stats.device_errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            running = true;
                        }
                    } else {
                        let _ = client.Stop();
                        running = false;
                        continue;
                    }
                }

                if !running || index != 1 {
                    // Woken by the control event, or a spurious wake.
                    if !running {
                        continue;
                    }
                }

                loop {
                    let packet_frames = match capture.GetNextPacketSize() {
                        Ok(n) => n,
                        Err(_) => {
                            stats.device_errors.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    };
                    if packet_frames == 0 {
                        break;
                    }

                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    if capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .is_err()
                    {
                        stats.device_errors.fetch_add(1, Ordering::Relaxed);
                        break;
                    }

                    // AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY
                    if flags & 1 != 0 {
                        stats.glitches.fetch_add(1, Ordering::Relaxed);
                    }

                    let count = frames as usize * channels;
                    scratch.clear();
                    // AUDCLNT_BUFFERFLAGS_SILENT
                    if flags & 2 != 0 || data.is_null() {
                        scratch.resize(count, 0.0);
                    } else if is_float && bits == 32 {
                        scratch.extend_from_slice(std::slice::from_raw_parts(
                            data as *const f32,
                            count,
                        ));
                    } else if bits == 16 {
                        let src = std::slice::from_raw_parts(data as *const i16, count);
                        scratch.extend(src.iter().map(|s| *s as f32 / 32768.0));
                    } else if bits == 32 {
                        let src = std::slice::from_raw_parts(data as *const i32, count);
                        scratch.extend(src.iter().map(|s| *s as f32 / 2147483648.0));
                    } else {
                        scratch.resize(count, 0.0);
                    }

                    let _ = capture.ReleaseBuffer(frames);
                    stats.packets.fetch_add(1, Ordering::Relaxed);

                    let converted = conv.convert(&scratch);
                    if !converted.is_empty() {
                        sink(converted);
                    }
                }
            }

            if running {
                let _ = client.Stop();
            }
            if let Some(h) = mmcss {
                let _ = AvRevertMmThreadCharacteristics(h);
            }
            let _ = CloseHandle(audio_event);
            Ok(())
        }
    })();

    if let Err(ref e) = result {
        let _ = ready.send(Err(e.clone()));
    }
    result
}

/// True when the mix format carries IEEE float samples, including the
/// WAVE_FORMAT_EXTENSIBLE spelling most Windows endpoints report.
unsafe fn format_is_float(mix: *const WAVEFORMATEX) -> bool {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    let tag = unsafe { (*mix).wFormatTag };
    if tag == WAVE_FORMAT_IEEE_FLOAT {
        return true;
    }
    if tag == WAVE_FORMAT_EXTENSIBLE {
        let ext = mix as *const WAVEFORMATEXTENSIBLE;
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        return unsafe { (*ext).SubFormat.data1 } == 3;
    }
    false
}

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_48k_stereo_to_16k_mono() {
        let mut c = Converter::new(48_000, 2);
        // 480 stereo frames = 10 ms at 48 kHz, which is 160 samples at 16 kHz.
        let input: Vec<f32> = (0..960).map(|i| (i % 2) as f32).collect();
        let out = c.convert(&input);
        assert!(
            (out.len() as i32 - 160).abs() <= 1,
            "expected ~160 samples, got {}",
            out.len()
        );
    }

    #[test]
    fn passes_16k_mono_through_untouched() {
        let mut c = Converter::new(16_000, 1);
        let input: Vec<f32> = vec![0.25; 320];
        let out = c.convert(&input);
        assert_eq!(out, &input[..]);
    }

    #[test]
    fn keeps_rate_across_packet_boundaries() {
        let mut c = Converter::new(44_100, 1);
        let mut total = 0usize;
        for _ in 0..100 {
            total += c.convert(&vec![0.0; 441]).len();
        }
        // One second of input must produce 16000 samples, give or take one.
        assert!((total as i32 - 16_000).abs() <= 2, "got {total}");
    }

    #[test]
    fn reset_clears_continuity() {
        let mut c = Converter::new(48_000, 1);
        let a = c.convert(&vec![1.0; 480]).len();
        c.reset();
        let b = c.convert(&vec![1.0; 480]).len();
        assert_eq!(a, b);
    }
}

//! PipeWire microphone capture.
//!
//! Mirrors the Windows WASAPI backend's lifecycle exactly: the stream is
//! created and connected to the graph at startup with `INACTIVE`, so the node
//! exists and is already linked to the default source, but nothing runs and no
//! microphone indicator lights. `arm()` flips it to active, which is a node
//! state change inside the PipeWire graph rather than a device open, so the
//! first word of a dictation is not clipped.
//!
//! Everything PipeWire touches lives on one thread ("flow-capture") which owns
//! the main loop, context, core and stream. `arm()` / `disarm()` are messages
//! posted into that loop through a `pipewire::channel`, so the caller never
//! blocks on the graph.
//!
//! The process callback runs on PipeWire's real-time data thread. It copies the
//! chunk out, converts if the negotiated format is not already 16 kHz mono f32,
//! and hands the samples to the sink. No allocation after warm-up, no locks.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::audio_convert::Converter;

use pipewire as pw;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

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
    /// Microseconds the last `set_active(true)` took, measured on the loop
    /// thread. This is the Linux counterpart of `IAudioClient::Start` and is
    /// the half of key-down-to-first-sample that the app can control.
    pub last_start_us: AtomicU32,
    /// Microseconds spent discarding stream state before arming. On PipeWire
    /// that is a single atomic store, so this reads as 0; it is kept so the
    /// mic-test output is identical on both platforms.
    pub last_reset_us: AtomicU32,
}

/// What the public handle asks the loop thread to do.
enum Cmd {
    Arm,
    Disarm,
    Shutdown,
}

pub struct Capture {
    tx: pw::channel::Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub stats: Arc<CaptureStats>,
}

/// Shared with the real-time process callback. Both flags are written by the
/// loop thread and read by the data thread, so they are atomics rather than
/// fields of the listener's user data.
struct Shared {
    armed: AtomicBool,
    /// Set on arm so the first packet of a new utterance does not interpolate
    /// against the last packet of the previous one.
    reset: AtomicBool,
}

impl Capture {
    /// Connects a capture stream without activating it. `sink` runs on the
    /// PipeWire data thread with 16 kHz mono f32 samples, so it must be cheap:
    /// copy and return.
    pub fn open<F>(sink: F) -> Result<Capture, String>
    where
        F: FnMut(&[f32]) + Send + 'static,
    {
        let stats = Arc::new(CaptureStats::default());
        let shared = Arc::new(Shared {
            armed: AtomicBool::new(false),
            reset: AtomicBool::new(false),
        });
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread = {
            let stats = Arc::clone(&stats);
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("flow-capture".into())
                .spawn(move || {
                    if let Err(e) = capture_thread(sink, rx, &stats, &shared, &ready_tx) {
                        stats.device_errors.fetch_add(1, Ordering::Relaxed);
                        let _ = ready_tx.send(Err(e.clone()));
                        eprintln!("capture thread ended: {e}");
                    }
                })
                .map_err(|e| e.to_string())?
        };

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Capture {
                tx,
                thread: Some(thread),
                stats,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(e) => Err(format!("capture device did not open: {e}")),
        }
    }

    /// Key-down. Activates the stream; samples begin reaching the sink.
    pub fn arm(&self) {
        let _ = self.tx.send(Cmd::Arm);
    }

    /// Key-up. Deactivates the stream so the next dictation cannot begin with
    /// stale audio.
    pub fn disarm(&self) {
        let _ = self.tx.send(Cmd::Disarm);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Listener state. Lives on the loop thread and is borrowed mutably by the
/// param_changed and process callbacks, which never run concurrently.
struct UserData<F> {
    /// Negotiated source rate and channel count, from the format pod.
    rate: u32,
    channels: usize,
    format: AudioFormat,
    /// Used only when the negotiated format is not already 16 kHz mono.
    conv: Converter,
    passthrough: bool,
    scratch: Vec<f32>,
    sink: F,
    stats: Arc<CaptureStats>,
    shared: Arc<Shared>,
}

fn capture_thread<F>(
    sink: F,
    rx: pw::channel::Receiver<Cmd>,
    stats: &Arc<CaptureStats>,
    shared: &Arc<Shared>,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(pw::init);

    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("pipewire loop: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| format!("pipewire context: {e}"))?;
    let core = context.connect_rc(None).map_err(|e| {
        format!("no PipeWire daemon on {} ({e}); is pipewire.service running?", socket_hint())
    })?;

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "Flow",
        *pw::keys::NODE_NAME => "flow-capture",
        // 10 ms quantum, matching the WASAPI packet size the app is tuned for.
        *pw::keys::NODE_LATENCY => "160/16000",
        // Idle while inactive: no device wakeups and no microphone indicator.
        *pw::keys::NODE_ALWAYS_PROCESS => "false",
    };

    let stream = pw::stream::StreamRc::new(core.clone(), "Flow", props)
        .map_err(|e| format!("pipewire stream: {e}"))?;

    let data = UserData {
        rate: TARGET_SR,
        channels: 1,
        format: AudioFormat::F32LE,
        conv: Converter::new(TARGET_SR, 1),
        passthrough: true,
        scratch: Vec::with_capacity(8192),
        sink,
        stats: Arc::clone(stats),
        shared: Arc::clone(shared),
    };

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_err() {
                ud.stats.device_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
            ud.rate = info.rate().max(1);
            ud.channels = info.channels().max(1) as usize;
            ud.format = info.format();
            ud.passthrough =
                ud.rate == TARGET_SR && ud.channels == 1 && ud.format == AudioFormat::F32LE;
            ud.conv = Converter::new(ud.rate, ud.channels);
            // Warm the converter's buffers so the first real packet allocates
            // nothing on the real-time thread.
            ud.scratch.resize(8192, 0.0);
            let _ = ud.conv.convert(&ud.scratch);
            ud.conv.reset();
            ud.scratch.clear();
            ud.scratch.reserve(8192);
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                ud.stats.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                ud.stats.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let data = &mut datas[0];
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let armed = ud.shared.armed.load(Ordering::Relaxed);
            if size == 0 || !armed {
                // A buffer with nothing in it, or one still in flight from
                // before disarm. Neither reaches the model.
                ud.stats.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let Some(bytes) = data.data() else {
                ud.stats.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let end = (offset + size).min(bytes.len());
            if end <= offset {
                ud.stats.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let bytes = &bytes[offset..end];

            if ud.shared.reset.swap(false, Ordering::Relaxed) {
                ud.conv.reset();
            }

            ud.scratch.clear();
            decode(bytes, ud.format, &mut ud.scratch);
            ud.stats.packets.fetch_add(1, Ordering::Relaxed);

            if ud.passthrough {
                if !ud.scratch.is_empty() {
                    (ud.sink)(&ud.scratch);
                }
            } else {
                let converted = ud.conv.convert(&ud.scratch);
                if !converted.is_empty() {
                    (ud.sink)(converted);
                }
            }
        })
        .register()
        .map_err(|e| format!("pipewire stream listener: {e}"))?;

    // Ask for F32LE mono 16 kHz. PipeWire's converter handles the device's real
    // rate, and `param_changed` keeps the fallback path honest if the graph
    // hands us something else.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(TARGET_SR);
    audio_info.set_channels(1);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| format!("format pod: {e}"))?
    .0
    .into_inner();
    let Some(pod) = Pod::from_bytes(&values) else {
        return Err("format pod: malformed".into());
    };
    let mut params = [pod];

    // AUTOCONNECT links the node to the default source now, at startup, so
    // arming later is only a state change. INACTIVE keeps it idle until then.
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::INACTIVE
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("connect capture stream: {e}"))?;

    let _receiver = rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        let stream = stream.clone();
        let stats = Arc::clone(stats);
        let shared = Arc::clone(shared);
        move |cmd| match cmd {
            Cmd::Arm => {
                let t_reset = std::time::Instant::now();
                shared.reset.store(true, Ordering::SeqCst);
                shared.armed.store(true, Ordering::SeqCst);
                stats
                    .last_reset_us
                    .store(t_reset.elapsed().as_micros() as u32, Ordering::Relaxed);
                let t_start = std::time::Instant::now();
                let started = stream.set_active(true);
                stats
                    .last_start_us
                    .store(t_start.elapsed().as_micros() as u32, Ordering::Relaxed);
                if started.is_err() {
                    stats.device_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            Cmd::Disarm => {
                shared.armed.store(false, Ordering::SeqCst);
                if stream.set_active(false).is_err() {
                    stats.device_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            Cmd::Shutdown => {
                shared.armed.store(false, Ordering::SeqCst);
                let _ = stream.set_active(false);
                mainloop.quit();
            }
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// Copies one chunk out of the PipeWire buffer as f32 samples. F32LE is what we
/// ask for and all but certainly what we get; the integer arms exist so an
/// unusual graph produces quiet audio rather than noise.
fn decode(bytes: &[u8], format: AudioFormat, out: &mut Vec<f32>) {
    match format {
        AudioFormat::F32LE => {
            out.extend(
                bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        AudioFormat::S16LE => {
            out.extend(
                bytes
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
            );
        }
        AudioFormat::S32LE => {
            out.extend(
                bytes
                    .chunks_exact(4)
                    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0),
            );
        }
        _ => {}
    }
}

fn socket_hint() -> String {
    match std::env::var("PIPEWIRE_REMOTE") {
        Ok(v) if !v.is_empty() => v,
        _ => "pipewire-0".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_f32le() {
        let mut out = Vec::new();
        let mut bytes = Vec::new();
        for v in [0.0f32, 0.5, -0.25] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        decode(&bytes, AudioFormat::F32LE, &mut out);
        assert_eq!(out, vec![0.0, 0.5, -0.25]);
    }

    #[test]
    fn decodes_s16le_into_unit_range() {
        let mut out = Vec::new();
        let bytes: Vec<u8> = [0i16, 16384, -32768]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        decode(&bytes, AudioFormat::S16LE, &mut out);
        assert_eq!(out, vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn ignores_a_trailing_partial_sample() {
        let mut out = Vec::new();
        // Nine bytes is two whole f32s and one byte of a third.
        decode(&[0u8; 9], AudioFormat::F32LE, &mut out);
        assert_eq!(out.len(), 2);
    }
}

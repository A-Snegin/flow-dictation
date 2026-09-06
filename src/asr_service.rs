//! The ASR worker: one thread, one transcriber, one command queue.
//!
//! Two rules decide the north-star latency and both live here.
//!
//! 1. Keep the model caught up while the user is still speaking. Partials are
//!    issued with FORCE_UPDATE on a tight cadence so that at key-up there is
//!    almost no unprocessed audio left. Measured on the 7736U: this drives the
//!    drain after `stop_stream` to roughly zero.
//! 2. Never start a partial once release is pending. The library cannot cancel
//!    an in-flight call, so the only bound we control is refusing to begin one.
//!    That caps the key-up wait at whatever call was already running.

use crate::asr::{join, Line, Transcriber};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Event {
    /// Live hypothesis while the user speaks.
    Partial { text: String },
    /// Utterance finished. `wait_for_inflight` is how long the release had to
    /// wait for a partial that was already running, `drain` is the stop plus
    /// final transcribe.
    Final {
        text: String,
        lines: Vec<Line>,
        wait_for_inflight: Duration,
        drain: Duration,
        total: Duration,
    },
    Error(String),
}

#[derive(Clone, Copy)]
pub struct Config {
    /// Minimum gap between partial calls. The library also throttles itself to
    /// 200 ms of new audio unless `force` is set.
    pub partial_cadence: Duration,
    pub force_partials: bool,
    /// How long to keep collecting after key-up before ending the stream.
    /// WASAPI delivers on the device period, so the last few milliseconds of
    /// speech are still in flight when the key comes up. Cheap insurance
    /// against a clipped final consonant.
    pub release_tail: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            partial_cadence: Duration::from_millis(250),
            force_partials: false,
            release_tail: Duration::from_millis(15),
        }
    }
}

enum Cmd {
    Begin,
    Release,
    SetKeyterms(String),
    SetConfig(Config),
    Shutdown,
}

/// Audio handed from the capture side to the ASR thread. The producer appends
/// under a mutex held for microseconds; the consumer swaps the whole buffer for
/// a spare, so inference never runs while the lock is held.
#[derive(Default)]
struct Staging {
    buf: Vec<f32>,
}

pub use crate::platform::sys::Waker;

pub struct AsrService {
    cmd: Sender<Cmd>,
    staging: Arc<Mutex<Staging>>,
    release_pending: Arc<AtomicBool>,
    /// Caller-side timestamp of the hotkey release, so the worker can report
    /// how long the user actually waited for a call it could not cancel.
    release_at: Arc<Mutex<Option<Instant>>>,
    pub events: Receiver<Event>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AsrService {
    pub fn spawn(transcriber: Transcriber, config: Config) -> AsrService {
        Self::spawn_with(transcriber, config, AudioSink::new(), Waker::default())
    }

    /// Takes a sink created earlier, so the audio device can be opened in
    /// parallel with loading the model rather than after it.
    pub fn spawn_with(
        transcriber: Transcriber,
        config: Config,
        sink: AudioSink,
        waker: Waker,
    ) -> AsrService {
        let (cmd_tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, ev_rx) = channel::<Event>();
        let staging = sink.staging;
        let release_pending = Arc::new(AtomicBool::new(false));
        let release_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

        let worker = {
            let staging = Arc::clone(&staging);
            let release_pending = Arc::clone(&release_pending);
            let release_at = Arc::clone(&release_at);
            std::thread::Builder::new()
                .name("flow-asr".into())
                .spawn(move || {
                    // A notch above normal, not real time. The work is bursty
                    // and latency-critical, and on a machine with a busy
                    // background it was being descheduled mid-decode. Real time
                    // would be wrong: starving audio capture or the UI to feed
                    // the recogniser makes the felt latency worse, not better.
                    raise_priority();
                    worker_loop(
                        transcriber,
                        config,
                        cmd_rx,
                        ev_tx,
                        staging,
                        release_pending,
                        release_at,
                        waker,
                    )
                })
                .expect("spawn asr thread")
        };

        AsrService {
            cmd: cmd_tx,
            staging,
            release_pending,
            release_at,
            events: ev_rx,
            worker: Some(worker),
        }
    }

    /// Hotkey down.
    pub fn begin(&self) {
        self.release_pending.store(false, Ordering::SeqCst);
        let _ = self.cmd.send(Cmd::Begin);
    }

    /// Called from the audio side. Cheap: a lock and a memcpy.
    pub fn push_audio(&self, samples: &[f32]) {
        if let Ok(mut s) = self.staging.lock() {
            s.buf.extend_from_slice(samples);
        }
    }

    /// Changes the timing knobs on the running worker. Applied between
    /// utterances, so a change cannot land halfway through one.
    pub fn set_config(&self, config: Config) {
        let _ = self.cmd.send(Cmd::SetConfig(config));
    }

    /// Replaces the decoder's biasing terms. Applied by the worker between
    /// utterances, so it never interrupts one in progress.
    pub fn set_keyterms(&self, terms: &str) {
        let _ = self.cmd.send(Cmd::SetKeyterms(terms.to_string()));
    }

    /// A handle the capture thread can own. The service itself holds an mpsc
    /// Receiver and so is not Sync; the audio path needs neither.
    pub fn audio_sink(&self) -> AudioSink {
        AudioSink {
            staging: Arc::clone(&self.staging),
        }
    }

    /// Hotkey up. Sets the flag first so the worker refuses to start another
    /// partial even before it drains the command queue.
    pub fn release(&self) {
        if let Ok(mut r) = self.release_at.lock() {
            *r = Some(Instant::now());
        }
        self.release_pending.store(true, Ordering::SeqCst);
        let _ = self.cmd.send(Cmd::Release);
    }
}

/// Write-only handle to the ASR worker's input buffer. Created before the
/// worker so the capture device can be opened while the model is still loading.
#[derive(Clone)]
pub struct AudioSink {
    staging: Arc<Mutex<Staging>>,
}

impl Default for AudioSink {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSink {
    pub fn new() -> AudioSink {
        AudioSink {
            staging: Arc::new(Mutex::new(Staging::default())),
        }
    }

    pub fn push(&self, samples: &[f32]) {
        if let Ok(mut s) = self.staging.lock() {
            s.buf.extend_from_slice(samples);
        }
    }

    /// Drops anything captured but not yet consumed, for a cancelled utterance.
    pub fn clear(&self) {
        if let Ok(mut s) = self.staging.lock() {
            s.buf.clear();
        }
    }
}

impl Drop for AsrService {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Shutdown);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

use crate::platform::sys::raise_priority;

fn worker_loop(
    transcriber: Transcriber,
    mut config: Config,
    cmd_rx: Receiver<Cmd>,
    ev_tx: Sender<Event>,
    staging: Arc<Mutex<Staging>>,
    release_pending: Arc<AtomicBool>,
    release_at: Arc<Mutex<Option<Instant>>>,
    waker: Waker,
) {
    const SR: i32 = 16_000;
    let mut spare: Vec<f32> = Vec::with_capacity(SR as usize * 2);

    loop {
        // Idle: block until something happens. Zero CPU between utterances.
        match cmd_rx.recv() {
            Ok(Cmd::Begin) => {}
            Ok(Cmd::Release) => continue,
            Ok(Cmd::SetKeyterms(terms)) => {
                if let Err(e) = transcriber.set_keyterms(&terms) {
                    let _ = ev_tx.send(Event::Error(format!("keyterms rejected: {e}")));
                }
                continue;
            }
            Ok(Cmd::SetConfig(fresh)) => {
                config = fresh;
                continue;
            }
            Ok(Cmd::Shutdown) | Err(_) => return,
        }

        let stream = match transcriber.create_stream() {
            Ok(s) => s,
            Err(e) => {
                let _ = ev_tx.send(Event::Error(e));
                continue;
            }
        };
        if let Err(e) = stream.start() {
            let _ = ev_tx.send(Event::Error(e));
            continue;
        }

        // Anything captured between key-down and the worker waking up.
        drain_into_stream(&staging, &mut spare, &stream, SR, &ev_tx);

        let mut last_partial = Instant::now() - config.partial_cadence;
        let release_instant;

        loop {
            if release_pending.load(Ordering::SeqCst) {
                release_instant = Instant::now();
                break;
            }
            match cmd_rx.try_recv() {
                Ok(Cmd::Release) => {
                    release_instant = Instant::now();
                    break;
                }
                Ok(Cmd::Shutdown) => return,
                Ok(Cmd::SetKeyterms(terms)) => {
                    // Arrived mid-utterance. Apply it rather than drop it; the
                    // library replaces the trie without disturbing the stream.
                    if let Err(e) = transcriber.set_keyterms(&terms) {
                        let _ = ev_tx.send(Event::Error(format!("keyterms rejected: {e}")));
                    }
                }
                Ok(Cmd::SetConfig(fresh)) => config = fresh,
                Ok(Cmd::Begin) => {}
                Err(_) => {}
            }

            drain_into_stream(&staging, &mut spare, &stream, SR, &ev_tx);

            if last_partial.elapsed() >= config.partial_cadence {
                // Re-check immediately before committing to a call we cannot
                // cancel. This is the whole bound on key-up latency.
                if release_pending.load(Ordering::SeqCst) {
                    release_instant = Instant::now();
                    break;
                }
                match stream.transcribe(config.force_partials) {
                    Ok(lines) => {
                        let text = join(&lines);
                        if !text.is_empty() {
                            let _ = ev_tx.send(Event::Partial { text });
                            waker.wake();
                        }
                    }
                    Err(e) => {
                        let _ = ev_tx.send(Event::Error(e));
                    }
                }
                last_partial = Instant::now();
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        // The wait the user actually pays for a call we could not cancel:
        // from their key-up to the moment the worker was free to act.
        let released_at = release_at
            .lock()
            .ok()
            .and_then(|r| *r)
            .unwrap_or(release_instant);
        let wait_for_inflight = released_at.elapsed();

        let t_drain = Instant::now();
        drain_into_stream(&staging, &mut spare, &stream, SR, &ev_tx);
        // Let the last device packet land before ending input.
        let tail_deadline = released_at + config.release_tail;
        let now = Instant::now();
        if tail_deadline > now {
            std::thread::sleep(tail_deadline - now);
            drain_into_stream(&staging, &mut spare, &stream, SR, &ev_tx);
        }
        let final_lines = match stream.stop().and_then(|_| stream.transcribe(false)) {
            Ok(l) => l,
            Err(e) => {
                let _ = ev_tx.send(Event::Error(e));
                Vec::new()
            }
        };
        let drain = t_drain.elapsed();

        let _ = ev_tx.send(Event::Final {
            text: join(&final_lines),
            lines: final_lines,
            wait_for_inflight,
            drain,
            total: released_at.elapsed(),
        });
        waker.wake();

        release_pending.store(false, Ordering::SeqCst);
    }
}

fn drain_into_stream(
    staging: &Arc<Mutex<Staging>>,
    spare: &mut Vec<f32>,
    stream: &crate::asr::Stream<'_>,
    sr: i32,
    ev_tx: &Sender<Event>,
) {
    spare.clear();
    if let Ok(mut s) = staging.lock() {
        if s.buf.is_empty() {
            return;
        }
        std::mem::swap(&mut s.buf, spare);
    }
    if spare.is_empty() {
        return;
    }
    if let Err(e) = stream.add_audio(spare, sr) {
        let _ = ev_tx.send(Event::Error(e));
    }
}

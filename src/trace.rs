//! Latency instrumentation.
//!
//! The trace points are the ones the brief names, T0 to T12, timed with
//! the platform monotonic counter so they are consistent across threads and immune
//! to wall-clock adjustments. Every utterance appends one JSON line locally and
//! nothing ever leaves the machine.

use std::sync::Mutex;

pub use crate::platform::sys::{freq, now};

use crate::stats::Samples;

pub fn ms_between(a: i64, b: i64) -> f64 {
    (b - a) as f64 * 1000.0 / freq() as f64
}

/// One dictation, from key-down to text on screen.
#[derive(Clone, Copy)]
pub struct Utterance {
    pub t0_hotkey_down: i64,
    /// When the microphone was actually started. Anything between this and
    /// t0 is work the app did before it began listening, and it is speech the
    /// user has already lost.
    pub t0b_armed: i64,
    pub t1_first_packet: i64,
    pub t6_first_partial: i64,
    pub t7_hotkey_up: i64,
    pub t9_final_ready: i64,
    pub t10_formatted: i64,
    pub t12_inserted: i64,
    pub chars: usize,
    pub audio_ms: f64,
    /// Split of the arm cost, in microseconds, straight from the capture
    /// thread: resetting the stream and starting it.
    pub reset_us: u32,
    pub start_us: u32,
    /// Loudest sample the microphone delivered during this utterance, 0..1.
    /// Near zero with text in the transcript would mean the level plumbing is
    /// broken rather than the microphone.
    pub peak_level: f32,
    /// How the text was put in, and whether the target was treated as a
    /// terminal. Paste is flat in the number of characters and typing is not,
    /// so without this the insertion timings cannot be compared.
    pub insert_mode: &'static str,
    pub terminal: bool,
}

impl Default for Utterance {
    fn default() -> Self {
        Utterance {
            t0_hotkey_down: 0,
            t0b_armed: 0,
            t1_first_packet: 0,
            t6_first_partial: 0,
            t7_hotkey_up: 0,
            t9_final_ready: 0,
            t10_formatted: 0,
            t12_inserted: 0,
            chars: 0,
            audio_ms: 0.0,
            reset_us: 0,
            start_us: 0,
            peak_level: 0.0,
            insert_mode: "none",
            terminal: false,
        }
    }
}

impl Utterance {
    pub fn begin() -> Utterance {
        Utterance {
            t0_hotkey_down: now(),
            ..Default::default()
        }
    }

    /// Hotkey up to text visible in the target application. The north star.
    pub fn user_perceived_ms(&self) -> f64 {
        ms_between(self.t7_hotkey_up, self.t12_inserted)
    }

    pub fn activation_ms(&self) -> f64 {
        ms_between(self.t0_hotkey_down, self.t1_first_packet)
    }

    /// Hotkey to microphone running. Should be almost nothing.
    pub fn arm_ms(&self) -> f64 {
        ms_between(self.t0_hotkey_down, self.t0b_armed)
    }

    pub fn finalisation_ms(&self) -> f64 {
        ms_between(self.t7_hotkey_up, self.t9_final_ready)
    }

    pub fn insertion_ms(&self) -> f64 {
        ms_between(self.t9_final_ready, self.t12_inserted)
    }

    pub fn first_partial_ms(&self) -> f64 {
        if self.t6_first_partial == 0 {
            f64::NAN
        } else {
            ms_between(self.t0_hotkey_down, self.t6_first_partial)
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"user_perceived_ms\":{:.1},\"activation_ms\":{:.1},\"arm_ms\":{:.1},\
             \"finalisation_ms\":{:.1},\
             \"insertion_ms\":{:.1},\"first_partial_ms\":{:.1},\"chars\":{},\
             \"reset_us\":{},\"start_us\":{},\"peak_level\":{:.3},             \"insert_mode\":\"{}\",\"terminal\":{}}}",
            self.user_perceived_ms(),
            self.activation_ms(),
            self.arm_ms(),
            self.finalisation_ms(),
            self.insertion_ms(),
            self.first_partial_ms(),
            self.chars,
            self.reset_us,
            self.start_us,
            self.peak_level,
            self.insert_mode,
            self.terminal
        )
    }
}

#[derive(Default)]
struct Accumulated {
    perceived: Samples,
    activation: Samples,
    finalisation: Samples,
    insertion: Samples,
    first_partial: Samples,
}

static ACC: Mutex<Option<Accumulated>> = Mutex::new(None);

/// Records one utterance: percentiles in memory, a JSON line on disk.
pub fn record(u: &Utterance) {
    if let Ok(mut guard) = ACC.lock() {
        let acc = guard.get_or_insert_with(Accumulated::default);
        acc.perceived.push_ms(u.user_perceived_ms());
        acc.activation.push_ms(u.activation_ms());
        acc.finalisation.push_ms(u.finalisation_ms());
        acc.insertion.push_ms(u.insertion_ms());
        let fp = u.first_partial_ms();
        if fp.is_finite() {
            acc.first_partial.push_ms(fp);
        }
    }

    let path = crate::settings::local_root().join("traces.jsonl");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(f, "{}", u.to_json());
    }
}

/// The report behind the tray menu item.
pub fn report() -> String {
    let guard = match ACC.lock() {
        Ok(g) => g,
        Err(_) => return "no samples".into(),
    };
    let Some(acc) = guard.as_ref() else {
        return "No dictations recorded yet.".into();
    };
    if acc.perceived.is_empty() {
        return "No dictations recorded yet.".into();
    }
    format!(
        "{}\n{}\n{}\n{}\n{}",
        acc.perceived.report("KEY UP -> TEXT ON SCREEN"),
        acc.finalisation.report("  key up -> transcript ready"),
        acc.insertion.report("  transcript -> inserted"),
        acc.activation.report("hotkey -> first audio packet"),
        acc.first_partial.report("hotkey -> first partial"),
    )
}

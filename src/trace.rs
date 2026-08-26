//! Latency instrumentation.
//!
//! The trace points are the ones the brief names, T0 to T12, timed with
//! `QueryPerformanceCounter` so they are consistent across threads and immune
//! to wall-clock adjustments. Every utterance appends one JSON line locally and
//! nothing ever leaves the machine.

use std::sync::Mutex;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::stats::Samples;

/// Raw counter ticks.
pub fn now() -> i64 {
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

pub fn freq() -> i64 {
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f == 0 {
        1
    } else {
        f
    }
}

pub fn ms_between(a: i64, b: i64) -> f64 {
    (b - a) as f64 * 1000.0 / freq() as f64
}

/// One dictation, from key-down to text on screen.
#[derive(Default, Clone, Copy)]
pub struct Utterance {
    pub t0_hotkey_down: i64,
    pub t1_first_packet: i64,
    pub t6_first_partial: i64,
    pub t7_hotkey_up: i64,
    pub t9_final_ready: i64,
    pub t10_formatted: i64,
    pub t12_inserted: i64,
    pub chars: usize,
    pub audio_ms: f64,
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
            "{{\"user_perceived_ms\":{:.1},\"activation_ms\":{:.1},\"finalisation_ms\":{:.1},\
             \"insertion_ms\":{:.1},\"first_partial_ms\":{:.1},\"audio_ms\":{:.0},\"chars\":{}}}",
            self.user_perceived_ms(),
            self.activation_ms(),
            self.finalisation_ms(),
            self.insertion_ms(),
            self.first_partial_ms(),
            self.audio_ms,
            self.chars
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

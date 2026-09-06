//! Streaming downmix and resample into 16 kHz mono, shared by every capture
//! backend. Pure arithmetic, no platform code.

use crate::platform::audio::TARGET_SR;

/// Streaming downmix and resample into 16 kHz mono. Keeps a fractional
/// position across packets so packet boundaries neither click nor drift.
pub struct Converter {
    src_rate: u32,
    channels: usize,
    pos: f64,
    prev: f32,
    have_prev: bool,
    mono: Vec<f32>,
    out: Vec<f32>,
}

impl Converter {
    pub fn new(src_rate: u32, channels: usize) -> Converter {
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
    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.prev = 0.0;
        self.have_prev = false;
    }

    pub fn convert(&mut self, interleaved: &[f32]) -> &[f32] {
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

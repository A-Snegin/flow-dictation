//! Minimal WAV reader. Enough for the bench corpus and for regression fixtures:
//! PCM 16-bit and IEEE float 32-bit, any channel count, any sample rate.

pub struct Wav {
    pub samples: Vec<f32>, // interleaved
    pub channels: u16,
    pub sample_rate: u32,
}

impl Wav {
    pub fn read(path: &str) -> Result<Wav, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(format!("{path}: not a RIFF/WAVE file"));
        }
        let mut pos = 12usize;
        let (mut format, mut channels, mut sample_rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
        let mut data: Option<&[u8]> = None;

        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let body_start = pos + 8;
            let body_end = (body_start + size).min(bytes.len());
            match id {
                b"fmt " => {
                    let b = &bytes[body_start..body_end];
                    if b.len() < 16 {
                        return Err(format!("{path}: short fmt chunk"));
                    }
                    format = u16::from_le_bytes(b[0..2].try_into().unwrap());
                    channels = u16::from_le_bytes(b[2..4].try_into().unwrap());
                    sample_rate = u32::from_le_bytes(b[4..8].try_into().unwrap());
                    bits = u16::from_le_bytes(b[14..16].try_into().unwrap());
                    if format == 0xFFFE && b.len() >= 26 {
                        // WAVE_FORMAT_EXTENSIBLE: the real tag is the first GUID field.
                        format = u16::from_le_bytes(b[24..26].try_into().unwrap());
                    }
                }
                b"data" => data = Some(&bytes[body_start..body_end]),
                _ => {}
            }
            pos = body_start + size + (size & 1); // chunks are word-aligned
        }

        let data = data.ok_or_else(|| format!("{path}: no data chunk"))?;
        let samples = match (format, bits) {
            (1, 16) => data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect(),
            (1, 32) => data
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as f32 / 2147483648.0)
                .collect(),
            (3, 32) => data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            _ => return Err(format!("{path}: unsupported format {format} / {bits} bits")),
        };

        Ok(Wav { samples, channels, sample_rate })
    }

    /// Interleaved to mono, averaging channels.
    pub fn to_mono(&self) -> Vec<f32> {
        if self.channels <= 1 {
            return self.samples.clone();
        }
        let n = self.channels as usize;
        self.samples
            .chunks_exact(n)
            .map(|f| f.iter().sum::<f32>() / n as f32)
            .collect()
    }

    pub fn duration_secs(&self) -> f64 {
        let frames = self.samples.len() / self.channels.max(1) as usize;
        frames as f64 / self.sample_rate as f64
    }
}

/// Linear-interpolation resampler. One pass, preallocated output.
/// Good enough for speech into a 16 kHz model; measured against the model's
/// own internal resampler in Phase 2.
pub fn resample_into(input: &[f32], from_hz: u32, to_hz: u32, out: &mut Vec<f32>) {
    out.clear();
    if input.is_empty() {
        return;
    }
    if from_hz == to_hz {
        out.extend_from_slice(input);
        return;
    }
    let ratio = from_hz as f64 / to_hz as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    out.reserve(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = (src - idx as f64) as f32;
        let a = input[idx];
        let b = *input.get(idx + 1).unwrap_or(&a);
        out.push(a + (b - a) * frac);
    }
}

//! Word error rate against a reference transcript.
//!
//! Exists to answer one question with evidence instead of opinion: does a
//! change that is supposed to make Flow faster also make it less accurate?
//! Latency numbers are easy to produce and easy to be pleased by; a
//! configuration that shaves 200 ms and loses a word in twenty is a bad trade,
//! and nothing but this tells you that happened.
//!
//! Audio is fed at wall-clock speed through the real ASR worker, so the result
//! reflects the streaming path the app actually uses rather than a batch
//! transcription that never happens in practice.

use flow::asr::Transcriber;
use flow::asr_service::{AsrService, Config, Event};
use flow::ffi;
use flow::format::Formatter;
use flow::wav::{resample_into, Wav};
use std::time::{Duration, Instant};

const SR: i32 = 16_000;

fn main() {
    let mut model = String::from("models/small-streaming-en");
    let mut arch = ffi::ARCH_SMALL_STREAMING;
    let mut wav = String::from("bench/corpus/two_cities_16k.wav");
    let mut reference = String::from("bench/corpus/two_cities.ref.txt");
    let mut cadence_ms = 250u64;
    let mut chunk_ms = 20u64;
    let mut show = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--model" => { model = next(i); i += 1; }
            "--arch" => { arch = next(i).parse().unwrap_or(arch); i += 1; }
            "--wav" => { wav = next(i); i += 1; }
            "--ref" => { reference = next(i); i += 1; }
            "--cadence-ms" => { cadence_ms = next(i).parse().unwrap_or(cadence_ms); i += 1; }
            "--chunk-ms" => { chunk_ms = next(i).parse().unwrap_or(chunk_ms); i += 1; }
            "--show" => show = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }

    let reference_text = std::fs::read_to_string(&reference).unwrap_or_else(|e| {
        eprintln!("{reference}: {e}");
        std::process::exit(1);
    });

    let wavfile = Wav::read(&wav).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let mono = wavfile.to_mono();
    let audio = if wavfile.sample_rate == SR as u32 {
        mono
    } else {
        let mut out = Vec::new();
        resample_into(&mono, wavfile.sample_rate, SR as u32, &mut out);
        out
    };

    let transcriber = Transcriber::load(&model, arch, &[]).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let mut warm = vec![0.0f32; SR as usize / 2];
    let _ = transcriber.transcribe_once(&mut warm, SR);

    let asr = AsrService::spawn(
        transcriber,
        Config {
            partial_cadence: Duration::from_millis(cadence_ms),
            force_partials: false,
            release_tail: Duration::from_millis(15),
        },
    );

    // One long hold over the whole file: the same streaming path as a real
    // dictation, just a longer one.
    asr.begin();
    let chunk = (SR as u64 * chunk_ms / 1000) as usize;
    let chunk_dur = Duration::from_millis(chunk_ms);
    let start = Instant::now();
    let mut next_at = start;
    for c in audio.chunks(chunk) {
        next_at += chunk_dur;
        let now = Instant::now();
        if next_at > now {
            std::thread::sleep(next_at - now);
        }
        asr.push_audio(c);
        while asr.events.try_recv().is_ok() {}
    }
    asr.release();

    let mut hypothesis = String::new();
    loop {
        match asr.events.recv_timeout(Duration::from_secs(60)) {
            Ok(Event::Final { text, .. }) => {
                hypothesis = text;
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("no final transcript: {e}");
                break;
            }
        }
    }

    let formatted = Formatter::default().format(&hypothesis);
    let r = normalise(&reference_text);
    let h = normalise(&formatted);
    let (dist, subs, del, ins) = edit_distance(&r, &h);
    let wer = if r.is_empty() { f64::NAN } else { dist as f64 / r.len() as f64 };

    println!("model      {model} (arch {arch})");
    println!("audio      {} ({:.1} s)", wav, audio.len() as f64 / SR as f64);
    println!("threads    {}", if std::env::var("MOONSHINE_ORT_SINGLE_THREAD").is_ok() { "1 (forced)" } else { "onnxruntime default" });
    println!("reference  {} words", r.len());
    println!("hypothesis {} words", h.len());
    println!(
        "\nWER {:.2}%   substitutions {subs}  deletions {del}  insertions {ins}",
        wer * 100.0
    );
    if show {
        println!("\n--- reference ---\n{}", r.join(" "));
        println!("\n--- hypothesis ---\n{}", h.join(" "));
    }
}

/// Lowercase, drop punctuation, split on whitespace. The usual normalisation
/// for scoring speech: it measures the words, not the typography, which the
/// formatter is responsible for separately.
fn normalise(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// Levenshtein over words, returning the total distance and its breakdown.
fn edit_distance(r: &[String], h: &[String]) -> (usize, usize, usize, usize) {
    let (n, m) = (r.len(), h.len());
    // Full matrix: transcripts here are hundreds of words, not millions.
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
        }
    }

    // Walk back to split the distance into the three kinds of error.
    let (mut i, mut j) = (n, m);
    let (mut subs, mut del, mut ins) = (0, 0, 0);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
            if d[i][j] == d[i - 1][j - 1] + cost {
                if cost == 1 {
                    subs += 1;
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            del += 1;
            i -= 1;
        } else {
            ins += 1;
            j -= 1;
        }
    }
    (d[n][m], subs, del, ins)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        normalise(s)
    }

    #[test]
    fn identical_text_scores_zero() {
        let a = words("the quick brown fox");
        assert_eq!(edit_distance(&a, &a).0, 0);
    }

    #[test]
    fn counts_each_kind_of_error() {
        let r = words("the quick brown fox jumps");
        let h = words("the quick red fox leaps over");
        let (dist, subs, del, ins) = edit_distance(&r, &h);
        assert_eq!(subs, 2, "brown->red and jumps->leaps");
        assert_eq!(ins, 1, "over");
        assert_eq!(del, 0);
        assert_eq!(dist, 3);
    }

    #[test]
    fn normalisation_ignores_case_and_punctuation() {
        assert_eq!(words("It was, the BEST!"), words("it was the best"));
    }

    #[test]
    fn keeps_apostrophes_because_they_change_the_word() {
        assert_ne!(words("were"), words("we're"));
    }
}

//! Phase 1 latency harness. No UI, no microphone: audio is fed from a WAV at
//! wall-clock speed so the numbers mean what they say.
//!
//! Measures, in order of importance:
//!   1. flush latency        stop_stream + drain after the last sample
//!                           (the north-star number, minus insertion)
//!   2. partial call cost    one transcribe_stream while speaking, which is
//!                           the worst case an in-flight partial adds to (1)
//!   3. first partial        speech start to first non-empty hypothesis
//!   4. RTF                  model time over audio time
//!   5. fragments            whether 1-3 word holds survive the internal VAD

use flow::asr::{join, Transcriber};
use flow::ffi;
use flow::stats::Samples;
use flow::wav::{resample_into, Wav};
use std::time::{Duration, Instant};

struct Args {
    model: String,
    arch: u32,
    wav: String,
    chunk_ms: u64,
    cadence_ms: u64,
    force: bool,
    hold_secs: f64,
    utterances: usize,
    keyterms: String,
    opts: Vec<(String, String)>,
    trace: bool,
    repeat: usize,
    skip_fragments: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        model: "models/small-streaming-en".into(),
        arch: ffi::ARCH_SMALL_STREAMING,
        wav: "bench/corpus/two_cities_16k.wav".into(),
        chunk_ms: 40,
        cadence_ms: 200,
        force: false,
        hold_secs: 6.0,
        utterances: 8,
        keyterms: String::new(),
        opts: Vec::new(),
        trace: false,
        repeat: 1,
        skip_fragments: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--model" => {
                a.model = next(i);
                i += 1;
            }
            "--arch" => {
                a.arch = next(i).parse().unwrap_or(a.arch);
                i += 1;
            }
            "--wav" => {
                a.wav = next(i);
                i += 1;
            }
            "--chunk-ms" => {
                a.chunk_ms = next(i).parse().unwrap_or(a.chunk_ms);
                i += 1;
            }
            "--cadence-ms" => {
                a.cadence_ms = next(i).parse().unwrap_or(a.cadence_ms);
                i += 1;
            }
            "--hold-secs" => {
                a.hold_secs = next(i).parse().unwrap_or(a.hold_secs);
                i += 1;
            }
            "--utterances" => {
                a.utterances = next(i).parse().unwrap_or(a.utterances);
                i += 1;
            }
            "--keyterms" => {
                a.keyterms = next(i);
                i += 1;
            }
            "--opt" => {
                let kv = next(i);
                if let Some((k, v)) = kv.split_once('=') {
                    a.opts.push((k.to_string(), v.to_string()));
                } else {
                    eprintln!("--opt expects name=value, got {kv}");
                }
                i += 1;
            }
            "--repeat" => {
                a.repeat = next(i).parse().unwrap_or(a.repeat);
                i += 1;
            }
            "--trace" => a.trace = true,
            "--skip-fragments" => a.skip_fragments = true,
            "--force" => a.force = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }
    a
}

const SR: i32 = 16_000;

fn load_audio(path: &str) -> Vec<f32> {
    let wav = Wav::read(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let mono = wav.to_mono();
    if wav.sample_rate == SR as u32 {
        mono
    } else {
        let mut out = Vec::new();
        resample_into(&mono, wav.sample_rate, SR as u32, &mut out);
        out
    }
}

struct HoldResult {
    first_partial: Option<Duration>,
    partial_calls: Vec<Duration>,
    flush: Duration,
    model_time: Duration,
    audio_secs: f64,
    text: String,
}

/// One simulated push-to-talk hold, fed at wall-clock speed.
fn run_hold(
    t: &Transcriber,
    audio: &[f32],
    chunk_ms: u64,
    cadence_ms: u64,
    force: bool,
) -> Result<HoldResult, String> {
    let stream = t.create_stream()?;
    stream.start()?;

    let chunk = (SR as u64 * chunk_ms / 1000) as usize;
    let chunk_dur = Duration::from_millis(chunk_ms);
    let cadence = Duration::from_millis(cadence_ms);

    let start = Instant::now();
    let mut next_chunk_at = start;
    let mut last_poll = start;
    let mut first_partial = None;
    let mut partial_calls = Vec::new();
    let mut model_time = Duration::ZERO;

    for slice in audio.chunks(chunk) {
        // Arrive in real time, exactly as WASAPI would deliver packets.
        next_chunk_at += chunk_dur;
        let now = Instant::now();
        if next_chunk_at > now {
            std::thread::sleep(next_chunk_at - now);
        }
        stream.add_audio(slice, SR)?;

        if last_poll.elapsed() >= cadence {
            let t0 = Instant::now();
            let lines = stream.transcribe(force)?;
            let took = t0.elapsed();
            partial_calls.push(took);
            model_time += took;
            last_poll = Instant::now();
            if first_partial.is_none() && !join(&lines).is_empty() {
                first_partial = Some(start.elapsed());
            }
        }
    }

    // Key release: end input and drain immediately. No VAD wait.
    let release = Instant::now();
    stream.stop()?;
    let lines = stream.transcribe(false)?;
    let flush = release.elapsed();
    model_time += flush;

    Ok(HoldResult {
        first_partial,
        partial_calls,
        flush,
        model_time,
        audio_secs: audio.len() as f64 / SR as f64,
        text: join(&lines),
    })
}

fn main() {
    let args = parse_args();
    println!("model      {} (arch {})", args.model, args.arch);
    println!("wav        {}", args.wav);
    println!(
        "feed       {} ms chunks, partial cadence {} ms, force_update={}",
        args.chunk_ms, args.cadence_ms, args.force
    );

    let audio = load_audio(&args.wav);
    println!(
        "audio      {:.1} s at {} Hz mono",
        audio.len() as f64 / SR as f64,
        SR
    );

    let t_load = Instant::now();
    let opt_refs: Vec<(&str, &str)> = args
        .opts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if !opt_refs.is_empty() {
        println!("options    {:?}", opt_refs);
    }
    let t = Transcriber::load(&args.model, args.arch, &opt_refs).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    println!("transcriber loaded in {:?}", t_load.elapsed());
    if !args.keyterms.is_empty() {
        t.set_keyterms(&args.keyterms).expect("set_keyterms");
        println!("keyterms   {}", args.keyterms);
    }

    // Warm-up so the first measured hold is not paying for lazy allocation.
    let mut warm = vec![0.0f32; SR as usize / 2];
    let _ = t.transcribe_once(&mut warm, SR);

    // ---- Part 1: push-to-talk holds -------------------------------------
    let hold_len = ((args.hold_secs * SR as f64) as usize).max(1);
    let n = args.utterances.min(audio.len() / hold_len).max(1);
    println!("\n--- {n} holds of {:.1} s (real-time fed) ---", args.hold_secs);

    let mut flush = Samples::new();
    let mut partial = Samples::new();
    let mut first = Samples::new();
    let mut rtf_samples: Vec<f64> = Vec::new();

    for pass in 0..args.repeat.max(1) {
    for i in 0..n {
        let slice = &audio[i * hold_len..((i + 1) * hold_len).min(audio.len())];
        match run_hold(&t, slice, args.chunk_ms, args.cadence_ms, args.force) {
            Ok(r) => {
                flush.push(r.flush);
                for p in &r.partial_calls {
                    partial.push(*p);
                }
                if let Some(f) = r.first_partial {
                    first.push(f);
                }
                rtf_samples.push(r.model_time.as_secs_f64() / r.audio_secs);
                if args.trace && pass == 0 {
                    let each: Vec<String> = r
                        .partial_calls
                        .iter()
                        .map(|d| format!("{:.0}", d.as_secs_f64() * 1000.0))
                        .collect();
                    println!("        partial ms: [{}]", each.join(" "));
                }
                println!(
                    "  hold {:>2}: flush {:>6.1} ms  partials {:<3} rtf {:.3}  \"{}\"",
                    i + 1,
                    r.flush.as_secs_f64() * 1000.0,
                    r.partial_calls.len(),
                    r.model_time.as_secs_f64() / r.audio_secs,
                    truncate(&r.text, 64)
                );
            }
            Err(e) => eprintln!("  hold {}: ERROR {e}", i + 1),
        }
    }
    }

    println!("\n{}", flush.report("key release -> final transcript"));
    println!("{}", partial.report("one partial transcribe_stream"));
    println!("{}", first.report("speech start -> first partial"));
    rtf_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !rtf_samples.is_empty() {
        println!(
            "{:<34} n={:<5} p50={:>7.3} p95={:>7.3} max={:>7.3}",
            "RTF (model time / audio time)",
            rtf_samples.len(),
            rtf_samples[rtf_samples.len() / 2],
            rtf_samples[(rtf_samples.len() * 95 / 100).min(rtf_samples.len() - 1)],
            rtf_samples[rtf_samples.len() - 1]
        );
    }

    // ---- Part 2: short fragments ----------------------------------------
    if args.skip_fragments {
        return;
    }
    println!("\n--- short holds (does the internal VAD swallow them?) ---");
    let mut frag_flush = Samples::new();
    for secs in [0.4f64, 0.6, 0.8, 1.0, 1.5, 2.0] {
        let len = (secs * SR as f64) as usize;
        // Several offsets, so one silent patch does not decide the answer.
        for (k, off) in [SR as usize * 2, SR as usize * 11, SR as usize * 23]
            .into_iter()
            .enumerate()
        {
            if off + len > audio.len() {
                continue;
            }
            let slice = &audio[off..off + len];
            match run_hold(&t, slice, args.chunk_ms, args.cadence_ms, args.force) {
                Ok(r) => {
                    frag_flush.push(r.flush);
                    let shown = if r.text.is_empty() {
                        "<EMPTY>".to_string()
                    } else {
                        format!("\"{}\"", truncate(&r.text, 56))
                    };
                    println!(
                        "  {:.1}s #{}: flush {:>6.1} ms  {}",
                        secs,
                        k + 1,
                        r.flush.as_secs_f64() * 1000.0,
                        shown
                    );
                }
                Err(e) => eprintln!("  {secs:.1}s #{}: ERROR {e}", k + 1),
            }
        }
    }
    println!("\n{}", frag_flush.report("short-hold flush"));
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}...")
    }
}

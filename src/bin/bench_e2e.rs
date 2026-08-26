//! End-to-end latency harness against the real ASR worker.
//!
//! Unlike `bench`, this runs the shipping architecture: a feeder thread
//! standing in for WASAPI, and the ASR worker on its own thread. The hold
//! length is swept in small increments so the release lands at every phase of
//! the partial cadence, which is the case the single-threaded harness could
//! never produce and the one that sets p95 and p99.

use flow::asr::Transcriber;
use flow::asr_service::{AsrService, Config, Event};
use flow::ffi;
use flow::stats::Samples;
use flow::wav::{resample_into, Wav};
use std::time::{Duration, Instant};

const SR: i32 = 16_000;

struct Args {
    model: String,
    arch: u32,
    wav: String,
    chunk_ms: u64,
    cadence_ms: u64,
    force: bool,
    holds: usize,
    base_secs: f64,
    phase_step_ms: u64,
    opts: Vec<(String, String)>,
    verbose: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        model: "models/small-streaming-en".into(),
        arch: ffi::ARCH_SMALL_STREAMING,
        wav: "bench/corpus/two_cities_16k.wav".into(),
        chunk_ms: 20,
        cadence_ms: 100,
        force: true,
        holds: 24,
        base_secs: 4.0,
        phase_step_ms: 37,
        opts: Vec::new(),
        verbose: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--model" => { a.model = next(i); i += 1; }
            "--arch" => { a.arch = next(i).parse().unwrap_or(a.arch); i += 1; }
            "--wav" => { a.wav = next(i); i += 1; }
            "--chunk-ms" => { a.chunk_ms = next(i).parse().unwrap_or(a.chunk_ms); i += 1; }
            "--cadence-ms" => { a.cadence_ms = next(i).parse().unwrap_or(a.cadence_ms); i += 1; }
            "--holds" => { a.holds = next(i).parse().unwrap_or(a.holds); i += 1; }
            "--base-secs" => { a.base_secs = next(i).parse().unwrap_or(a.base_secs); i += 1; }
            "--phase-step-ms" => { a.phase_step_ms = next(i).parse().unwrap_or(a.phase_step_ms); i += 1; }
            "--no-force" => a.force = false,
            "--verbose" => a.verbose = true,
            "--opt" => {
                let kv = next(i);
                if let Some((k, v)) = kv.split_once('=') {
                    a.opts.push((k.to_string(), v.to_string()));
                }
                i += 1;
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }
    a
}

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

fn main() {
    let args = parse_args();
    let audio = load_audio(&args.wav);

    println!("model      {} (arch {})", args.model, args.arch);
    println!(
        "feed       {} ms chunks, cadence {} ms, force={}",
        args.chunk_ms, args.cadence_ms, args.force
    );
    println!(
        "holds      {} starting at {:.2} s, phase step {} ms",
        args.holds, args.base_secs, args.phase_step_ms
    );
    println!("audio      {:.1} s\n", audio.len() as f64 / SR as f64);

    let opt_refs: Vec<(&str, &str)> = args.opts.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let t_load = Instant::now();
    let transcriber = Transcriber::load(&args.model, args.arch, &opt_refs).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let load_ms = t_load.elapsed();

    // Warm the graph once, exactly as the app does at startup.
    let mut warm = vec![0.0f32; SR as usize / 2];
    let _ = transcriber.transcribe_once(&mut warm, SR);
    println!("transcriber loaded and warmed in {load_ms:?}");

    let service = AsrService::spawn(
        transcriber,
        Config {
            partial_cadence: Duration::from_millis(args.cadence_ms),
            force_partials: args.force,
            release_tail: Duration::from_millis(15),
        },
    );

    let mut total = Samples::new();
    let mut inflight = Samples::new();
    let mut drain = Samples::new();
    let mut first_partial = Samples::new();
    let mut empties = 0usize;

    let chunk = (SR as u64 * args.chunk_ms / 1000) as usize;
    let chunk_dur = Duration::from_millis(args.chunk_ms);
    let mut cursor = 0usize;

    for h in 0..args.holds {
        // Sweep the hold length so release lands at every cadence phase.
        let secs = args.base_secs + (h as f64 * args.phase_step_ms as f64 / 1000.0);
        let len = (secs * SR as f64) as usize;
        if cursor + len > audio.len() {
            cursor = 0;
        }
        let slice = &audio[cursor..cursor + len];
        cursor += len;

        service.begin();
        let started = Instant::now();
        let mut saw_partial: Option<Duration> = None;
        let mut next_at = started;

        for c in slice.chunks(chunk) {
            next_at += chunk_dur;
            let now = Instant::now();
            if next_at > now {
                std::thread::sleep(next_at - now);
            }
            service.push_audio(c);
            // Consume partial events as they arrive, like the overlay would.
            while let Ok(ev) = service.events.try_recv() {
                if let Event::Partial { .. } = ev {
                    if saw_partial.is_none() {
                        saw_partial = Some(started.elapsed());
                    }
                }
            }
        }

        // Key up.
        let release = Instant::now();
        service.release();

        let mut got_final = false;
        while !got_final {
            match service.events.recv_timeout(Duration::from_secs(20)) {
                Ok(Event::Final { text, wait_for_inflight, drain: d, .. }) => {
                    let e2e = release.elapsed();
                    total.push(e2e);
                    inflight.push(wait_for_inflight);
                    drain.push(d);
                    if text.trim().is_empty() {
                        empties += 1;
                    }
                    if args.verbose {
                        println!(
                            "  hold {:>2} ({:.2}s): total {:>6.1} ms = inflight {:>6.1} + drain {:>6.1}  \"{}\"",
                            h + 1,
                            secs,
                            e2e.as_secs_f64() * 1000.0,
                            wait_for_inflight.as_secs_f64() * 1000.0,
                            d.as_secs_f64() * 1000.0,
                            truncate(text.trim(), 48)
                        );
                    }
                    got_final = true;
                }
                Ok(Event::Partial { .. }) => {
                    if saw_partial.is_none() {
                        saw_partial = Some(started.elapsed());
                    }
                }
                Ok(Event::Error(e)) => {
                    eprintln!("  hold {}: ERROR {e}", h + 1);
                    got_final = true;
                }
                Err(e) => {
                    eprintln!("  hold {}: timed out waiting for final: {e}", h + 1);
                    got_final = true;
                }
            }
        }

        if let Some(fp) = saw_partial {
            first_partial.push(fp);
        }
    }

    println!("\n{}", total.report("KEY UP -> FINAL TRANSCRIPT"));
    println!("{}", inflight.report("  of which: in-flight partial"));
    println!("{}", drain.report("  of which: stop + drain"));
    println!("{}", first_partial.report("speech start -> first partial"));
    if empties > 0 {
        println!("\nWARNING: {empties} of {} holds returned empty text", args.holds);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "..."
    }
}

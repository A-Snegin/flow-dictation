//! Flow: hold a key, talk, let go, the text is there.
//!
//! One resident process. The Win32 message thread owns the hotkey hook, the
//! tray icon and the overlay and is asleep the rest of the time. Audio capture
//! and inference each own a thread. Nothing on the path from key-up to text
//! touches the disk, the network, or a browser engine.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flow::asr::Transcriber;
use flow::asr_service::{AsrService, AudioSink, Config, Event, Waker};
use flow::audio::Capture;
use flow::format::Formatter;
use flow::hotkey::{self, HotkeyEvent};
use flow::inject;
use flow::overlay::{Overlay, OverlayState};
use flow::settings::{Insertion, Settings};
use flow::target_app;
use flow::trace::{self, Utterance};
use flow::tray::{Tray, TrayCommand};

use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage,
    MWMO_INPUTAVAILABLE, MSG, PM_REMOVE, QS_ALLINPUT,
};

/// Posted to the message thread whenever there is something to service, so the
/// loop can block rather than poll. Polling cost 1.4% of a core at idle and put
/// up to 2 ms between the final transcript and the paste.
const WM_FLOW_WAKE: u32 = 0x0400 + 2;

fn main() {
    // Before any window exists: otherwise the overlay is drawn at 96 dpi and
    // scaled up by the compositor, which looks soft on a scaled display.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--mic-test") => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
            return mic_test(secs);
        }
        Some("--dictate") => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
            return dictate_once(secs);
        }
        Some("--which-app") => {
            let (settings, _) = Settings::load();
            let exe = target_app::foreground_executable();
            let terminal = target_app::is_terminal(&exe, &settings.insertion.terminal_apps);
            println!("foreground application: {}", if exe.is_empty() { "unknown".into() } else { exe });
            println!("treated as a terminal:  {terminal}");
            println!(
                "insertion would use:    {}",
                if terminal { &settings.insertion.terminal_mode } else { &settings.insertion.mode }
            );
            if terminal {
                println!("line breaks replaced with {:?} so nothing can be executed", settings.insertion.terminal_newline_replacement);
            }
            return;
        }
        Some("--help") | Some("-h") => {
            println!(
                "flow-core                run the tray app\n\
                 flow-core --mic-test N   capture N seconds and report the audio path\n\
                 flow-core --dictate N    capture N seconds, transcribe, print (no insertion)
                 flow-core --which-app    report the focused app and how text would be inserted"
            );
            return;
        }
        _ => {}
    }

    let (settings, problem) = Settings::load();
    if let Some(p) = problem {
        eprintln!("settings: {p}");
    }

    // Must be set before the first ONNX session exists, which means before the
    // transcriber loads. The library reads it when it builds session options.
    if settings.model.single_thread {
        std::env::set_var("MOONSHINE_ORT_SINGLE_THREAD", "1");
    }

    let (model_dir, arch) = settings.resolve_model();
    if !model_dir.join("streaming_config.json").exists() {
        eprintln!(
            "No model at {}.\nRun scripts\\fetch-model.ps1 to download it.",
            model_dir.display()
        );
        std::process::exit(1);
    }

    let hotkey_vk = hotkey::vk::from_name(&settings.hotkey.key).unwrap_or_else(|| {
        eprintln!(
            "unknown hotkey \"{}\", falling back to Right Ctrl",
            settings.hotkey.key
        );
        hotkey::vk::RCONTROL
    });
    let insertion = settings.insertion.clone();

    // ---- Warm everything before the user can possibly press the key -------
    let boot = Instant::now();

    // Opening the capture device costs about 320 ms and loading the model about
    // 700 ms. Neither depends on the other, so they overlap. The sink exists
    // before either, which is what lets the device start filling it.
    let sink = AudioSink::new();
    let first_packet = Arc::new(AtomicI64::new(0));
    // Peak level per packet, in thousandths. The overlay reads it to show that
    // the microphone is actually hearing something, which is the whole point
    // of the pill: knowing the words are not being wasted.
    let level = Arc::new(AtomicU32::new(0));
    let capture_thread = {
        let sink = sink.clone();
        let first = Arc::clone(&first_packet);
        let level = Arc::clone(&level);
        std::thread::spawn(move || {
            Capture::open(move |samples| {
                if first.load(Ordering::Relaxed) == 0 {
                    first.store(trace::now(), Ordering::Relaxed);
                }
                let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                // fetch_max, not store: packets arrive every 10 ms and the
                // overlay looks every 40 ms, so a plain store threw away three
                // packets in four and could miss the loudest one entirely.
                level.fetch_max((peak * 1000.0) as u32, Ordering::Relaxed);
                sink.push(samples);
            })
        })
    };

    println!("Flow: loading {} ...", model_dir.display());
    let keyterm_boost = settings.model.keyterm_boost.to_string();
    let transcriber = match Transcriber::load(
        &model_dir.to_string_lossy(),
        arch,
        &[("keyterm_boost", keyterm_boost.as_str())],
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not load the model: {e}");
            std::process::exit(1);
        }
    };

    let mut formatter = Formatter::default();
    formatter.capitalise_sentences = settings.formatting.capitalise_sentences;
    formatter.spoken_punctuation = settings.formatting.spoken_punctuation;
    formatter.trailing_space = settings.formatting.trailing_space;
    if !settings.dictionary.is_empty() {
        formatter.set_dictionary(&settings.dictionary);
        if let Err(e) = transcriber.set_keyterms(&formatter.keyterms()) {
            eprintln!("keyterms rejected: {e}");
        }
    }
    let formatter = Arc::new(Mutex::new(formatter));

    // One warm-up inference so the first real dictation pays nothing extra.
    let mut warm = vec![0.0f32; 8_000];
    let _ = transcriber.transcribe_once(&mut warm, 16_000);

    let asr = Arc::new(AsrService::spawn_with(
        transcriber,
        Config {
            partial_cadence: Duration::from_millis(settings.model.partial_cadence_ms),
            force_partials: settings.model.force_partials,
            release_tail: Duration::from_millis(settings.model.release_tail_ms),
        },
        sink,
        Waker::for_current_thread(WM_FLOW_WAKE),
    ));

    // ---- Hotkey hook and tray, both on this thread -----------------------
    let (hk_tx, hk_rx) = channel::<HotkeyEvent>();
    let hook = match hotkey::install(hotkey_vk, hk_tx, WM_FLOW_WAKE) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("could not install the hotkey hook: {e}");
            std::process::exit(1);
        }
    };
    let mut overlay = Overlay::create(&hotkey_label(&settings.hotkey.key)).unwrap_or_else(|e| {
        eprintln!("overlay unavailable, continuing without it: {e}");
        Overlay::disabled()
    });
    overlay.attach_level(Arc::clone(&level));
    let mut tray = Tray::create().unwrap_or_else(|e| {
        eprintln!("tray unavailable, continuing without it: {e}");
        Tray::disabled()
    });

    println!(
        "Ready in {:?}. Hold {} to dictate.",
        boot.elapsed(),
        settings.hotkey.key
    );
    println!(
        "Insertion: {} normally, {} in terminals.",
        insertion.mode, insertion.terminal_mode
    );
    println!(
        "Model: {} profile, {}.",
        settings.model.profile,
        if settings.model.single_thread { "single threaded" } else { "multi threaded" }
    );
    println!("Settings: {}", Settings::path().display());

    // The device was opened above, in parallel with the model, and is only
    // started when the key goes down.
    let capture = match capture_thread.join() {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => {
            eprintln!("microphone unavailable: {e}");
            None
        }
        Err(_) => {
            eprintln!("microphone thread panicked");
            None
        }
    };

    let mut app = App {
        asr,
        formatter,
        insertion,
        capture,
        utterance: None,
        enabled: true,
        first_packet,
        level,
        overlay,
    };

    // ---- Message pump ----------------------------------------------------
    // The hook needs this thread pumping messages or it never fires. The loop
    // blocks until something arrives: a Windows message, or a wake posted by
    // the hook or the ASR worker. The 250 ms timeout is only a safety net.
    let mut msg = MSG::default();
    loop {
        // While the pill is up, wake often enough to animate the meter.
        // Otherwise sleep until something actually happens.
        let wait = app
            .overlay
            .tick()
            .map(|d| d.as_millis() as u32)
            .unwrap_or(u32::MAX);

        unsafe {
            MsgWaitForMultipleObjectsEx(None, wait, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        match tray.poll() {
            Some(TrayCommand::Quit) => break,
            Some(TrayCommand::Toggle) => {
                app.enabled = !app.enabled;
                if !app.enabled {
                    app.cancel();
                }
                tray.set_paused(!app.enabled);
                println!("Dictation {}", if app.enabled { "enabled" } else { "paused" });
            }
            Some(TrayCommand::LatencyReport) => println!("\n{}\n", trace::report()),
            Some(TrayCommand::OpenSettings) => {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &Settings::path().to_string_lossy()])
                    .spawn();
            }
            Some(TrayCommand::ToggleAutostart) => match flow::autostart::toggle() {
                Ok(on) => println!(
                    "Start at login is now {}",
                    if on { "on" } else { "off" }
                ),
                Err(e) => eprintln!("could not change autostart: {e}"),
            },
            Some(TrayCommand::ReloadDictionary) => {
                // Dictionary, formatting and insertion reload live. The hotkey
                // and the model do not: one is hooked, the other is a warmed
                // ONNX session, and silently rebuilding either mid-session
                // would cost more than restarting.
                let (fresh, problem) = Settings::load();
                if let Some(p) = problem {
                    eprintln!("settings: {p}");
                } else {
                    if let Ok(mut f) = app.formatter.lock() {
                        f.capitalise_sentences = fresh.formatting.capitalise_sentences;
                        f.spoken_punctuation = fresh.formatting.spoken_punctuation;
                        f.trailing_space = fresh.formatting.trailing_space;
                        f.set_dictionary(&fresh.dictionary);
                        app.asr.set_keyterms(&f.keyterms());
                    }
                    app.insertion = fresh.insertion.clone();
                    println!(
                        "Reloaded: {} dictionary entries, insertion {} / {} in terminals.",
                        fresh.dictionary.len(),
                        fresh.insertion.mode,
                        fresh.insertion.terminal_mode
                    );
                }
            }
            None => {}
        }

        app.pump_hotkey(&hk_rx);
        app.pump_asr();
    }

    hotkey::uninstall(hook);
}

struct App {
    asr: Arc<AsrService>,
    formatter: Arc<Mutex<Formatter>>,
    insertion: Insertion,
    /// Open for the life of the process, started only while the key is held.
    capture: Option<Capture>,
    utterance: Option<Utterance>,
    enabled: bool,
    first_packet: Arc<AtomicI64>,
    /// Peak microphone level in thousandths, written by the capture thread.
    level: Arc<AtomicU32>,
    overlay: Overlay,
}

impl App {
    fn pump_hotkey(&mut self, rx: &Receiver<HotkeyEvent>) {
        loop {
            match rx.try_recv() {
                Ok(HotkeyEvent::Down) => self.start(),
                Ok(HotkeyEvent::Up) => self.stop(),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn start(&mut self) {
        if !self.enabled || self.utterance.is_some() {
            return;
        }
        let mut u = Utterance::begin();

        // Microphone first, screen second. Painting the overlay means an
        // UpdateLayeredWindow and a ShowWindow on a topmost layered window,
        // and doing that before arming put a fixed delay between the key going
        // down and the first sample arriving. Every millisecond spent here is
        // speech the user has already said and the recogniser will never see.
        self.first_packet.store(0, Ordering::SeqCst);
        self.level.store(0, Ordering::Relaxed);
        self.asr.begin();

        match self.capture.as_ref() {
            Some(c) => c.arm(),
            None => {
                let msg = "no microphone";
                eprintln!("{msg}");
                self.overlay.set(OverlayState::Error, msg);
                return;
            }
        }
        u.t0b_armed = trace::now();
        if let Some(c) = self.capture.as_ref() {
            u.reset_us = c.stats.last_reset_us.load(Ordering::Relaxed);
            u.start_us = c.stats.last_start_us.load(Ordering::Relaxed);
        }
        self.utterance = Some(u);

        self.overlay.set(OverlayState::Listening, "");
    }

    fn stop(&mut self) {
        let Some(u) = self.utterance.as_mut() else {
            return;
        };
        u.t7_hotkey_up = trace::now();
        u.t1_first_packet = self.first_packet.load(Ordering::SeqCst);
        self.asr.release();
        self.overlay.set(OverlayState::Finalising, "");
        // Stopping is a flag and an event: it does not join the capture thread,
        // so nothing blocks between key-up and text. The worker collects the
        // release tail before ending the stream, so the last packet still lands.
        if let Some(c) = self.capture.as_ref() {
            c.disarm();
        }
    }

    fn cancel(&mut self) {
        if let Some(c) = self.capture.as_ref() {
            c.disarm();
        }
        self.asr.audio_sink().clear();
        self.utterance = None;
        self.overlay.set(OverlayState::Hidden, "");
    }

    fn pump_asr(&mut self) {
        loop {
            let ev = match self.asr.events.try_recv() {
                Ok(e) => e,
                Err(_) => return,
            };
            match ev {
                Event::Partial { text } => {
                    if let Some(u) = self.utterance.as_mut() {
                        if u.t6_first_partial == 0 {
                            u.t6_first_partial = trace::now();
                        }
                    }
                    self.overlay.set(OverlayState::Listening, &text);
                }
                Event::Final { text, .. } => {
                    let Some(mut u) = self.utterance.take() else {
                        continue;
                    };
                    u.t9_final_ready = trace::now();

                    let formatted = match self.formatter.lock() {
                        Ok(f) => f.format(&text),
                        Err(_) => text.clone(),
                    };
                    u.t10_formatted = trace::now();
                    u.chars = formatted.chars().count();

                    if formatted.trim().is_empty() {
                        u.t12_inserted = trace::now();
                        self.overlay.set(OverlayState::Error, "nothing heard");
                    } else {
                        // Decide per target: a shell prompt is not a document.
                        let exe = target_app::foreground_executable();
                        let terminal =
                            target_app::is_terminal(&exe, &self.insertion.terminal_apps);
                        let (mode_name, formatted) = if terminal {
                            (
                                self.insertion.terminal_mode.as_str(),
                                target_app::strip_newlines(
                                    &formatted,
                                    &self.insertion.terminal_newline_replacement,
                                ),
                            )
                        } else {
                            (self.insertion.mode.as_str(), formatted)
                        };
                        let mode = inject::Mode::from_name(mode_name);
                        u.chars = formatted.chars().count();
                        if let Err(e) = inject::insert(&formatted, mode) {
                            eprintln!("insertion failed: {e}");
                            self.overlay.set(OverlayState::Error, &e);
                        }
                        u.t12_inserted = trace::now();
                        // Painted after the text is already in the target
                        // application, so it costs the user nothing.
                        self.overlay.set(OverlayState::Done, formatted.trim());
                        println!(
                            "{:>6.0} ms  {}",
                            u.user_perceived_ms(),
                            formatted.trim()
                        );
                    }
                    trace::record(&u);
                }
                Event::Error(e) => {
                    eprintln!("asr: {e}");
                    self.overlay.set(OverlayState::Error, &e);
                }
            }
        }
    }
}

/// What to print on the pill's key cap. The user is holding a physical key;
/// the label should look like the key, not like a config value.
fn hotkey_label(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "rightctrl" | "rctrl" | "right_control" | "leftctrl" | "lctrl" => "Ctrl",
        "rightalt" | "ralt" => "Alt",
        "rightshift" | "rshift" => "Shift",
        "capslock" => "Caps",
        "f13" => "F13",
        other => other,
    }
    .to_string()
}

/// Proves the capture path without needing anyone to speak: packet count,
/// delivered sample rate and peak level. A silent result with packets flowing
/// means the device works and the room is quiet; zero packets means it does not.
fn mic_test(secs: u64) {
    use std::sync::atomic::AtomicU32;

    let samples = Arc::new(AtomicU32::new(0));
    let peak_milli = Arc::new(AtomicU32::new(0));
    let first = Arc::new(AtomicI64::new(0));

    let t_open = trace::now();
    let capture = {
        let samples = Arc::clone(&samples);
        let peak_milli = Arc::clone(&peak_milli);
        let first = Arc::clone(&first);
        Capture::open(move |buf| {
            if first.load(Ordering::Relaxed) == 0 {
                first.store(trace::now(), Ordering::Relaxed);
            }
            samples.fetch_add(buf.len() as u32, Ordering::Relaxed);
            let p = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let p = (p * 1000.0) as u32;
            peak_milli.fetch_max(p, Ordering::Relaxed);
        })
    };

    let capture = match capture {
        Ok(c) => c,
        Err(e) => {
            eprintln!("microphone unavailable: {e}");
            std::process::exit(1);
        }
    };

    let opened = trace::now();
    println!(
        "device opened in {:.1} ms (startup cost, paid once)",
        trace::ms_between(t_open, opened)
    );

    // The number that matters: key-down to a stream that is delivering.
    first.store(0, Ordering::Relaxed);
    let t0 = trace::now();
    capture.arm();
    println!(
        "arm() returned in {:.2} ms, listening for {secs} s ...",
        trace::ms_between(t0, trace::now())
    );
    std::thread::sleep(Duration::from_secs(secs));

    let n = samples.load(Ordering::Relaxed);
    let peak = peak_milli.load(Ordering::Relaxed) as f32 / 1000.0;
    let stats = &capture.stats;
    println!(
        "first packet {:.1} ms after arm (reset {} us, start {} us)",
        trace::ms_between(t0, first.load(Ordering::Relaxed)),
        stats.last_reset_us.load(Ordering::Relaxed),
        stats.last_start_us.load(Ordering::Relaxed)
    );
    println!(
        "{} samples in {secs} s = {:.0} Hz after conversion (target {})",
        n,
        n as f64 / secs as f64,
        flow::audio::TARGET_SR
    );
    println!(
        "packets {}, glitches {}, device errors {}",
        stats.packets.load(Ordering::Relaxed),
        stats.glitches.load(Ordering::Relaxed),
        stats.device_errors.load(Ordering::Relaxed)
    );
    println!("peak level {peak:.3} ({})", if peak > 0.005 { "audio present" } else { "silence" });
    drop(capture);
}

/// One dictation from the real microphone, on a timer instead of a hotkey.
/// Prints the transcript rather than inserting it, so it is safe to run from a
/// terminal.
fn dictate_once(secs: u64) {
    let (settings, _) = Settings::load();
    // Must be set before the first ONNX session exists, which means before the
    // transcriber loads. The library reads it when it builds session options.
    if settings.model.single_thread {
        std::env::set_var("MOONSHINE_ORT_SINGLE_THREAD", "1");
    }

    let (model_dir, arch) = settings.resolve_model();
    println!("loading {} ...", model_dir.display());
    let transcriber = match Transcriber::load(&model_dir.to_string_lossy(), arch, &[]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut warm = vec![0.0f32; 8_000];
    let _ = transcriber.transcribe_once(&mut warm, 16_000);

    let asr = AsrService::spawn(
        transcriber,
        Config {
            partial_cadence: Duration::from_millis(settings.model.partial_cadence_ms),
            force_partials: settings.model.force_partials,
            release_tail: Duration::from_millis(settings.model.release_tail_ms),
        },
    );

    let sink = asr.audio_sink();
    let capture = match Capture::open(move |buf| sink.push(buf)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("microphone unavailable: {e}");
            std::process::exit(1);
        }
    };
    asr.begin();
    capture.arm();
    println!("SPEAK NOW for {secs} seconds ...");

    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        while let Ok(ev) = asr.events.try_recv() {
            if let Event::Partial { text } = ev {
                println!("  partial: {text}");
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let release = Instant::now();
    asr.release();
    capture.disarm();

    loop {
        match asr.events.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Final { text, wait_for_inflight, drain, .. }) => {
                let formatted = Formatter::default().format(&text);
                println!(
                    "\nkey up -> transcript: {:.0} ms (in-flight {:.0}, drain {:.0})",
                    release.elapsed().as_secs_f64() * 1000.0,
                    wait_for_inflight.as_secs_f64() * 1000.0,
                    drain.as_secs_f64() * 1000.0
                );
                println!("raw:       {text}");
                println!("formatted: {formatted}");
                return;
            }
            Ok(Event::Partial { text }) => println!("  partial: {text}"),
            Ok(Event::Error(e)) => eprintln!("asr: {e}"),
            Err(e) => {
                eprintln!("no final transcript: {e}");
                return;
            }
        }
    }
}

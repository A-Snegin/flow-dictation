//! Checks the clipboard half of the insertion path without touching the
//! keyboard.
//!
//! An earlier version of this test opened Notepad, called `SetForegroundWindow`
//! and sent Ctrl+V. Windows refuses focus changes requested by a background
//! console process, so it typed into whatever the user happened to have in
//! front. Synthetic keystrokes go wherever focus is, which makes them
//! untestable unattended and dangerous to try. The keystroke itself is one
//! `SendInput` call; what actually carries risk is the clipboard being
//! clobbered, and that is what this checks.

use std::time::Instant;

use flow::inject;

fn main() {
    let mut failures = 0;

    let original = "clipboard contents the user cared about";
    check(
        "write then read round trip",
        &mut failures,
        inject::set_clipboard_text(original).is_ok()
            && inject::clipboard_text().as_deref() == Some(original),
    );

    let unicode = "Naïve café — 42 £ ✓ 日本語";
    check(
        "unicode survives the round trip",
        &mut failures,
        inject::set_clipboard_text(unicode).is_ok()
            && inject::clipboard_text().as_deref() == Some(unicode),
    );

    let long: String = "Lift-Off ".repeat(4000);
    check(
        "a long dictation fits",
        &mut failures,
        inject::set_clipboard_text(&long).is_ok()
            && inject::clipboard_text().as_deref() == Some(long.as_str()),
    );

    // What a dictation does to the clipboard: save, replace, restore.
    let _ = inject::set_clipboard_text(original);
    let saved = inject::clipboard_text();
    let _ = inject::set_clipboard_text("the dictated text");
    let _ = inject::set_clipboard_text(saved.as_deref().unwrap_or(""));
    check(
        "save and restore returns the original",
        &mut failures,
        inject::clipboard_text().as_deref() == Some(original),
    );

    // Cost of the clipboard work, which is the part of insertion that scales.
    let t0 = Instant::now();
    for _ in 0..20 {
        let _ = inject::set_clipboard_text("a typical dictated sentence of about this length. ");
    }
    println!(
        "\nclipboard write: {:.2} ms each over 20 runs",
        t0.elapsed().as_secs_f64() * 1000.0 / 20.0
    );

    let _ = inject::set_clipboard_text("");
    if failures == 0 {
        println!("\nAll clipboard checks passed.");
        println!("The Ctrl+V keystroke itself is verified by dictating into a real application.");
    } else {
        eprintln!("\n{failures} check(s) failed");
        std::process::exit(1);
    }
}

fn check(name: &str, failures: &mut u32, ok: bool) {
    println!("{:<40} {}", name, if ok { "pass" } else { "FAIL" });
    if !ok {
        *failures += 1;
    }
}

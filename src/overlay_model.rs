//! Everything about the status pill that is not drawing.
//!
//! The pill exists on both platforms and has to look and behave identically on
//! each. What it is made of, how big every piece is, how the meter reacts to a
//! voice, how the trace travels, when the pill goes away: none of that is
//! Win32 or Wayland, and having two copies of it would mean the two builds
//! drifting apart one small fix at a time.
//!
//! So this file holds the model and the geometry, in logical pixels, and the
//! platform files hold only the code that puts pixels on a screen: GDI into a
//! layered window on Windows, `wl_shm` and tiny-skia into a layer-shell surface
//! on Linux.
//!
//! Sizes here are logical. Multiply by the display scale to get device pixels.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayState {
    Hidden,
    /// Key is down, audio is being captured.
    Listening,
    /// Key released, the recogniser is finishing.
    Finalising,
    /// Text went into the target application. Shown briefly, then gone.
    Done,
    Error,
}

pub const DONE_LINGER: Duration = Duration::from_millis(1000);
pub const ERROR_LINGER: Duration = Duration::from_millis(3000);
/// 25 fps: fluid enough to read as live, cheap enough not to matter, and only
/// ever running while the pill is on screen.
pub const LISTENING_TICK: Duration = Duration::from_millis(40);

/// Bars in the waveform. Thin and many, so it reads as a voice trace rather
/// than a level meter.
pub const BARS: usize = 11;

/// Fixed per-bar weights. A flat envelope looks synthetic; this gives the trace
/// the uneven shape a real waveform has, while staying identical frame to frame
/// so only the level and the travelling wave move.
pub const BAR_WEIGHT: [f32; BARS] = [
    0.42, 0.68, 0.50, 0.88, 0.60, 1.00, 0.55, 0.92, 0.48, 0.72, 0.40,
];

// ---- colours, plain RGB ---------------------------------------------------

pub const COL_BG: [u8; 3] = [0x1E, 0x1D, 0x1C];
/// Coral. The one warm thing on screen, so "it is recording" reads instantly.
pub const COL_ACCENT: [u8; 3] = [0xF0, 0x60, 0x3C];
pub const COL_DONE: [u8; 3] = [0x57, 0xC4, 0x6A];
pub const COL_ERROR: [u8; 3] = [0xE5, 0x48, 0x4D];
pub const COL_TEXT: [u8; 3] = [0xF2, 0xF2, 0xF2];
pub const COL_TEXT_LIVE: [u8; 3] = [0xD6, 0xD6, 0xD6];
pub const COL_HINT: [u8; 3] = [0x8E, 0x8E, 0x8E];
pub const COL_KEYCAP: [u8; 3] = [0x3A, 0x38, 0x36];
pub const COL_KEYCAP_TEXT: [u8; 3] = [0xD8, 0xD8, 0xD8];
pub const COL_WAVE: [u8; 3] = [0xBC, 0xBC, 0xBC];

/// How opaque the pill is. Not quite solid, so what is behind it is still
/// faintly there and the pill reads as an overlay rather than a hole.
pub const PILL_OPACITY: u8 = 244;

// ---- geometry, logical pixels ---------------------------------------------

/// Fixed. The pill never resizes: a shape that grows and shrinks as words
/// arrive is movement at the edge of vision, which is the opposite of what an
/// unobtrusive indicator should be. Long sentences scroll inside it.
pub const WIDTH: f32 = 560.0;
pub const HEIGHT: f32 = 44.0;
/// Gap from either end of the pill to the first thing drawn in it.
pub const PAD: f32 = 17.0;
pub const DOT_RADIUS: f32 = 5.0;
/// Dot to waveform.
pub const DOT_TO_WAVE: f32 = 13.0;
pub const BAR_WIDTH: f32 = 3.0;
pub const BAR_GAP: f32 = 3.0;
/// Nearly the full height of the pill. The meter is the only part that answers
/// "is it hearing me", so it is worth the room, and a low floor makes the
/// difference between silence and speech large enough to catch out of the
/// corner of an eye.
pub const BAR_MAX_HEIGHT: f32 = 26.0;
pub const BAR_MIN_HEIGHT: f32 = 3.0;
/// Waveform to the first word.
pub const WAVE_TO_TEXT: f32 = 15.0;
/// Room kept clear on the right of the words for the caret.
pub const CARET_SPACE: f32 = 10.0;
pub const CARET_OFFSET: f32 = 5.0;
pub const CARET_WIDTH: f32 = 2.0;
pub const CARET_HEIGHT: f32 = 18.0;
pub const KEYCAP_HEIGHT: f32 = 20.0;
pub const KEYCAP_PAD: f32 = 7.0;
/// Corner radius of the keycap.
pub const KEYCAP_RADIUS: f32 = 3.0;
/// "Hold" to the keycap.
pub const HOLD_TO_KEYCAP: f32 = 8.0;
/// The whole hint to whatever is left of it.
pub const HINT_TO_TEXT: f32 = 14.0;
/// Width of the band the scrolled-off words fade out across.
pub const FADE_WIDTH: f32 = 34.0;
/// Gap from the bottom of the screen's usable area.
pub const BOTTOM_MARGIN: f32 = 78.0;

/// Cell height of the words, the way `CreateFontW` means height: ascent plus
/// descent, not the em size.
pub const TEXT_FONT_HEIGHT: f32 = 16.5;
/// Cell height of "Hold" and the key name.
pub const HINT_FONT_HEIGHT: f32 = 13.5;

/// What the caller should do after [`OverlayModel::tick`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    /// The animation moved. Repaint, then look again after this long.
    Redraw(Duration),
    /// Nothing changed, but the pill is still up. Look again after this long.
    Wait(Duration),
    /// It has been up long enough. Take it down and stop ticking.
    Hide,
    /// Nothing on screen. The loop can sleep until something else wakes it.
    Idle,
}

/// The pill's state, minus anything to do with a window.
pub struct OverlayModel {
    pub state: OverlayState,
    pub text: String,
    /// Smoothed 0..1 microphone level.
    pub level: f32,
    /// Per-bar heights, each chasing the level with its own lag so the trace
    /// undulates instead of moving as one block.
    pub bars: [f32; BARS],
    /// Advances every tick and drives the travelling wave through the bars.
    pub phase: f32,
    pub shown_at: Instant,
    /// Which key the user is holding, drawn on the right as a reminder.
    pub hint_key: String,
    /// Peak level since the last frame, in thousandths, written by the capture
    /// thread. Read and cleared once per animation step: sampling it from the
    /// message loop instead meant reading it several times between frames, and
    /// every read after the first saw zero and pulled the meter down.
    pub level_source: Option<Arc<AtomicU32>>,
}

impl OverlayModel {
    pub fn new(hint_key: &str) -> OverlayModel {
        OverlayModel {
            state: OverlayState::Hidden,
            text: String::new(),
            level: 0.0,
            bars: [0.0; BARS],
            phase: 0.0,
            shown_at: Instant::now(),
            hint_key: hint_key.to_string(),
            level_source: None,
        }
    }

    /// Hands the model the counter the capture thread writes peaks into.
    pub fn attach_level(&mut self, source: Arc<AtomicU32>) {
        self.level_source = Some(source);
    }

    /// Takes the loudest sample since the last frame and clears the counter.
    pub fn sample_level(&mut self) {
        if let Some(src) = self.level_source.as_ref() {
            let peak = src.swap(0, Ordering::Relaxed) as f32 / 1000.0;
            self.set_level(peak);
        }
    }

    /// Live microphone level from a raw sample peak, 0..1.
    ///
    /// The raw peak is a poor thing to draw directly. Ordinary speech into a
    /// laptop microphone peaks around 0.05 to 0.3, so feeding it straight
    /// through left every bar pinned to its minimum height and the trace
    /// looked identical whether or not anyone was talking. A square root opens
    /// out the quiet end, where speech actually lives, and the gain puts a
    /// normal speaking voice near the top of the meter.
    ///
    /// Fast attack so a syllable registers at once, slow release so the trace
    /// does not collapse between words.
    pub fn set_level(&mut self, peak: f32) {
        let target = (peak.clamp(0.0, 1.0).sqrt() * 1.7).min(1.0);
        self.level = if target > self.level {
            self.level * 0.35 + target * 0.65
        } else {
            self.level * 0.80 + target * 0.20
        };
    }

    /// Records a new state and new words. The caller repaints afterwards.
    pub fn set(&mut self, state: OverlayState, text: &str) {
        if state != self.state {
            self.shown_at = Instant::now();
            if state == OverlayState::Listening {
                self.level = 0.0;
                self.bars = [0.0; BARS];
            }
        }
        self.state = state;
        if self.text != text {
            self.text.clear();
            self.text.push_str(text);
        }
    }

    /// One step of the state machine: advances the animation where there is
    /// one and says what the caller should do next.
    pub fn tick(&mut self) -> Tick {
        match self.state {
            OverlayState::Listening => {
                self.sample_level();
                self.advance();
                Tick::Redraw(LISTENING_TICK)
            }
            OverlayState::Finalising => {
                // Keep the trace moving as it settles, so the moment between
                // release and text does not look like a freeze.
                self.set_level(0.0);
                self.advance();
                Tick::Redraw(LISTENING_TICK)
            }
            OverlayState::Done => {
                if self.shown_at.elapsed() >= DONE_LINGER {
                    Tick::Hide
                } else {
                    Tick::Wait(Duration::from_millis(120))
                }
            }
            OverlayState::Error => {
                if self.shown_at.elapsed() >= ERROR_LINGER {
                    Tick::Hide
                } else {
                    Tick::Wait(Duration::from_millis(200))
                }
            }
            OverlayState::Hidden => Tick::Idle,
        }
    }

    /// One animation step. Each bar chases its own weight scaled by the live
    /// level and a travelling wave, lagging by an amount that grows towards the
    /// edges, which is what makes the trace look fluid rather than mechanical.
    pub fn advance(&mut self) {
        self.phase += 0.34;
        if self.phase > std::f32::consts::TAU * 64.0 {
            self.phase -= std::f32::consts::TAU * 64.0;
        }
        // A floor so the trace breathes gently rather than flatlining: a quiet
        // room should not look like a broken application.
        let energy = self.level.clamp(0.06, 1.0);
        let centre = (BARS as f32 - 1.0) / 2.0;
        for i in 0..BARS {
            let from_centre = (i as f32 - centre).abs() / centre;
            let wave = (self.phase - i as f32 * 0.55).sin() * 0.5 + 0.5;
            let target = (energy * BAR_WEIGHT[i] * (0.40 + 0.60 * wave)).clamp(0.03, 1.0);
            let lag = 0.55 - 0.12 * from_centre;
            self.bars[i] += (target - self.bars[i]) * lag;
        }
    }

    /// Whether the meter and the words are live, as opposed to a confirmation.
    pub fn is_live(&self) -> bool {
        matches!(self.state, OverlayState::Listening | OverlayState::Finalising)
    }

    /// The accent colour for the current state.
    pub fn accent(&self) -> [u8; 3] {
        match self.state {
            OverlayState::Done => COL_DONE,
            OverlayState::Error => COL_ERROR,
            _ => COL_ACCENT,
        }
    }

    /// The colour the words are drawn in.
    pub fn text_colour(&self) -> [u8; 3] {
        match self.state {
            OverlayState::Listening if !self.text.trim().is_empty() => COL_TEXT_LIVE,
            OverlayState::Error => COL_HINT,
            _ => COL_TEXT,
        }
    }

    /// The words to draw. Empty states get a caption rather than a blank pill,
    /// which would look like the app had stalled.
    pub fn body(&self) -> String {
        match (self.text.trim(), self.state) {
            ("", OverlayState::Listening) => "Listening".to_string(),
            ("", OverlayState::Finalising) => "Transcribing".to_string(),
            ("", OverlayState::Error) => "Nothing heard".to_string(),
            ("", _) => String::new(),
            (t, _) => t.to_string(),
        }
    }
}

/// The longest suffix of `text` that fits in `max_px`, on a word boundary where
/// possible. Returns the suffix and whether anything was dropped.
///
/// Following the tail is the point: while someone keeps talking, the words that
/// matter are the ones just said, not the ones at the start of the sentence.
///
/// `measure` gives the width of a string in the same units as `max_px`. It is a
/// parameter because measuring a string is the one part of this that only the
/// platform can do.
pub fn tail_that_fits<F>(text: &str, max_px: i32, mut measure: F) -> (String, bool)
where
    F: FnMut(&str) -> i32,
{
    if text.is_empty() || max_px <= 0 {
        return (String::new(), false);
    }
    if measure(text) <= max_px {
        return (text.to_string(), false);
    }
    // Walk word starts back from the end: a handful of measurements on a
    // sentence rather than one per character.
    let mut best: Option<usize> = None;
    let bytes = text.as_bytes();
    let mut i = text.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b' ' {
            let candidate = i + 1;
            if measure(&text[candidate..]) <= max_px {
                best = Some(candidate);
            } else {
                break;
            }
        }
    }
    if let Some(start) = best {
        return (text[start..].to_string(), true);
    }
    // One very long word: fall back to character granularity.
    let mut start = text.len();
    for (idx, _) in text.char_indices().rev() {
        if measure(&text[idx..]) > max_px {
            break;
        }
        start = idx;
    }
    (text[start..].to_string(), start > 0)
}

/// A colour scaled towards black, for dimming a dot or lifting a bar.
pub fn dim(colour: [u8; 3], factor: f32) -> [u8; 3] {
    let f = factor.clamp(0.0, 1.0);
    [
        (colour[0] as f32 * f) as u8,
        (colour[1] as f32 * f) as u8,
        (colour[2] as f32 * f) as u8,
    ]
}

/// Blends pixels toward the pill background across a horizontal band: fully
/// background at the left edge, untouched at the right.
///
/// When the sentence is longer than the pill, the left edge of the text fades
/// into the background rather than being chopped or prefixed with an ellipsis.
/// It reads as words scrolling past a window.
///
/// Works on any 4-bytes-per-pixel buffer; `bg` is in whatever order the first
/// three bytes of a pixel are, so BGR on both platforms as it happens.
pub fn fade_into_background(pixels: &mut [u8], w: i32, h: i32, x0: i32, x1: i32, bg: [u8; 3]) {
    let x0 = x0.max(0);
    let x1 = x1.min(w);
    if x1 <= x0 {
        return;
    }
    let span = (x1 - x0) as f32;
    for y in 0..h {
        for x in x0..x1 {
            let t = (x - x0) as f32 / span;
            let i = ((y * w + x) * 4) as usize;
            for c in 0..3 {
                let src = pixels[i + c] as f32;
                pixels[i + c] = (bg[c] as f32 * (1.0 - t) + src * t) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_reaches_the_background_and_leaves_the_rest() {
        let (w, h) = (20, 4);
        let mut px = vec![200u8; (w * h * 4) as usize];
        let bg = [10u8, 20, 30];
        fade_into_background(&mut px, w, h, 0, 10, bg);
        assert_eq!(px[0], bg[0], "left edge is fully background");
        assert_eq!(px[(15 * 4) as usize], 200, "beyond the band, untouched");
    }

    #[test]
    fn level_attacks_fast_and_releases_slowly() {
        let mut o = OverlayModel::new("Ctrl");
        o.set_level(1.0);
        assert!(o.level > 0.55, "a syllable registers immediately");
        o.set_level(0.0);
        assert!(o.level > 0.4, "and does not drop out between syllables");
        for _ in 0..60 {
            o.set_level(0.0);
        }
        assert!(o.level < 0.01, "but does settle to nothing");
    }

    #[test]
    fn bars_never_flatline_and_stay_in_range() {
        let mut o = OverlayModel::new("Ctrl");
        for _ in 0..100 {
            o.advance();
        }
        for b in o.bars {
            assert!(b > 0.0 && b <= 1.0, "bar out of range: {b}");
        }
        assert!(o.bars.iter().all(|b| *b < 0.35), "silence should stay low");
    }

    /// The bug this guards against: the meter looked identical whether or not
    /// anyone was speaking, because a raw microphone peak of 0.1 mapped to a
    /// bar height below the minimum and every bar sat pinned at its floor.
    #[test]
    fn ordinary_speech_lifts_the_bars_well_clear_of_silence() {
        let quiet = settled_peak(0.0);
        // A laptop microphone at a normal speaking distance.
        let speech = settled_peak(0.10);
        let loud = settled_peak(0.30);

        assert!(quiet < 0.15, "silence should sit low, got {quiet}");
        assert!(
            speech > quiet * 3.0,
            "speech at 0.10 must clearly beat silence: {speech} against {quiet}"
        );
        assert!(
            speech > 0.4,
            "speech at 0.10 must use a real part of the meter, got {speech}"
        );
        assert!(loud > speech, "louder must read taller: {loud} against {speech}");
    }

    /// Drives the animation to a steady state at a given microphone peak and
    /// returns the tallest bar, which is what the eye reads.
    fn settled_peak(peak: f32) -> f32 {
        let mut o = OverlayModel::new("Ctrl");
        for _ in 0..60 {
            o.set_level(peak);
            o.advance();
        }
        let mut tallest: f32 = 0.0;
        // Over a full cycle of the travelling wave, not one arbitrary frame.
        for _ in 0..40 {
            o.set_level(peak);
            o.advance();
            tallest = tallest.max(o.bars.iter().cloned().fold(0.0f32, f32::max));
        }
        tallest
    }

    #[test]
    fn bars_respond_to_level() {
        let mut loud = OverlayModel::new("Ctrl");
        for _ in 0..40 {
            loud.set_level(1.0);
            loud.advance();
        }
        let mut quiet = OverlayModel::new("Ctrl");
        for _ in 0..40 {
            quiet.advance();
        }
        let loudest = loud.bars.iter().cloned().fold(0.0f32, f32::max);
        let quietest = quiet.bars.iter().cloned().fold(0.0f32, f32::max);
        assert!(loudest > quietest * 2.0, "loud must read taller");
    }

    /// One character is worth one unit, which is enough to check the walk.
    fn by_char(s: &str) -> i32 {
        s.chars().count() as i32
    }

    #[test]
    fn short_text_is_left_alone() {
        let (shown, clipped) = tail_that_fits("hello there", 40, by_char);
        assert_eq!(shown, "hello there");
        assert!(!clipped);
    }

    #[test]
    fn long_text_keeps_the_end_on_a_word_boundary() {
        let (shown, clipped) = tail_that_fits("one two three four five", 12, by_char);
        assert!(clipped, "something had to go");
        assert!(shown.ends_with("five"), "the newest words survive: {shown}");
        assert!(by_char(&shown) <= 12, "and it fits: {shown}");
        assert!(!shown.starts_with(' '), "no leading space: {shown:?}");
    }

    #[test]
    fn one_long_word_falls_back_to_characters() {
        let (shown, clipped) = tail_that_fits("abcdefghijklmnop", 5, by_char);
        assert!(clipped);
        assert_eq!(shown, "lmnop");
    }

    #[test]
    fn no_room_means_no_text() {
        let (shown, clipped) = tail_that_fits("anything", 0, by_char);
        assert!(shown.is_empty());
        assert!(!clipped);
    }

    #[test]
    fn done_hides_after_its_linger_and_listening_keeps_animating() {
        let mut o = OverlayModel::new("Ctrl");
        o.set(OverlayState::Listening, "");
        assert_eq!(o.tick(), Tick::Redraw(LISTENING_TICK));

        o.set(OverlayState::Done, "landed");
        assert_eq!(o.tick(), Tick::Wait(Duration::from_millis(120)));
        o.shown_at = Instant::now() - DONE_LINGER;
        assert_eq!(o.tick(), Tick::Hide);

        o.set(OverlayState::Hidden, "");
        assert_eq!(o.tick(), Tick::Idle);
    }

    #[test]
    fn error_lingers_longer_than_done() {
        let mut o = OverlayModel::new("Ctrl");
        o.set(OverlayState::Error, "nothing heard");
        o.shown_at = Instant::now() - DONE_LINGER;
        assert_eq!(o.tick(), Tick::Wait(Duration::from_millis(200)), "still up");
        o.shown_at = Instant::now() - ERROR_LINGER;
        assert_eq!(o.tick(), Tick::Hide);
    }

    #[test]
    fn entering_listening_clears_the_meter() {
        let mut o = OverlayModel::new("Ctrl");
        o.set(OverlayState::Listening, "");
        for _ in 0..20 {
            o.set_level(0.5);
            o.advance();
        }
        assert!(o.level > 0.1);
        o.set(OverlayState::Done, "landed");
        o.set(OverlayState::Listening, "");
        assert_eq!(o.level, 0.0, "a new hold starts from silence");
        assert!(o.bars.iter().all(|b| *b == 0.0));
    }

    #[test]
    fn empty_states_say_something() {
        let mut o = OverlayModel::new("Ctrl");
        o.set(OverlayState::Listening, "");
        assert_eq!(o.body(), "Listening");
        o.set(OverlayState::Finalising, "");
        assert_eq!(o.body(), "Transcribing");
        o.set(OverlayState::Listening, "some words");
        assert_eq!(o.body(), "some words");
    }
}

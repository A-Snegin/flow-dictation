//! The floating status pill: the only thing Flow puts on screen.
//!
//! What it has to answer, in order of how much it matters:
//!   1. Is it listening right now, so my words are not being wasted?
//!   2. Is my voice actually reaching it? (a live meter, not a static dot)
//!   3. Am I seeing what I just said, rather than what I said first?
//!   4. Did the text land, or did I lose a sentence?
//!
//! Drawn with GDI into a 32-bit DIB and pushed with `UpdateLayeredWindow`.
//! Not Direct2D: this is a rounded rectangle, a few bars and one line of text,
//! repainted 25 times a second and only while the key is held. A D2D device and
//! swap chain would cost more memory and startup than the rest of the process.
//!
//! Three Win32 details this depends on, all of which were wrong at first:
//!
//! `SetLayeredWindowAttributes` and `UpdateLayeredWindow` are mutually
//! exclusive. Calling the first makes every later call to the second fail and
//! the window renders nothing at all. Only `UpdateLayeredWindow` is used here.
//!
//! `ULW_ALPHA` needs a real `BLENDFUNCTION` and premultiplied pixels. GDI
//! leaves the alpha byte at zero on everything it draws, so the bitmap is
//! post-processed: coverage comes from the rounded-rectangle shape, RGB is
//! multiplied by it, and that becomes the alpha channel.
//!
//! ClearType cannot be used on a layered window. It assumes an opaque
//! background and leaves coloured fringes along the alpha edges, so the font is
//! created with `ANTIALIASED_QUALITY`.
//!
//! `WS_EX_NOACTIVATE` matters more than it looks. If this window ever took
//! focus the caret would leave the user's document and the text would land in
//! the wrong place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, FillRect, GetDC, GetStockObject, ReleaseDC, RoundRect, SelectObject, SetBkMode,
    SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
    DT_CALCRECT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, HBITMAP, HDC, HFONT,
    HGDIOBJ, NULL_PEN, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, ShowWindow,
    SystemParametersInfoW, UpdateLayeredWindow, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

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

const DONE_LINGER: Duration = Duration::from_millis(1100);
const ERROR_LINGER: Duration = Duration::from_millis(3500);
/// 25 fps. Enough for the meter to read as fluid, cheap enough not to matter,
/// and only ever running while the pill is on screen.
pub const LISTENING_TICK: Duration = Duration::from_millis(40);

/// Bars in the voice meter. Odd number so there is a centre.
const BARS: usize = 5;

pub struct Overlay {
    hwnd: Option<HWND>,
    state: OverlayState,
    text: String,
    /// Smoothed 0..1 microphone level.
    level: f32,
    /// Per-bar heights, each chasing the level with its own lag so the group
    /// undulates instead of moving as one block.
    bars: [f32; BARS],
    /// Advances every tick and drives the travelling wave through the bars.
    phase: f32,
    shown_at: Instant,
    visible: bool,
    scale: f32,
    /// Fixed. The pill never resizes: a shape that grows and shrinks as words
    /// arrive is movement at the edge of vision, which is the opposite of what
    /// an unobtrusive indicator should be. Long sentences scroll inside it.
    width: i32,
    height: i32,
    /// Fonts, the memory DC and the bitmap are built once and reused. At 25 fps
    /// creating them per frame would be the most expensive thing the process
    /// does while the key is held, for no reason: none of them ever change.
    gdi: Option<Gdi>,
}

/// GDI objects held for the life of the overlay. Only ever touched from the
/// message thread, which is where the window lives.
struct Gdi {
    mem_dc: HDC,
    bitmap: HBITMAP,
    bits: *mut u8,
    text_font: HFONT,
    icon_font: HFONT,
    /// True when the icon font resolved to a face that actually has the glyph.
    icon_ok: bool,
    bg_brush: windows::Win32::Graphics::Gdi::HBRUSH,
    bolt_w: i32,
}

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// An overlay that draws nothing, for when window creation fails.
    /// Dictation has to keep working with no visual feedback.
    pub fn disabled() -> Overlay {
        Overlay {
            hwnd: None,
            state: OverlayState::Hidden,
            text: String::new(),
            level: 0.0,
            bars: [0.0; BARS],
            phase: 0.0,
            shown_at: Instant::now(),
            visible: false,
            scale: 1.0,
            width: 0,
            height: 0,
            gdi: None,
        }
    }

    pub fn create() -> Result<Overlay, String> {
        unsafe {
            let dpi = GetDpiForSystem();
            let scale = (dpi as f32 / 96.0).clamp(1.0, 3.0);
            let width = (430.0 * scale) as i32;
            let height = (52.0 * scale) as i32;

            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance.into(),
                    lpszClassName: w!("FlowOverlay"),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    return Err("RegisterClassW failed".into());
                }
            }

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW,
                w!("FlowOverlay"),
                w!("Flow"),
                WS_POPUP,
                0,
                0,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW: {e}"))?;

            // Deliberately no SetLayeredWindowAttributes here.

            let gdi = Gdi::create(width, height, scale);

            Ok(Overlay {
                hwnd: Some(hwnd),
                state: OverlayState::Hidden,
                text: String::new(),
                level: 0.0,
                bars: [0.0; BARS],
                phase: 0.0,
                shown_at: Instant::now(),
                visible: false,
                scale,
                width,
                height,
                gdi,
            })
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Live microphone level, 0..1. Fast attack so a syllable registers at
    /// once, slow release so the meter does not look dead between words.
    pub fn set_level(&mut self, level: f32) {
        let target = level.clamp(0.0, 1.0);
        self.level = if target > self.level {
            self.level * 0.4 + target * 0.6
        } else {
            self.level * 0.82 + target * 0.18
        };
    }

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
        self.render();
    }

    /// Called from the message loop. Advances the animation, repaints, and
    /// takes the pill down when a confirmation has been up long enough.
    /// Returns how long to wait before the next tick, or None when the screen
    /// is clear and the loop can go back to sleeping indefinitely.
    pub fn tick(&mut self) -> Option<Duration> {
        match self.state {
            OverlayState::Listening => {
                self.advance();
                self.render();
                Some(LISTENING_TICK)
            }
            OverlayState::Finalising => {
                // Keep the bars moving as they settle, so the moment between
                // release and text does not look like a freeze.
                self.set_level(0.0);
                self.advance();
                self.render();
                Some(LISTENING_TICK)
            }
            OverlayState::Done => {
                if self.shown_at.elapsed() >= DONE_LINGER {
                    self.set(OverlayState::Hidden, "");
                    None
                } else {
                    Some(Duration::from_millis(120))
                }
            }
            OverlayState::Error => {
                if self.shown_at.elapsed() >= ERROR_LINGER {
                    self.set(OverlayState::Hidden, "");
                    None
                } else {
                    Some(Duration::from_millis(200))
                }
            }
            OverlayState::Hidden => None,
        }
    }

    /// One animation step. Each bar chases a travelling wave scaled by the
    /// current level, and lags by an amount that grows towards the edges, which
    /// is what makes the group look like liquid rather than a bar chart.
    fn advance(&mut self) {
        self.phase += 0.30;
        if self.phase > std::f32::consts::TAU * 64.0 {
            self.phase -= std::f32::consts::TAU * 64.0;
        }
        // A floor so the meter breathes gently rather than flatlining in a
        // quiet room: it should look alive, not broken.
        let energy = (self.level * 1.25).min(1.0).max(0.06);
        for i in 0..BARS {
            let centre = (BARS as f32 - 1.0) / 2.0;
            let from_centre = (i as f32 - centre).abs() / centre.max(1.0);
            // Centre bars taller, edges shorter, plus a phase offset per bar.
            let shape = 1.0 - 0.45 * from_centre;
            let wave = (self.phase - i as f32 * 0.75).sin() * 0.5 + 0.5;
            let target = (energy * shape * (0.45 + 0.55 * wave)).clamp(0.04, 1.0);
            let lag = 0.55 - 0.10 * from_centre;
            self.bars[i] += (target - self.bars[i]) * lag;
        }
    }

    fn render(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            if self.state == OverlayState::Hidden {
                if self.visible {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    self.visible = false;
                }
                return;
            }
            self.paint(hwnd);
            if !self.visible {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                self.visible = true;
            }
        }
    }

    unsafe fn paint(&self, hwnd: HWND) {
        let Some(g) = self.gdi.as_ref() else { return };
        let w = self.width;
        let h = self.height;
        let s = self.scale;
        let pad = (17.0 * s) as i32;
        let gap = (13.0 * s) as i32;
        let mem_dc = g.mem_dc;

        let (accent, label) = match self.state {
            // COLORREF is 0x00BBGGRR.
            OverlayState::Listening => (COLORREF(0x00_6A_E0_53), "Listening"),
            OverlayState::Finalising => (COLORREF(0x00_30_C0_FF), "Transcribing"),
            OverlayState::Done => (COLORREF(0x00_6A_E0_53), "Inserted"),
            OverlayState::Error => (COLORREF(0x00_55_55_FF), "Nothing heard"),
            OverlayState::Hidden => (COLORREF(0), ""),
        };

        let show_meter = matches!(self.state, OverlayState::Listening | OverlayState::Finalising);
        let bar_w = (3.0 * s).round().max(2.0) as i32;
        let bar_gap = (4.0 * s).round().max(2.0) as i32;
        let meter_w = if show_meter {
            bar_w * BARS as i32 + bar_gap * (BARS as i32 - 1)
        } else {
            0
        };
        let text_left = pad + g.bolt_w + gap + if show_meter { meter_w + gap } else { 0 };
        let text_max = (w - pad - text_left).max(0);

        let body = if self.text.trim().is_empty() {
            label.to_string()
        } else {
            self.text.trim().to_string()
        };

        // ---- draw ---------------------------------------------------------
        let full = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe {
            FillRect(mem_dc, &full, g.bg_brush);
            SetBkMode(mem_dc, TRANSPARENT);
        }

        // Lightning bolt in the accent colour.
        if g.icon_ok {
            unsafe {
                SelectObject(mem_dc, g.icon_font.into());
                SetTextColor(mem_dc, accent);
            }
            let mut glyph = vec![BOLT_GLYPH];
            let mut r = RECT { left: pad, top: 0, right: pad + g.bolt_w, bottom: h };
            unsafe {
                DrawTextW(mem_dc, &mut glyph, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
        }

        // Voice meter: rounded bars, symmetric about the middle.
        if show_meter {
            let meter_left = pad + g.bolt_w + gap;
            let max_bar_h = (22.0 * s) as i32;
            let old_pen = unsafe { SelectObject(mem_dc, GetStockObject(NULL_PEN)) };
            for i in 0..BARS {
                let value = self.bars[i].clamp(0.0, 1.0);
                let bar_h = ((max_bar_h as f32) * value).round().max(bar_w as f32) as i32;
                let x = meter_left + i as i32 * (bar_w + bar_gap);
                let top = h / 2 - bar_h / 2;
                // Quieter bars sit dimmer, so the meter has depth instead of
                // being a row of identical blocks.
                let brush = unsafe { CreateSolidBrush(dim(accent, 0.45 + 0.55 * value)) };
                let old_brush = unsafe { SelectObject(mem_dc, brush.into()) };
                unsafe {
                    let _ = RoundRect(mem_dc, x, top, x + bar_w, top + bar_h, bar_w, bar_w);
                    SelectObject(mem_dc, old_brush);
                    let _ = DeleteObject(brush.into());
                }
            }
            unsafe {
                SelectObject(mem_dc, old_pen);
            }
        }

        // Text. A live hypothesis is dimmer than a finished result: it is going
        // to change and should not read as the text that landed.
        let colour = match self.state {
            OverlayState::Listening if !self.text.trim().is_empty() => COLORREF(0x00_C4_C4_C4),
            OverlayState::Error => COLORREF(0x00_BB_BB_FF),
            OverlayState::Done => COLORREF(0x00_F0_F0_F0),
            _ => COLORREF(0x00_E4_E4_E4),
        };
        unsafe {
            SelectObject(mem_dc, g.text_font.into());
            SetTextColor(mem_dc, colour);
        }

        // Follow the tail. While someone keeps talking, the words that matter
        // are the ones just said, not the ones at the start of the sentence.
        let (shown, clipped) = tail_that_fits(mem_dc, &body, text_max);
        let mut wide: Vec<u16> = shown.encode_utf16().collect();
        let mut text_rect = RECT { left: text_left, top: 0, right: w - pad, bottom: h };
        if !wide.is_empty() {
            unsafe {
                DrawTextW(
                    mem_dc,
                    &mut wide,
                    &mut text_rect,
                    DT_SINGLELINE | DT_VCENTER | DT_LEFT,
                );
            }
        }

        let pixels = unsafe { std::slice::from_raw_parts_mut(g.bits, (w * h * 4) as usize) };

        // When the sentence is longer than the pill, the left edge of the text
        // fades into the background rather than being chopped or prefixed with
        // an ellipsis. It reads as words scrolling past a window.
        if clipped {
            let fade = (34.0 * s) as i32;
            fade_into_background(pixels, w, h, text_left, text_left + fade, BG);
        }

        apply_rounded_alpha(pixels, w, h, h / 2, 242);

        // Bottom centre of the work area, so it never sits under the taskbar.
        let mut work = RECT::default();
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work as *mut RECT as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let mut pos = POINT {
            x: work.left + (work.right - work.left - w) / 2,
            y: work.bottom - h - (76.0 * s) as i32,
        };
        let mut size = SIZE { cx: w, cy: h };
        let mut src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let screen_dc = unsafe { GetDC(None) };
        unsafe {
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(screen_dc),
                Some(&mut pos),
                Some(&mut size),
                Some(mem_dc),
                Some(&mut src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen_dc);
        }
    }
}

/// The pill background, in BGR order to match the DIB layout.
const BG: [u8; 3] = [0x1B, 0x19, 0x17];
/// Lightning bolt in Segoe Fluent Icons and Segoe MDL2 Assets alike.
const BOLT_GLYPH: u16 = 0xE945;

impl Gdi {
    fn create(width: i32, height: i32, scale: f32) -> Option<Gdi> {
        unsafe {
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));

            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            SelectObject(mem_dc, bitmap.into());

            let text_font = make_font(17.0 * scale, FW_SEMIBOLD.0 as i32, w!("Segoe UI"));
            // Windows 11 ships Segoe Fluent Icons, Windows 10 has Segoe MDL2
            // Assets, and E945 is the bolt in both. If neither is installed the
            // glyph measures as nothing and it is left out rather than drawn as
            // a missing-character box.
            let mut icon_font =
                make_font(19.0 * scale, FW_NORMAL.0 as i32, w!("Segoe Fluent Icons"));
            SelectObject(mem_dc, icon_font.into());
            let mut bolt_w = measure(mem_dc, &[BOLT_GLYPH]);
            if bolt_w == 0 {
                let _ = DeleteObject(icon_font.into());
                icon_font = make_font(19.0 * scale, FW_NORMAL.0 as i32, w!("Segoe MDL2 Assets"));
                SelectObject(mem_dc, icon_font.into());
                bolt_w = measure(mem_dc, &[BOLT_GLYPH]);
            }
            let icon_ok = bolt_w > 0;

            let bg_brush = CreateSolidBrush(COLORREF(
                (BG[0] as u32) << 16 | (BG[1] as u32) << 8 | BG[2] as u32,
            ));

            ReleaseDC(None, screen_dc);
            Some(Gdi {
                mem_dc,
                bitmap,
                bits: bits as *mut u8,
                text_font,
                icon_font,
                icon_ok,
                bg_brush,
                bolt_w,
            })
        }
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.text_font.into());
            let _ = DeleteObject(self.icon_font.into());
            let _ = DeleteObject(self.bg_brush.into());
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

fn make_font(height: f32, weight: i32, face: PCWSTR) -> HFONT {
    unsafe {
        CreateFontW(
            height as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            face,
        )
    }
}

fn measure(dc: HDC, text: &[u16]) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let mut buf = text.to_vec();
    let mut r = RECT::default();
    unsafe {
        DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_CALCRECT | DT_LEFT);
    }
    r.right - r.left
}

fn measure_str(dc: HDC, s: &str) -> i32 {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    measure(dc, &utf16)
}

/// The longest suffix of `text` that fits in `max_px`, on a word boundary where
/// possible. Returns the suffix and whether anything was dropped.
fn tail_that_fits(dc: HDC, text: &str, max_px: i32) -> (String, bool) {
    if text.is_empty() || max_px <= 0 {
        return (String::new(), false);
    }
    if measure_str(dc, text) <= max_px {
        return (text.to_string(), false);
    }
    // Walk word starts from the end until the tail no longer fits, which is a
    // handful of measurements on a sentence rather than one per character.
    let mut best: Option<usize> = None;
    let bytes = text.as_bytes();
    let mut i = text.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b' ' {
            let candidate = i + 1;
            if measure_str(dc, &text[candidate..]) <= max_px {
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
        if measure_str(dc, &text[idx..]) > max_px {
            break;
        }
        start = idx;
    }
    (text[start..].to_string(), start > 0)
}

fn dim(colour: COLORREF, factor: f32) -> COLORREF {
    let f = factor.clamp(0.0, 1.0);
    let r = (colour.0 & 0xFF) as f32 * f;
    let g = ((colour.0 >> 8) & 0xFF) as f32 * f;
    let b = ((colour.0 >> 16) & 0xFF) as f32 * f;
    COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
}

/// Blends pixels toward the pill background across a horizontal band, left
/// edge fully faded, right edge untouched.
fn fade_into_background(pixels: &mut [u8], w: i32, h: i32, x0: i32, x1: i32, bg: [u8; 3]) {
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

/// Turns an opaque rectangle into a rounded, antialiased, premultiplied pill.
///
/// Coverage is computed per pixel from the distance to the rounded rectangle,
/// so corners fade over a pixel instead of stepping. `opacity` is the overall
/// alpha, 0..255.
fn apply_rounded_alpha(pixels: &mut [u8], w: i32, h: i32, radius: i32, opacity: u8) {
    let r = radius.clamp(0, w.min(h) / 2) as f32;
    let wf = w as f32;
    let hf = h as f32;
    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;
            // Distance outside the rounded rectangle's corner circles.
            let dx = (r - px).max(px - (wf - r)).max(0.0);
            let dy = (r - py).max(py - (hf - r)).max(0.0);
            let coverage = if dx > 0.0 && dy > 0.0 {
                let d = (dx * dx + dy * dy).sqrt();
                (r + 0.5 - d).clamp(0.0, 1.0)
            } else {
                1.0
            };

            let a = (coverage * opacity as f32) as u32;
            let i = ((y * w + x) * 4) as usize;
            if a == 0 {
                pixels[i..i + 4].fill(0);
                continue;
            }
            // UpdateLayeredWindow with AC_SRC_ALPHA wants premultiplied colour.
            for c in 0..3 {
                pixels[i + c] = ((pixels[i + c] as u32 * a) / 255) as u8;
            }
            pixels[i + 3] = a as u8;
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// Kept so the HGDIOBJ alias used through SelectObject stays imported.
const _: Option<HGDIOBJ> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_cut_and_the_middle_is_solid() {
        let (w, h) = (60, 20);
        let mut px = vec![200u8; (w * h * 4) as usize];
        apply_rounded_alpha(&mut px, w, h, 10, 255);
        let alpha = |x: i32, y: i32| px[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "top-left corner cut away");
        assert_eq!(alpha(w - 1, h - 1), 0, "bottom-right too");
        assert_eq!(alpha(w / 2, h / 2), 255, "middle solid");
        assert_eq!(alpha(w / 2, 0), 255, "top edge is not a corner");
    }

    #[test]
    fn colour_is_premultiplied() {
        let (w, h) = (10, 10);
        let mut px = vec![200u8; (w * h * 4) as usize];
        apply_rounded_alpha(&mut px, w, h, 0, 128);
        let i = ((5 * w + 5) * 4) as usize;
        assert_eq!(px[i + 3], 128);
        assert_eq!(px[i], (200 * 128 / 255) as u8);
    }

    #[test]
    fn fade_reaches_the_background_on_the_left_and_leaves_the_right() {
        let (w, h) = (20, 4);
        let mut px = vec![200u8; (w * h * 4) as usize];
        let bg = [10u8, 20, 30];
        fade_into_background(&mut px, w, h, 0, 10, bg);
        assert_eq!(px[0], bg[0], "left edge is fully background");
        assert_eq!(px[(9 * 4) as usize + 0], 181, "and blends back to the text");
        assert_eq!(px[(15 * 4) as usize], 200, "beyond the band, untouched");
    }

    #[test]
    fn level_attacks_fast_and_releases_slowly() {
        let mut o = Overlay::disabled();
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
        let mut o = Overlay::disabled();
        for _ in 0..100 {
            o.advance();
        }
        for b in o.bars {
            assert!(b > 0.0 && b <= 1.0, "bar out of range: {b}");
        }
        // Silence still breathes: the pill should not look broken in a quiet
        // room, but it must not look like speech either.
        assert!(o.bars.iter().all(|b| *b < 0.35), "silence should stay low");
    }

    #[test]
    fn bars_respond_to_level() {
        let mut o = Overlay::disabled();
        for _ in 0..40 {
            o.set_level(1.0);
            o.advance();
        }
        let loud = o.bars[BARS / 2];
        let mut q = Overlay::disabled();
        for _ in 0..40 {
            q.advance();
        }
        assert!(loud > q.bars[BARS / 2] * 2.0, "loud must read taller");
    }
}

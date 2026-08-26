//! The floating status pill: the only thing Flow puts on screen.
//!
//! A slim black bar low on the screen. Left to right: a recording dot, a live
//! waveform, the words as they are recognised with a caret at the end, and a
//! reminder of which key is being held.
//!
//! What it has to answer, in order of how much it matters:
//!   1. Is it listening right now, so my words are not being wasted?
//!   2. Is my voice actually reaching it? (a live trace, not a static dot)
//!   3. Am I seeing what I just said, rather than what I said first?
//!   4. Did the text land, or did I lose a sentence?
//!
//! Drawn with GDI into a 32-bit DIB and pushed with `UpdateLayeredWindow`.
//! Not Direct2D: this is a rounded rectangle, some bars and a line of text,
//! repainted 25 times a second and only while the key is held. A D2D device and
//! swap chain would cost more memory and startup than the rest of the process.
//!
//! Everything that can be built once is: fonts, the memory DC, the bitmap and
//! the rounded-corner alpha mask. Per frame there is drawing and a single pass
//! over the pixels. Idle CPU with Flow resident measures zero.
//!
//! Three Win32 details this depends on, all of which were wrong at first:
//!
//! `SetLayeredWindowAttributes` and `UpdateLayeredWindow` are mutually
//! exclusive. Calling the first makes every later call to the second fail and
//! the window renders nothing at all. Only `UpdateLayeredWindow` is used here.
//!
//! `ULW_ALPHA` needs a real `BLENDFUNCTION` and premultiplied pixels. GDI
//! leaves the alpha byte at zero on everything it draws, so the bitmap is
//! post-processed against a precomputed coverage mask.
//!
//! ClearType cannot be used on a layered window. It assumes an opaque
//! background and leaves coloured fringes along the alpha edges, so fonts are
//! created with `ANTIALIASED_QUALITY`.
//!
//! `WS_EX_NOACTIVATE` matters more than it looks. If this window ever took
//! focus the caret would leave the user's document and the text would land in
//! the wrong place.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, Ellipse, FillRect, GetDC, GetStockObject, ReleaseDC, RoundRect, SelectObject,
    SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DT_CALCRECT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_MEDIUM, FW_SEMIBOLD,
    HBITMAP, HBRUSH, HDC, HFONT, NULL_PEN, OUT_DEFAULT_PRECIS, TRANSPARENT,
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

const DONE_LINGER: Duration = Duration::from_millis(1000);
const ERROR_LINGER: Duration = Duration::from_millis(3000);
/// 25 fps: fluid enough to read as live, cheap enough not to matter, and only
/// ever running while the pill is on screen.
pub const LISTENING_TICK: Duration = Duration::from_millis(40);

/// Bars in the waveform. Thin and many, so it reads as a voice trace rather
/// than a level meter.
const BARS: usize = 11;

/// Fixed per-bar weights. A flat envelope looks synthetic; this gives the trace
/// the uneven shape a real waveform has, while staying identical frame to frame
/// so only the level and the travelling wave move.
const BAR_WEIGHT: [f32; BARS] = [
    0.42, 0.68, 0.50, 0.88, 0.60, 1.00, 0.55, 0.92, 0.48, 0.72, 0.40,
];

// COLORREF is 0x00BBGGRR.
const COL_BG: COLORREF = COLORREF(0x00_1C_1D_1E);
const COL_ACCENT: COLORREF = COLORREF(0x00_3C_60_F0); // coral
const COL_DONE: COLORREF = COLORREF(0x00_6A_C4_57);
const COL_ERROR: COLORREF = COLORREF(0x00_4D_48_E5);
const COL_TEXT: COLORREF = COLORREF(0x00_F2_F2_F2);
const COL_TEXT_LIVE: COLORREF = COLORREF(0x00_D6_D6_D6);
const COL_HINT: COLORREF = COLORREF(0x00_8E_8E_8E);
const COL_KEYCAP: COLORREF = COLORREF(0x00_36_38_3A);
const COL_KEYCAP_TEXT: COLORREF = COLORREF(0x00_D8_D8_D8);
const COL_WAVE: COLORREF = COLORREF(0x00_9A_9A_9A);
/// The background again, in BGR byte order, for the pixel passes.
const BG_BYTES: [u8; 3] = [0x1E, 0x1D, 0x1C];

pub struct Overlay {
    hwnd: Option<HWND>,
    state: OverlayState,
    text: String,
    /// Smoothed 0..1 microphone level.
    level: f32,
    /// Per-bar heights, each chasing the level with its own lag so the trace
    /// undulates instead of moving as one block.
    bars: [f32; BARS],
    /// Advances every tick and drives the travelling wave through the bars.
    phase: f32,
    shown_at: Instant,
    visible: bool,
    /// Which key the user is holding, drawn on the right as a reminder.
    hint_key: String,
    /// Peak level since the last frame, in thousandths, written by the capture
    /// thread. Read and cleared once per animation step: sampling it from the
    /// message loop instead meant reading it several times between frames, and
    /// every read after the first saw zero and pulled the meter down.
    level_source: Option<Arc<AtomicU32>>,
    scale: f32,
    /// Fixed. The pill never resizes: a shape that grows and shrinks as words
    /// arrive is movement at the edge of vision, which is the opposite of what
    /// an unobtrusive indicator should be. Long sentences scroll inside it.
    width: i32,
    height: i32,
    gdi: Option<Gdi>,
}

/// Everything that can be built once. Only touched from the message thread,
/// which is where the window lives.
struct Gdi {
    mem_dc: HDC,
    bitmap: HBITMAP,
    bits: *mut u8,
    text_font: HFONT,
    hint_font: HFONT,
    bg_brush: HBRUSH,
    /// Precomputed rounded-corner coverage, one byte per pixel. Computing it
    /// per frame meant a square root per pixel for a shape that never changes.
    mask: Vec<u8>,
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
            hint_key: "Ctrl".into(),
            level_source: None,
            scale: 1.0,
            width: 0,
            height: 0,
            gdi: None,
        }
    }

    pub fn create(hint_key: &str) -> Result<Overlay, String> {
        unsafe {
            let dpi = GetDpiForSystem();
            let scale = (dpi as f32 / 96.0).clamp(1.0, 3.0);
            let width = (560.0 * scale) as i32;
            let height = (44.0 * scale) as i32;

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

            Ok(Overlay {
                hwnd: Some(hwnd),
                state: OverlayState::Hidden,
                text: String::new(),
                level: 0.0,
                bars: [0.0; BARS],
                phase: 0.0,
                shown_at: Instant::now(),
                visible: false,
                hint_key: hint_key.to_string(),
                level_source: None,
                scale,
                width,
                height,
                gdi: Gdi::create(width, height, scale),
            })
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Hands the overlay the counter the capture thread writes peaks into.
    pub fn attach_level(&mut self, source: Arc<AtomicU32>) {
        self.level_source = Some(source);
    }

    /// Takes the loudest sample since the last frame and clears the counter.
    fn sample_level(&mut self) {
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
                self.sample_level();
                self.advance();
                self.render();
                Some(LISTENING_TICK)
            }
            OverlayState::Finalising => {
                // Keep the trace moving as it settles, so the moment between
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

    /// One animation step. Each bar chases its own weight scaled by the live
    /// level and a travelling wave, lagging by an amount that grows towards the
    /// edges, which is what makes the trace look fluid rather than mechanical.
    fn advance(&mut self) {
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
        let (w, h, s) = (self.width, self.height, self.scale);
        let dc = g.mem_dc;
        let px = |v: f32| (v * s).round() as i32;

        let accent = match self.state {
            OverlayState::Done => COL_DONE,
            OverlayState::Error => COL_ERROR,
            _ => COL_ACCENT,
        };
        let live = matches!(self.state, OverlayState::Listening | OverlayState::Finalising);

        unsafe {
            let full = RECT { left: 0, top: 0, right: w, bottom: h };
            FillRect(dc, &full, g.bg_brush);
            SetBkMode(dc, TRANSPARENT);
        }

        // ---- recording dot -------------------------------------------------
        let pad = px(17.0);
        let dot_r = px(5.0);
        let dot_dim = if self.state == OverlayState::Finalising { 0.55 } else { 1.0 };
        unsafe {
            let brush = CreateSolidBrush(dim(accent, dot_dim));
            let old_brush = SelectObject(dc, brush.into());
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let _ = Ellipse(dc, pad, h / 2 - dot_r, pad + dot_r * 2, h / 2 + dot_r);
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            let _ = DeleteObject(brush.into());
        }

        // ---- waveform ------------------------------------------------------
        let bar_w = px(2.0).max(2);
        let bar_gap = px(3.0).max(2);
        let wave_left = pad + dot_r * 2 + px(13.0);
        let wave_w = bar_w * BARS as i32 + bar_gap * (BARS as i32 - 1);
        if live {
            let max_h = px(19.0);
            // A floor tall enough to see. RoundRect on a rectangle only a
            // couple of pixels across degenerates to nothing at all, so short
            // bars are filled rectangles and only tall ones get rounded ends.
            let min_h = px(4.0).max(3);
            for i in 0..BARS {
                let value = self.bars[i].clamp(0.0, 1.0);
                let bar_h = ((max_h as f32) * value).round().max(min_h as f32) as i32;
                let x = wave_left + i as i32 * (bar_w + bar_gap);
                let top = h / 2 - bar_h / 2;
                // Taller bars brighter, so the trace has depth instead of
                // being a row of identical marks.
                let colour = dim(COL_WAVE, 0.55 + 0.45 * value);
                unsafe {
                    let brush = CreateSolidBrush(colour);
                    if bar_h > bar_w * 3 {
                        let old_brush = SelectObject(dc, brush.into());
                        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                        let _ = RoundRect(dc, x, top, x + bar_w, top + bar_h, bar_w, bar_w);
                        SelectObject(dc, old_pen);
                        SelectObject(dc, old_brush);
                    } else {
                        let r = RECT { left: x, top, right: x + bar_w, bottom: top + bar_h };
                        FillRect(dc, &r, brush);
                    }
                    let _ = DeleteObject(brush.into());
                }
            }
        }

        // ---- key hint on the right -----------------------------------------
        // Drawn before the text, so the text knows where it has to stop.
        let mut right_edge = w - pad;
        if self.state == OverlayState::Listening {
            unsafe {
                SelectObject(dc, g.hint_font.into());
            }
            let cap_text: Vec<u16> = self.hint_key.encode_utf16().collect();
            let cap_text_w = measure(dc, &cap_text);
            let cap_pad = px(7.0);
            let cap_w = cap_text_w + cap_pad * 2;
            let cap_h = px(20.0);
            let cap_left = right_edge - cap_w;
            let cap_top = h / 2 - cap_h / 2;

            unsafe {
                let brush = CreateSolidBrush(COL_KEYCAP);
                let old_brush = SelectObject(dc, brush.into());
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                let _ = RoundRect(
                    dc,
                    cap_left,
                    cap_top,
                    cap_left + cap_w,
                    cap_top + cap_h,
                    px(6.0),
                    px(6.0),
                );
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                let _ = DeleteObject(brush.into());

                SetTextColor(dc, COL_KEYCAP_TEXT);
                let mut buf = cap_text.clone();
                let mut r = RECT {
                    left: cap_left + cap_pad,
                    top: 0,
                    right: cap_left + cap_w,
                    bottom: h,
                };
                DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }

            let hold: Vec<u16> = "Hold".encode_utf16().collect();
            let hold_w = measure(dc, &hold);
            let hold_left = cap_left - px(8.0) - hold_w;
            unsafe {
                SetTextColor(dc, COL_HINT);
                let mut buf = hold;
                let mut r = RECT { left: hold_left, top: 0, right: cap_left, bottom: h };
                DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
            right_edge = hold_left - px(14.0);
        }

        // ---- the words -----------------------------------------------------
        let text_left = if live { wave_left + wave_w + px(15.0) } else { wave_left };
        let caret_space = if self.state == OverlayState::Listening { px(10.0) } else { 0 };
        let text_max = (right_edge - text_left - caret_space).max(0);

        let body = match (self.text.trim(), self.state) {
            ("", OverlayState::Listening) => "Listening".to_string(),
            ("", OverlayState::Finalising) => "Transcribing".to_string(),
            ("", OverlayState::Error) => "Nothing heard".to_string(),
            ("", _) => String::new(),
            (t, _) => t.to_string(),
        };

        unsafe {
            SelectObject(dc, g.text_font.into());
            SetTextColor(
                dc,
                match self.state {
                    OverlayState::Listening if !self.text.trim().is_empty() => COL_TEXT_LIVE,
                    OverlayState::Error => COL_HINT,
                    _ => COL_TEXT,
                },
            );
        }

        // Follow the tail. While someone keeps talking, the words that matter
        // are the ones just said, not the ones at the start of the sentence.
        let (shown, clipped) = tail_that_fits(dc, &body, text_max);
        let shown_w = measure_str(dc, &shown);
        if !shown.is_empty() {
            let mut buf: Vec<u16> = shown.encode_utf16().collect();
            let mut r = RECT { left: text_left, top: 0, right: right_edge, bottom: h };
            unsafe {
                DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
        }

        // Caret, right after the words: the same signal a text field gives.
        if self.state == OverlayState::Listening {
            let caret_x = (text_left + shown_w + px(5.0)).min(right_edge - px(2.0));
            let caret_h = px(18.0);
            let caret = RECT {
                left: caret_x,
                top: h / 2 - caret_h / 2,
                right: caret_x + px(2.0).max(1),
                bottom: h / 2 + caret_h / 2,
            };
            unsafe {
                let brush = CreateSolidBrush(COL_ACCENT);
                FillRect(dc, &caret, brush);
                let _ = DeleteObject(brush.into());
            }
        }

        // ---- compose --------------------------------------------------------
        let pixels = unsafe { std::slice::from_raw_parts_mut(g.bits, (w * h * 4) as usize) };

        // When the sentence is longer than the pill, the left edge of the text
        // fades into the background rather than being chopped or prefixed with
        // an ellipsis. It reads as words scrolling past a window.
        if clipped {
            fade_into_background(pixels, w, h, text_left, text_left + px(34.0), BG_BYTES);
        }
        premultiply_with_mask(pixels, &g.mask);

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
            y: work.bottom - h - px(78.0),
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
                Some(dc),
                Some(&mut src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen_dc);
        }
    }
}

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

            let text_font = make_font(16.5 * scale, FW_MEDIUM.0 as i32, w!("Segoe UI"));
            let hint_font = make_font(13.5 * scale, FW_SEMIBOLD.0 as i32, w!("Segoe UI"));
            let bg_brush = CreateSolidBrush(COL_BG);

            ReleaseDC(None, screen_dc);
            Some(Gdi {
                mem_dc,
                bitmap,
                bits: bits as *mut u8,
                text_font,
                hint_font,
                bg_brush,
                mask: rounded_mask(width, height, height / 2, 244),
            })
        }
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.text_font.into());
            let _ = DeleteObject(self.hint_font.into());
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
    // Walk word starts back from the end: a handful of measurements on a
    // sentence rather than one per character.
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

/// Blends pixels toward the pill background across a horizontal band: fully
/// background at the left edge, untouched at the right.
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

/// Coverage of a rounded rectangle, one byte per pixel, scaled by `opacity`.
/// Corners fade over a pixel rather than stepping, which is what antialiases
/// them. Computed once, because the shape never changes.
fn rounded_mask(w: i32, h: i32, radius: i32, opacity: u8) -> Vec<u8> {
    let r = radius.clamp(0, w.min(h) / 2) as f32;
    let (wf, hf) = (w as f32, h as f32);
    let mut mask = vec![0u8; (w * h) as usize];
    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let pxf = x as f32 + 0.5;
            let dx = (r - pxf).max(pxf - (wf - r)).max(0.0);
            let dy = (r - py).max(py - (hf - r)).max(0.0);
            let coverage = if dx > 0.0 && dy > 0.0 {
                let d = (dx * dx + dy * dy).sqrt();
                (r + 0.5 - d).clamp(0.0, 1.0)
            } else {
                1.0
            };
            mask[(y * w + x) as usize] = (coverage * opacity as f32) as u8;
        }
    }
    mask
}

/// Applies the mask as the alpha channel and premultiplies the colour, which is
/// what `UpdateLayeredWindow` with `AC_SRC_ALPHA` expects.
fn premultiply_with_mask(pixels: &mut [u8], mask: &[u8]) {
    for (i, &a) in mask.iter().enumerate() {
        let p = i * 4;
        if a == 0 {
            pixels[p..p + 4].fill(0);
            continue;
        }
        let a32 = a as u32;
        pixels[p] = ((pixels[p] as u32 * a32) / 255) as u8;
        pixels[p + 1] = ((pixels[p + 1] as u32 * a32) / 255) as u8;
        pixels[p + 2] = ((pixels[p + 2] as u32 * a32) / 255) as u8;
        pixels[p + 3] = a;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_cuts_corners_and_keeps_the_middle() {
        let (w, h) = (60, 20);
        let mask = rounded_mask(w, h, h / 2, 255);
        let at = |x: i32, y: i32| mask[(y * w + x) as usize];
        assert_eq!(at(0, 0), 0, "top-left corner cut away");
        assert_eq!(at(w - 1, h - 1), 0, "bottom-right too");
        assert_eq!(at(w / 2, h / 2), 255, "middle solid");
        assert_eq!(at(w / 2, 0), 255, "top edge is not a corner");
    }

    #[test]
    fn colour_is_premultiplied() {
        let (w, h) = (10, 10);
        let mut px = vec![200u8; (w * h * 4) as usize];
        let mask = vec![128u8; (w * h) as usize];
        premultiply_with_mask(&mut px, &mask);
        assert_eq!(px[3], 128);
        assert_eq!(px[0], (200 * 128 / 255) as u8);
    }

    #[test]
    fn transparent_pixels_are_cleared_entirely() {
        let mut px = vec![200u8; 4 * 4];
        let mask = vec![0u8, 255, 0, 255];
        premultiply_with_mask(&mut px, &mask);
        assert_eq!(&px[0..4], &[0, 0, 0, 0]);
        assert_eq!(px[7], 255);
    }

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
        assert!(o.bars.iter().all(|b| *b < 0.35), "silence should stay low");
    }

    #[test]
    fn bars_respond_to_level() {
        let mut loud = Overlay::disabled();
        for _ in 0..40 {
            loud.set_level(1.0);
            loud.advance();
        }
        let mut quiet = Overlay::disabled();
        for _ in 0..40 {
            quiet.advance();
        }
        let loudest = loud.bars.iter().cloned().fold(0.0f32, f32::max);
        let quietest = quiet.bars.iter().cloned().fold(0.0f32, f32::max);
        assert!(loudest > quietest * 2.0, "loud must read taller");
    }
}

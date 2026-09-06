//! The floating status pill on Windows: the only thing Flow puts on screen.
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
//! What it is made of, how it animates and when it goes away all live in
//! `crate::overlay_model`, shared with the Wayland version. This file is only
//! the Win32 part: a window and a bitmap.
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
use std::time::Duration;

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

use crate::overlay_model as model;
use crate::overlay_model::{OverlayModel, Tick, BARS};

pub use crate::overlay_model::{OverlayState, LISTENING_TICK};

/// The pill background in the byte order a 32-bit DIB uses, for the pixel pass.
const BG_BYTES: [u8; 3] = [
    model::COL_BG[2],
    model::COL_BG[1],
    model::COL_BG[0],
];

pub struct Overlay {
    hwnd: Option<HWND>,
    m: OverlayModel,
    visible: bool,
    scale: f32,
    /// Device pixels. The pill never resizes; see `overlay_model`.
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
            m: OverlayModel::new("Ctrl"),
            visible: false,
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
            let width = (model::WIDTH * scale) as i32;
            let height = (model::HEIGHT * scale) as i32;

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
                m: OverlayModel::new(hint_key),
                visible: false,
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

    /// Whether the drawing resources were built. Without them the window
    /// exists and is shown but every pixel stays transparent, which looks
    /// exactly like the overlay not working at all.
    pub fn is_drawable(&self) -> bool {
        self.gdi.is_some()
    }

    /// Hands the overlay the counter the capture thread writes peaks into.
    pub fn attach_level(&mut self, source: Arc<AtomicU32>) {
        self.m.attach_level(source);
    }

    /// Live microphone level from a raw sample peak, 0..1.
    pub fn set_level(&mut self, peak: f32) {
        self.m.set_level(peak);
    }

    pub fn set(&mut self, state: OverlayState, text: &str) {
        self.m.set(state, text);
        self.render();
    }

    /// Called from the message loop. Advances the animation, repaints, and
    /// takes the pill down when a confirmation has been up long enough.
    /// Returns how long to wait before the next tick, or None when the screen
    /// is clear and the loop can go back to sleeping indefinitely.
    pub fn tick(&mut self) -> Option<Duration> {
        match self.m.tick() {
            Tick::Redraw(next) => {
                self.render();
                Some(next)
            }
            Tick::Wait(next) => Some(next),
            Tick::Hide => {
                self.set(OverlayState::Hidden, "");
                None
            }
            Tick::Idle => None,
        }
    }

    fn render(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            if self.m.state == OverlayState::Hidden {
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

        let accent = colorref(self.m.accent());
        let live = self.m.is_live();

        unsafe {
            let full = RECT { left: 0, top: 0, right: w, bottom: h };
            FillRect(dc, &full, g.bg_brush);
            SetBkMode(dc, TRANSPARENT);
        }

        // ---- recording dot -------------------------------------------------
        let pad = px(model::PAD);
        let dot_r = px(model::DOT_RADIUS);
        let dot_dim = if self.m.state == OverlayState::Finalising { 0.55 } else { 1.0 };
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
        let bar_w = px(model::BAR_WIDTH).max(2);
        let bar_gap = px(model::BAR_GAP).max(2);
        let wave_left = pad + dot_r * 2 + px(model::DOT_TO_WAVE);
        let wave_w = bar_w * BARS as i32 + bar_gap * (BARS as i32 - 1);
        if live {
            let max_h = px(model::BAR_MAX_HEIGHT);
            // RoundRect on a rectangle only a couple of pixels across
            // degenerates to nothing at all, so short bars are filled
            // rectangles and only tall ones get rounded ends.
            let min_h = px(model::BAR_MIN_HEIGHT).max(2);
            for i in 0..BARS {
                let value = self.m.bars[i].clamp(0.0, 1.0);
                let bar_h = ((max_h as f32) * value).round().max(min_h as f32) as i32;
                let x = wave_left + i as i32 * (bar_w + bar_gap);
                let top = h / 2 - bar_h / 2;
                // Taller bars brighter, so the trace has depth instead of
                // being a row of identical marks.
                let colour = dim(colorref(model::COL_WAVE), 0.42 + 0.58 * value);
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
        if self.m.state == OverlayState::Listening {
            unsafe {
                SelectObject(dc, g.hint_font.into());
            }
            let cap_text: Vec<u16> = self.m.hint_key.encode_utf16().collect();
            let cap_text_w = measure(dc, &cap_text);
            let cap_pad = px(model::KEYCAP_PAD);
            let cap_w = cap_text_w + cap_pad * 2;
            let cap_h = px(model::KEYCAP_HEIGHT);
            let cap_left = right_edge - cap_w;
            let cap_top = h / 2 - cap_h / 2;

            unsafe {
                let brush = CreateSolidBrush(colorref(model::COL_KEYCAP));
                let old_brush = SelectObject(dc, brush.into());
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                let _ = RoundRect(
                    dc,
                    cap_left,
                    cap_top,
                    cap_left + cap_w,
                    cap_top + cap_h,
                    px(model::KEYCAP_RADIUS * 2.0),
                    px(model::KEYCAP_RADIUS * 2.0),
                );
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                let _ = DeleteObject(brush.into());

                SetTextColor(dc, colorref(model::COL_KEYCAP_TEXT));
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
            let hold_left = cap_left - px(model::HOLD_TO_KEYCAP) - hold_w;
            unsafe {
                SetTextColor(dc, colorref(model::COL_HINT));
                let mut buf = hold;
                let mut r = RECT { left: hold_left, top: 0, right: cap_left, bottom: h };
                DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
            right_edge = hold_left - px(model::HINT_TO_TEXT);
        }

        // ---- the words -----------------------------------------------------
        let text_left = if live { wave_left + wave_w + px(model::WAVE_TO_TEXT) } else { wave_left };
        let caret_space = if self.m.state == OverlayState::Listening {
            px(model::CARET_SPACE)
        } else {
            0
        };
        let text_max = (right_edge - text_left - caret_space).max(0);

        let body = self.m.body();

        unsafe {
            SelectObject(dc, g.text_font.into());
            SetTextColor(dc, colorref(self.m.text_colour()));
        }

        // Follow the tail. While someone keeps talking, the words that matter
        // are the ones just said, not the ones at the start of the sentence.
        let (shown, clipped) = model::tail_that_fits(&body, text_max, |s| measure_str(dc, s));
        let shown_w = measure_str(dc, &shown);
        if !shown.is_empty() {
            let mut buf: Vec<u16> = shown.encode_utf16().collect();
            let mut r = RECT { left: text_left, top: 0, right: right_edge, bottom: h };
            unsafe {
                DrawTextW(dc, &mut buf, &mut r, DT_SINGLELINE | DT_VCENTER | DT_LEFT);
            }
        }

        // Caret, right after the words: the same signal a text field gives.
        if self.m.state == OverlayState::Listening {
            let caret_x =
                (text_left + shown_w + px(model::CARET_OFFSET)).min(right_edge - px(2.0));
            let caret_h = px(model::CARET_HEIGHT);
            let caret = RECT {
                left: caret_x,
                top: h / 2 - caret_h / 2,
                right: caret_x + px(model::CARET_WIDTH).max(1),
                bottom: h / 2 + caret_h / 2,
            };
            unsafe {
                let brush = CreateSolidBrush(colorref(model::COL_ACCENT));
                FillRect(dc, &caret, brush);
                let _ = DeleteObject(brush.into());
            }
        }

        // ---- compose --------------------------------------------------------
        let pixels = unsafe { std::slice::from_raw_parts_mut(g.bits, (w * h * 4) as usize) };

        if clipped {
            model::fade_into_background(
                pixels,
                w,
                h,
                text_left,
                text_left + px(model::FADE_WIDTH),
                BG_BYTES,
            );
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
            y: work.bottom - h - px(model::BOTTOM_MARGIN),
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

            let text_font =
                make_font(model::TEXT_FONT_HEIGHT * scale, FW_MEDIUM.0 as i32, w!("Segoe UI"));
            let hint_font =
                make_font(model::HINT_FONT_HEIGHT * scale, FW_SEMIBOLD.0 as i32, w!("Segoe UI"));
            let bg_brush = CreateSolidBrush(colorref(model::COL_BG));

            ReleaseDC(None, screen_dc);
            Some(Gdi {
                mem_dc,
                bitmap,
                bits: bits as *mut u8,
                text_font,
                hint_font,
                bg_brush,
                mask: rounded_mask(width, height, height / 2, model::PILL_OPACITY),
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

/// A shared RGB colour as the 0x00BBGGRR word GDI wants.
const fn colorref(rgb: [u8; 3]) -> COLORREF {
    COLORREF((rgb[2] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[0] as u32)
}

fn dim(colour: COLORREF, factor: f32) -> COLORREF {
    let rgb = [
        (colour.0 & 0xFF) as u8,
        ((colour.0 >> 8) & 0xFF) as u8,
        ((colour.0 >> 16) & 0xFF) as u8,
    ];
    let d = model::dim(rgb, factor);
    COLORREF((d[2] as u32) << 16 | (d[1] as u32) << 8 | d[0] as u32)
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

    /// The shared colours are plain RGB; GDI wants them the other way round.
    #[test]
    fn colours_survive_the_trip_to_gdi() {
        assert_eq!(colorref(model::COL_ACCENT).0, 0x00_3C_60_F0);
        assert_eq!(colorref(model::COL_BG).0, 0x00_1C_1D_1E);
        assert_eq!(colorref(model::COL_KEYCAP).0, 0x00_36_38_3A);
    }
}

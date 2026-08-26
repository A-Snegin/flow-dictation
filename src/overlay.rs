//! The floating status pill.
//!
//! A layered, click-through, non-activating window drawn with GDI into a
//! 32-bit DIB and pushed with `UpdateLayeredWindow`. GDI rather than Direct2D
//! on purpose: the window is a rounded rectangle and one line of text, it
//! repaints only when the text changes, and a D2D device plus swap chain would
//! cost more memory and startup than the whole rest of the process.
//!
//! `WS_EX_NOACTIVATE` matters more than it looks: if this window ever took
//! focus, the caret would leave the user's document and the text would land in
//! the wrong place.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetDC, ReleaseDC, RoundRect, SelectObject, SetBkMode,
    SetTextColor, CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_SEMIBOLD, HBRUSH, HDC, PAINTSTRUCT,
    TRANSPARENT, CreatePen, PS_SOLID, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
    CLEARTYPE_QUALITY, DEFAULT_PITCH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics, RegisterClassW,
    SetLayeredWindowAttributes, ShowWindow, UpdateLayeredWindow, LWA_ALPHA,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayState {
    Hidden,
    Listening,
    Finalising,
    Error,
}

pub struct Overlay {
    hwnd: Option<HWND>,
    state: OverlayState,
    text: String,
}

const WIDTH: i32 = 460;
const HEIGHT: i32 = 56;
const MARGIN_BOTTOM: i32 = 120;

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

impl Overlay {
    /// An overlay that draws nothing, for when window creation fails. The app
    /// must keep dictating even with no visual feedback.
    pub fn disabled() -> Overlay {
        Overlay {
            hwnd: None,
            state: OverlayState::Hidden,
            text: String::new(),
        }
    }

    pub fn create() -> Result<Overlay, String> {
        unsafe {
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

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);
            let x = (screen_w - WIDTH) / 2;
            let y = screen_h - MARGIN_BOTTOM - HEIGHT;

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW,
                w!("FlowOverlay"),
                w!("Flow"),
                WS_POPUP,
                x,
                y,
                WIDTH,
                HEIGHT,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW: {e}"))?;

            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);

            Ok(Overlay {
                hwnd: Some(hwnd),
                state: OverlayState::Hidden,
                text: String::new(),
            })
        }
    }

    /// Repaints only when something actually changed. Called from the message
    /// thread on every state or partial-text update.
    pub fn set(&mut self, state: OverlayState, text: &str) {
        if self.state == state && self.text == text {
            return;
        }
        self.state = state;
        self.text.clear();
        self.text.push_str(text);

        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            if state == OverlayState::Hidden {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return;
            }
            self.paint(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    unsafe fn paint(&self, hwnd: HWND) {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: WIDTH,
                // Negative height: top-down rows, which is what everything else
                // in this file assumes.
                biHeight: -HEIGHT,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            Some(mem_dc),
            &info as *const _ as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        );
        let bitmap = match bitmap {
            Ok(b) => b,
            Err(_) => {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return;
            }
        };
        let old_bitmap = SelectObject(mem_dc, bitmap.into());
        let _ = &mut info;

        // Background: near-black pill with a state-coloured edge.
        let (edge, dot) = match self.state {
            OverlayState::Listening => (COLORREF(0x00_66_CC_00), COLORREF(0x00_66_FF_44)),
            OverlayState::Finalising => (COLORREF(0x00_00_99_FF), COLORREF(0x00_22_BB_FF)),
            OverlayState::Error => (COLORREF(0x00_33_33_DD), COLORREF(0x00_44_44_FF)),
            OverlayState::Hidden => (COLORREF(0), COLORREF(0)),
        };

        let bg = CreateSolidBrush(COLORREF(0x00_1A_18_16));
        let pen = CreatePen(PS_SOLID, 2, edge);
        let old_brush = SelectObject(mem_dc, bg.into());
        let old_pen = SelectObject(mem_dc, pen.into());
        let _ = RoundRect(mem_dc, 0, 0, WIDTH, HEIGHT, 18, 18);

        // State dot.
        let dot_brush = CreateSolidBrush(dot);
        let dot_rect = RECT {
            left: 18,
            top: HEIGHT / 2 - 5,
            right: 28,
            bottom: HEIGHT / 2 + 5,
        };
        FillRect(mem_dc, &dot_rect, dot_brush);

        // Text.
        let font = CreateFontW(
            19,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(mem_dc, font.into());
        SetBkMode(mem_dc, TRANSPARENT);
        SetTextColor(mem_dc, COLORREF(0x00_E8_E8_E8));

        let label = match self.state {
            OverlayState::Listening if self.text.is_empty() => "Listening...".to_string(),
            OverlayState::Finalising if self.text.is_empty() => "Finishing...".to_string(),
            OverlayState::Error => format!("Flow: {}", self.text),
            _ => self.text.clone(),
        };
        let mut wide: Vec<u16> = label.encode_utf16().collect();
        let mut text_rect = RECT {
            left: 40,
            top: 0,
            right: WIDTH - 16,
            bottom: HEIGHT,
        };
        if !wide.is_empty() {
            DrawTextW(
                mem_dc,
                &mut wide,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
            );
        }

        // The DIB is opaque; UpdateLayeredWindow needs an alpha channel, so
        // stamp full opacity across it. Per-pixel alpha would let the corners
        // round properly, which is a refinement, not a requirement.
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (WIDTH * HEIGHT * 4) as usize);
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }

        let mut pos = POINT {
            x: (GetSystemMetrics(SM_CXSCREEN) - WIDTH) / 2,
            y: GetSystemMetrics(SM_CYSCREEN) - MARGIN_BOTTOM - HEIGHT,
        };
        let mut size = SIZE {
            cx: WIDTH,
            cy: HEIGHT,
        };
        let mut src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            Some(&mut pos),
            Some(&mut size),
            Some(mem_dc),
            Some(&mut src),
            COLORREF(0),
            None,
            ULW_ALPHA,
        );

        SelectObject(mem_dc, old_font);
        SelectObject(mem_dc, old_pen);
        SelectObject(mem_dc, old_brush);
        SelectObject(mem_dc, old_bitmap);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(bg.into());
        let _ = DeleteObject(dot_brush.into());
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
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
    const WM_PAINT: u32 = 0x000F;
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        let _: HDC = unsafe { BeginPaint(hwnd, &mut ps) };
        let _ = unsafe { EndPaint(hwnd, &ps) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// Kept so the unused-import lint does not fight the brush type alias used above.
const _: Option<HBRUSH> = None;
const _: Option<PCWSTR> = None;

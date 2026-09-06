//! The settings window.
//!
//! Plain Win32 controls, created when the window opens and destroyed when it
//! closes. No WebView, no framework, no second process: it appears instantly,
//! costs nothing while it is shut, and matches everything else about how this
//! application is built.
//!
//! It exposes exactly what the settings file exposes and nothing more. The file
//! remains the source of truth and stays hand-editable; this is a friendlier
//! way to reach the same values.
//!
//! The window lives on the message thread alongside the hotkey hook and the
//! overlay. It is deliberately not modal: dictation keeps working while it is
//! open, which is the obvious way to try a setting you have just changed.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetStockObject, HBRUSH, ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, FW_NORMAL, HFONT, OUT_DEFAULT_PRECIS, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowTextLengthW,
    GetWindowTextW,
    LoadCursorW, LoadIconW, PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetWindowPos, SetWindowTextW, ShowWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON,
    BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_NUMBER, IDC_ARROW, IDI_APPLICATION, SW_SHOW,
    WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_OVERLAPPED, WS_CAPTION, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::settings::Settings;

const ID_HOTKEY: i32 = 200;
const ID_PROFILE: i32 = 201;
const ID_SINGLE_THREAD: i32 = 202;
const ID_CADENCE: i32 = 203;
const ID_TAIL: i32 = 204;
const ID_BOOST: i32 = 205;
const ID_INSERT: i32 = 206;
const ID_TERMINAL: i32 = 207;
const ID_CAPS: i32 = 208;
const ID_PUNCT: i32 = 209;
const ID_TRAILING: i32 = 210;
const ID_DICTIONARY: i32 = 211;
const ID_SAVE: i32 = 212;
const ID_CANCEL: i32 = 213;
const ID_EDIT_FILE: i32 = 214;

/// Order matters: the index into these arrays is what the combo box stores.
const HOTKEYS: &[(&str, &str)] = &[
    ("rightctrl", "Right Ctrl"),
    ("rightalt", "Right Alt"),
    ("rightshift", "Right Shift"),
    ("capslock", "Caps Lock"),
    ("leftctrl", "Left Ctrl"),
    ("f13", "F13"),
];
const PROFILES: &[(&str, &str)] = &[
    ("balanced", "Balanced: small model, more accurate"),
    ("fast", "Fast: tiny model, about twice as quick"),
];
/// The labels say what the choice costs, because in an ordinary text box the
/// two produce identical text and the difference only shows up elsewhere.
const MODES: &[(&str, &str)] = &[
    ("paste", "Paste: instant, borrows the clipboard"),
    ("type", "Type: slower on long text, no clipboard"),
];

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static SAVED: AtomicBool = AtomicBool::new(false);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Set when the user saves, so the message loop can apply what applies live.
pub fn take_saved() -> bool {
    SAVED.swap(false, Ordering::SeqCst)
}

pub fn is_open() -> bool {
    WINDOW.load(Ordering::SeqCst) != 0
}

/// Fills the dictionary box and saves, using exactly the code path a person
/// typing into it would. Cross-process automation cannot put text into an edit
/// control, so without this the read-and-save path could only be checked by
/// hand.
pub fn test_roundtrip(settings: &Settings, dictionary_text: &str) -> bool {
    unsafe { create(settings) };
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if hwnd == 0 {
        return false;
    }
    let hwnd = HWND(hwnd as *mut _);
    let Some(box_) = (unsafe { child(hwnd, ID_DICTIONARY) }) else {
        return false;
    };
    set_text(box_, dictionary_text);
    unsafe { collect_and_save(hwnd) };
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    take_saved()
}

/// Opens the window, or brings it forward if it is already up.
pub fn open(settings: &Settings) {
    let existing = WINDOW.load(Ordering::SeqCst);
    if existing != 0 {
        unsafe {
            let _ = SetForegroundWindow(HWND(existing as *mut _));
        }
        return;
    }
    unsafe { create(settings) };
}

struct Layout {
    scale: f32,
    y: i32,
    font: HFONT,
    parent: HWND,
    instance: windows::Win32::Foundation::HMODULE,
}

impl Layout {
    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// A label on the left, a control on the right, and a row of vertical
    /// space consumed. Every row in this window is built this way, which is
    /// what keeps the layout code short enough to read.
    unsafe fn row(
        &mut self,
        label: PCWSTR,
        class: PCWSTR,
        style: u32,
        ex: WINDOW_EX_STYLE,
        id: i32,
        height: f32,
        advance: f32,
    ) -> HWND {
        let label_w = self.px(168.0);
        let control_w = self.px(296.0);
        let h = self.px(height);
        unsafe {
            let text = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                label,
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0,
                ),
                self.px(18.0),
                self.y + self.px(4.0),
                label_w,
                self.px(20.0),
                Some(self.parent),
                None,
                Some(self.instance.into()),
                None,
            );
            if let Ok(t) = text {
                SendMessageW(t, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            }

            let control = CreateWindowExW(
                ex,
                class,
                w!(""),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | style,
                ),
                self.px(18.0) + label_w,
                self.y,
                control_w,
                h,
                Some(self.parent),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(id as isize as *mut _)),
                Some(self.instance.into()),
                None,
            )
            .unwrap_or_default();
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(self.font.0 as usize)),
                Some(LPARAM(1)),
            );
            // For a drop-down list, `height` is how far the list falls open,
            // not how tall the closed control is. Advancing the layout by that
            // would leave a hundred pixels of nothing under every combo box.
            self.y += self.px(advance) + self.px(9.0);
            control
        }
    }
}

unsafe fn create(settings: &Settings) {
    let dpi = unsafe { GetDpiForSystem() };
    let scale = (dpi as f32 / 96.0).clamp(1.0, 3.0);
    let px = |v: f32| (v * scale).round() as i32;

    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };

    if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: w!("FlowSettings"),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            hbrBackground: HBRUSH(unsafe { GetStockObject(WHITE_BRUSH) }.0),
            hIcon: unsafe {
                LoadIconW(Some(instance.into()), PCWSTR(1usize as *const u16))
                    .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            }
            .unwrap_or_default(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return;
        }
    }

    // Ask Windows how big the frame must be for the client area the controls
    // need, rather than guessing and having the right-hand column fall off.
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: px(500.0),
        bottom: px(600.0),
    };
    let _ = unsafe {
        AdjustWindowRect(
            &mut frame,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0,
            ),
            false,
        )
    };
    let width = frame.right - frame.left;
    let height = frame.bottom - frame.top;
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("FlowSettings"),
            w!("Flow settings"),
            // WS_VISIBLE in the style rather than relying on ShowWindow: the
            // window was being created and populated correctly and then never
            // appearing, which reads to the user as the menu item doing
            // nothing at all.
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_VISIBLE.0,
            ),
            px(220.0),
            px(120.0),
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }) else {
        return;
    };

    let font = unsafe {
        CreateFontW(
            px(16.0),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    };

    let mut l = Layout {
        scale,
        y: px(16.0),
        font,
        parent: hwnd,
        instance,
    };

    unsafe {
        // ---- what key, which model -------------------------------------
        let hotkey = l.row(
            w!("Hold to dictate"),
            w!("COMBOBOX"),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            WINDOW_EX_STYLE(0),
            ID_HOTKEY,
            160.0,
            24.0,
        );
        fill_combo(hotkey, HOTKEYS, &settings.hotkey.key);

        let profile = l.row(
            w!("Model"),
            w!("COMBOBOX"),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            WINDOW_EX_STYLE(0),
            ID_PROFILE,
            160.0,
            24.0,
        );
        fill_combo(profile, PROFILES, &settings.model.profile);

        let single = l.row(
            w!(""),
            w!("BUTTON"),
            BS_AUTOCHECKBOX as u32,
            WINDOW_EX_STYLE(0),
            ID_SINGLE_THREAD,
            22.0,
            22.0,
        );
        let _ = SetWindowTextW(single, w!("One inference thread (faster tail, far less CPU)"));
        set_check(single, settings.model.single_thread);

        // ---- timing ------------------------------------------------------
        let cadence = l.row(
            w!("Live text every (ms)"),
            w!("EDIT"),
            ES_AUTOHSCROLL as u32 | ES_NUMBER as u32 | WS_BORDER.0,
            WS_EX_CLIENTEDGE,
            ID_CADENCE,
            22.0,
            22.0,
        );
        set_text(cadence, &settings.model.partial_cadence_ms.to_string());

        let tail = l.row(
            w!("Keep listening after (ms)"),
            w!("EDIT"),
            ES_AUTOHSCROLL as u32 | ES_NUMBER as u32 | WS_BORDER.0,
            WS_EX_CLIENTEDGE,
            ID_TAIL,
            22.0,
            22.0,
        );
        set_text(tail, &settings.model.release_tail_ms.to_string());

        let boost = l.row(
            w!("Dictionary bias strength"),
            w!("EDIT"),
            ES_AUTOHSCROLL as u32 | WS_BORDER.0,
            WS_EX_CLIENTEDGE,
            ID_BOOST,
            22.0,
            22.0,
        );
        set_text(boost, &format!("{:.1}", settings.model.keyterm_boost));

        // ---- where the text goes ------------------------------------------
        let insert = l.row(
            w!("Insert by"),
            w!("COMBOBOX"),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            WINDOW_EX_STYLE(0),
            ID_INSERT,
            120.0,
            24.0,
        );
        fill_combo(insert, MODES, &settings.insertion.mode);

        let terminal = l.row(
            w!("In terminals, insert by"),
            w!("COMBOBOX"),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            WINDOW_EX_STYLE(0),
            ID_TERMINAL,
            120.0,
            24.0,
        );
        fill_combo(terminal, MODES, &settings.insertion.terminal_mode);

        // ---- formatting ----------------------------------------------------
        let caps = l.row(
            w!(""),
            w!("BUTTON"),
            BS_AUTOCHECKBOX as u32,
            WINDOW_EX_STYLE(0),
            ID_CAPS,
            22.0,
            22.0,
        );
        let _ = SetWindowTextW(caps, w!("Capitalise sentences"));
        set_check(caps, settings.formatting.capitalise_sentences);

        let punct = l.row(
            w!(""),
            w!("BUTTON"),
            BS_AUTOCHECKBOX as u32,
            WINDOW_EX_STYLE(0),
            ID_PUNCT,
            22.0,
            22.0,
        );
        let _ = SetWindowTextW(punct, w!("Spoken punctuation (\"comma\", \"new line\")"));
        set_check(punct, settings.formatting.spoken_punctuation);

        let trailing = l.row(
            w!(""),
            w!("BUTTON"),
            BS_AUTOCHECKBOX as u32,
            WINDOW_EX_STYLE(0),
            ID_TRAILING,
            22.0,
            22.0,
        );
        let _ = SetWindowTextW(trailing, w!("Add a space after each dictation"));
        set_check(trailing, settings.formatting.trailing_space);

        // ---- dictionary -----------------------------------------------------
        let dictionary = l.row(
            w!("Dictionary\r\n(spoken = written)"),
            w!("EDIT"),
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL.0 | WS_BORDER.0,
            WS_EX_CLIENTEDGE,
            ID_DICTIONARY,
            110.0,
            110.0,
        );
        set_text(dictionary, &dictionary_to_text(settings));

        // ---- buttons ---------------------------------------------------------
        let button_y = l.y + l.px(6.0);
        let button_w = l.px(96.0);
        let button_h = l.px(28.0);
        let make_button = |id: i32, text: PCWSTR, x: i32, default: bool| {
            let style = if default { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
            if let Ok(b) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                text,
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | style as u32,
                ),
                x,
                button_y,
                button_w,
                button_h,
                Some(hwnd),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    id as isize as *mut _,
                )),
                Some(instance.into()),
                None,
            ) {
                SendMessageW(b, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
        };
        make_button(ID_SAVE, w!("Save"), px(18.0), true);
        make_button(ID_CANCEL, w!("Cancel"), px(18.0) + button_w + px(10.0), false);
        make_button(
            ID_EDIT_FILE,
            w!("Edit file"),
            px(18.0) + (button_w + px(10.0)) * 2,
            false,
        );

        let note = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Everything here applies when you save. Changing the model reloads it, about a second."),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0,
            ),
            px(18.0),
            button_y + button_h + px(12.0),
            px(450.0),
            px(34.0),
            Some(hwnd),
            None,
            Some(instance.into()),
            None,
        );
        if let Ok(n) = note {
            SendMessageW(n, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }

        // Size the frame to where the controls actually ended up. Guessing a
        // height and hoping meant the buttons sat just past the bottom edge,
        // and every change to a row nudged it again.
        let needed_client = button_y + button_h + px(12.0) + px(34.0) + px(14.0);
        let mut want = RECT {
            left: 0,
            top: 0,
            right: px(500.0),
            bottom: needed_client,
        };
        let _ = AdjustWindowRect(
            &mut want,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0,
            ),
            false,
        );
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            want.right - want.left,
            want.bottom - want.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );

        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        FONT.store(font.0 as isize, Ordering::SeqCst);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

static FONT: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_SAVE => {
                    unsafe { collect_and_save(hwnd) };
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    return LRESULT(0);
                }
                ID_CANCEL => {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    return LRESULT(0);
                }
                ID_EDIT_FILE => {
                    let path = Settings::path();
                    let _ = std::process::Command::new("notepad.exe")
                        .arg(path.as_os_str())
                        .spawn();
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            WINDOW.store(0, Ordering::SeqCst);
            let font = FONT.swap(0, Ordering::SeqCst);
            if font != 0 {
                unsafe {
                    let _ = DeleteObject(HFONT(font as *mut _).into());
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Reads every control back into a Settings and writes it to disk. Values that
/// cannot be parsed keep whatever is already saved rather than resetting to a
/// default: a half-typed number should not silently change a setting.
unsafe fn collect_and_save(hwnd: HWND) {
    let (mut settings, _) = Settings::load();

    if let Some(i) = unsafe { combo_index(hwnd, ID_HOTKEY) } {
        if let Some((value, _)) = HOTKEYS.get(i) {
            settings.hotkey.key = value.to_string();
        }
    }
    if let Some(i) = unsafe { combo_index(hwnd, ID_PROFILE) } {
        if let Some((value, _)) = PROFILES.get(i) {
            settings.model.profile = value.to_string();
        }
    }
    if let Some(i) = unsafe { combo_index(hwnd, ID_INSERT) } {
        if let Some((value, _)) = MODES.get(i) {
            settings.insertion.mode = value.to_string();
        }
    }
    if let Some(i) = unsafe { combo_index(hwnd, ID_TERMINAL) } {
        if let Some((value, _)) = MODES.get(i) {
            settings.insertion.terminal_mode = value.to_string();
        }
    }

    settings.model.single_thread = unsafe { checked(hwnd, ID_SINGLE_THREAD) };
    settings.formatting.capitalise_sentences = unsafe { checked(hwnd, ID_CAPS) };
    settings.formatting.spoken_punctuation = unsafe { checked(hwnd, ID_PUNCT) };
    settings.formatting.trailing_space = unsafe { checked(hwnd, ID_TRAILING) };

    if let Ok(v) = unsafe { text_of(hwnd, ID_CADENCE) }.trim().parse::<u64>() {
        settings.model.partial_cadence_ms = v.clamp(0, 5_000);
    }
    if let Ok(v) = unsafe { text_of(hwnd, ID_TAIL) }.trim().parse::<u64>() {
        settings.model.release_tail_ms = v.clamp(0, 500);
    }
    if let Ok(v) = unsafe { text_of(hwnd, ID_BOOST) }.trim().parse::<f32>() {
        settings.model.keyterm_boost = v.clamp(0.0, 10.0);
    }

    let raw = unsafe { text_of(hwnd, ID_DICTIONARY) };
    let parsed = text_to_dictionary(&raw);
    // Say so when a line was thrown away. Silently discarding what someone
    // typed is how a dictionary comes to look like it does nothing.
    let lines = raw
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .count();
    if lines != parsed.len() {
        eprintln!("dictionary: kept {} of {lines} lines", parsed.len());
    }
    settings.dictionary = parsed;

    match settings.save() {
        Ok(()) => SAVED.store(true, Ordering::SeqCst),
        Err(e) => eprintln!("could not save settings: {e}"),
    }
}

fn dictionary_to_text(settings: &Settings) -> String {
    let mut entries: Vec<(&String, &String)> = settings.dictionary.iter().collect();
    entries.sort();
    entries
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// One entry per line. Two forms, because people reach for both:
///
///   Lift-Off Consulting          a term to recognise, written as typed
///   lift off = Lift-Off          heard on the left, written on the right
///
/// The bare form matters. Someone who wants their company name recognised
/// types the company name; silently discarding that because it had no equals
/// sign is how a dictionary appears not to work at all.
pub fn text_to_dictionary(text: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (spoken, written) = match line.split_once('=') {
            Some((left, right)) => (
                left.trim().trim_matches('"').to_string(),
                right.trim().trim_matches('"').to_string(),
            ),
            // Bare term: bias the recogniser towards it, and leave it alone
            // afterwards because it is already written the way it should be.
            None => {
                let term = line.trim_matches('"').to_string();
                // Match how it is said, not how it is written. Nobody
                // pronounces the hyphen in "Lift-Off", so a bare term keyed on
                // "lift-off consulting" would never fire against a transcript
                // that reads "lift off consulting".
                let spoken = term
                    .to_lowercase()
                    .replace(['-', '_', '/'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                (spoken, term)
            }
        };
        if !spoken.is_empty() && !written.is_empty() {
            map.insert(spoken, written);
        }
    }
    map
}

// ---- small control helpers ------------------------------------------------

fn fill_combo(combo: HWND, items: &[(&str, &str)], current: &str) {
    unsafe {
        let mut selected = 0usize;
        for (i, (value, label)) in items.iter().enumerate() {
            let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(wide.as_ptr() as isize)),
            );
            if value.eq_ignore_ascii_case(current) {
                selected = i;
            }
        }
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(selected)), Some(LPARAM(0)));
    }
}

unsafe fn combo_index(parent: HWND, id: i32) -> Option<usize> {
    let control = unsafe { child(parent, id) }?;
    let r = unsafe { SendMessageW(control, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))) };
    if r.0 < 0 {
        None
    } else {
        Some(r.0 as usize)
    }
}

fn set_check(control: HWND, on: bool) {
    unsafe {
        SendMessageW(
            control,
            BM_SETCHECK,
            Some(WPARAM(if on { 1 } else { 0 })),
            Some(LPARAM(0)),
        );
    }
}

unsafe fn checked(parent: HWND, id: i32) -> bool {
    match unsafe { child(parent, id) } {
        Some(c) => unsafe { SendMessageW(c, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))) }.0 == 1,
        None => false,
    }
}

fn set_text(control: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetWindowTextW(control, PCWSTR(wide.as_ptr()));
    }
}

unsafe fn text_of(parent: HWND, id: i32) -> String {
    let Some(control) = (unsafe { child(parent, id) }) else {
        return String::new();
    };
    unsafe {
        let len = GetWindowTextLengthW(control);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(control, &mut buf);
        String::from_utf16_lossy(&buf[..got as usize])
    }
}

unsafe fn child(parent: HWND, id: i32) -> Option<HWND> {
    let h = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(parent), id)
    };
    match h {
        Ok(h) if !h.is_invalid() => Some(h),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_round_trips() {
        let text = "lift off = Lift-Off\r\ndddm = DDDM";
        let map = text_to_dictionary(text);
        assert_eq!(map.get("lift off").map(String::as_str), Some("Lift-Off"));
        assert_eq!(map.get("dddm").map(String::as_str), Some("DDDM"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn quotes_and_blank_lines_are_tolerated() {
        let map = text_to_dictionary("\n  \"lift off\" = \"Lift-Off\"  \n\n# a note\n");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("lift off").map(String::as_str), Some("Lift-Off"));
    }

    #[test]
    fn empty_sides_are_rejected() {
        let map = text_to_dictionary(" = Lift-Off\nfoo =\n");
        assert!(map.is_empty());
    }
}

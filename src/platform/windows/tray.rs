//! Notification-area icon and menu.
//!
//! A hidden window receives the icon's callbacks. It records that a menu was
//! asked for and returns; the menu itself is shown from the message loop.
//!
//! That split matters. `TrackPopupMenu` runs its own modal loop and does not
//! return until the user picks something or clicks away, so calling it from
//! inside the window procedure means blocking inside a Windows callback with
//! the low-level keyboard hook and the ASR event pump stuck behind it.
//!
//! Two Win32 requirements this depends on, and whose absence is the usual
//! reason a tray menu does nothing at all: the window must be brought to the
//! foreground before `TrackPopupMenu` or the menu is dismissed the instant it
//! appears, and a message must be posted afterwards or it can linger on screen
//! after a click elsewhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadIconW, PostMessageW, RegisterClassW, SetForegroundWindow, TrackPopupMenu,
    HICON, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTALIGN, WINDOW_EX_STYLE, WM_NULL, WNDCLASSW, WS_OVERLAPPED,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Toggle,
    LatencyReport,
    OpenSettings,
    ReloadDictionary,
    ToggleAutostart,
    Quit,
}

const WM_TRAY: u32 = 0x0400 + 1; // WM_APP + 1
const ID_TOGGLE: usize = 1;
const ID_REPORT: usize = 2;
const ID_SETTINGS: usize = 3;
const ID_RELOAD: usize = 4;
const ID_AUTOSTART: usize = 5;
const ID_QUIT: usize = 6;

/// The icon compiled into the executable. Matches the id used in build.rs.
const IDI_FLOW: u16 = 1;

static SENDER: OnceLock<Mutex<Sender<TrayCommand>>> = OnceLock::new();
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
/// Set by the window procedure, cleared by the message loop.
static MENU_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct Tray {
    hwnd: Option<HWND>,
    rx: Option<Receiver<TrayCommand>>,
    paused: bool,
}

impl Tray {
    /// A tray that does nothing, for when the shell will not cooperate.
    pub fn disabled() -> Tray {
        Tray {
            hwnd: None,
            rx: None,
            paused: false,
        }
    }

    pub fn create() -> Result<Tray, String> {
        let (tx, rx) = channel::<TrayCommand>();
        SENDER
            .set(Mutex::new(tx))
            .map_err(|_| "tray already created".to_string())?;

        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance.into(),
                    lpszClassName: w!("FlowTray"),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    return Err("RegisterClassW failed".into());
                }
            }

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("FlowTray"),
                w!("Flow"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW: {e}"))?;

            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAY,
                hIcon: app_icon(),
                ..Default::default()
            };
            set_tip(&mut data, "Flow: hold Right Ctrl to dictate");
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                let _ = DestroyWindow(hwnd);
                return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
            }

            Ok(Tray {
                hwnd: Some(hwnd),
                rx: Some(rx),
                paused: false,
            })
        }
    }

    /// Reflects the enabled state in the tooltip, so hovering the icon says
    /// whether Flow is listening for the hotkey at all.
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        let Some(hwnd) = self.hwnd else { return };
        unsafe {
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_TIP,
                ..Default::default()
            };
            set_tip(
                &mut data,
                if paused {
                    "Flow: paused"
                } else {
                    "Flow: hold Right Ctrl to dictate"
                },
            );
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// Called every pass of the message loop. Shows the menu if the icon was
    /// clicked, then returns whatever the user chose.
    pub fn poll(&self) -> Option<TrayCommand> {
        if MENU_REQUESTED.swap(false, Ordering::SeqCst) {
            if let Some(hwnd) = self.hwnd {
                unsafe { show_menu(hwnd, self.paused) };
            }
        }
        self.rx.as_ref()?.try_recv().ok()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take() {
            unsafe {
                let data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: 1,
                    ..Default::default()
                };
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

fn set_tip(data: &mut NOTIFYICONDATAW, tip: &str) {
    data.szTip = [0; 128];
    for (i, c) in tip.encode_utf16().enumerate().take(127) {
        data.szTip[i] = c;
    }
}

/// The application icon compiled into the executable, falling back to the
/// system default if the resource is not there.
fn app_icon() -> HICON {
    unsafe {
        if let Ok(instance) = GetModuleHandleW(None) {
            if let Ok(icon) = LoadIconW(
                Some(instance.into()),
                PCWSTR(IDI_FLOW as usize as *const u16),
            ) {
                if !icon.is_invalid() {
                    return icon;
                }
            }
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_CONTEXTMENU: u32 = 0x007B;

    if msg == WM_TRAY {
        let event = (lparam.0 as u32) & 0xFFFF;
        if event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU {
            // Flag only: the modal menu loop belongs on the message loop, not
            // inside a callback the hotkey hook is queued behind.
            MENU_REQUESTED.store(true, Ordering::SeqCst);
            return LRESULT(0);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn show_menu(hwnd: HWND, paused: bool) {
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(m) => m,
        Err(_) => return,
    };
    unsafe {
        let toggle_label = if paused {
            w!("Resume dictation")
        } else {
            w!("Pause dictation")
        };
        let autostart_label = if crate::autostart::is_enabled() {
            w!("Start at login: on")
        } else {
            w!("Start at login: off")
        };

        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, toggle_label);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Open settings"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, w!("Reload dictionary"));
        let _ = AppendMenuW(menu, MF_STRING, ID_REPORT, w!("Latency report"));
        let _ = AppendMenuW(menu, MF_STRING, ID_AUTOSTART, autostart_label);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit Flow"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);

        // Without this the menu is dismissed the moment it appears.
        let _ = SetForegroundWindow(hwnd);

        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );

        // And without this it can linger after a click elsewhere.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        let cmd = match chosen.0 as usize {
            ID_TOGGLE => Some(TrayCommand::Toggle),
            ID_REPORT => Some(TrayCommand::LatencyReport),
            ID_SETTINGS => Some(TrayCommand::OpenSettings),
            ID_RELOAD => Some(TrayCommand::ReloadDictionary),
            ID_AUTOSTART => Some(TrayCommand::ToggleAutostart),
            ID_QUIT => Some(TrayCommand::Quit),
            _ => None,
        };
        if let (Some(cmd), Some(tx)) = (cmd, SENDER.get()) {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(cmd);
            }
        }
    }
}

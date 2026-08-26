//! Notification-area icon and menu.
//!
//! A hidden message-only window receives the icon's callbacks. Commands are
//! queued and polled from the main loop rather than acted on inside the window
//! procedure, so nothing slow ever runs inside a Windows callback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadIconW, RegisterClassW, SetForegroundWindow, TrackPopupMenu, IDI_APPLICATION,
    MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_RIGHTALIGN, TPM_RETURNCMD, WINDOW_EX_STYLE,
    WNDCLASSW, WS_OVERLAPPED,
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

static SENDER: OnceLock<Mutex<Sender<TrayCommand>>> = OnceLock::new();
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

pub struct Tray {
    hwnd: Option<HWND>,
    rx: Option<Receiver<TrayCommand>>,
}

impl Tray {
    /// A tray that does nothing, for when the shell will not cooperate.
    pub fn disabled() -> Tray {
        Tray {
            hwnd: None,
            rx: None,
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

            let icon = LoadIconW(None, IDI_APPLICATION).map_err(|e| e.to_string())?;
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAY,
                hIcon: icon,
                ..Default::default()
            };
            let tip = "Flow: hold Right Ctrl to dictate";
            for (i, c) in tip.encode_utf16().enumerate().take(127) {
                data.szTip[i] = c;
            }
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                let _ = DestroyWindow(hwnd);
                return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
            }

            Ok(Tray {
                hwnd: Some(hwnd),
                rx: Some(rx),
            })
        }
    }

    pub fn poll(&self) -> Option<TrayCommand> {
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

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_LBUTTONUP: u32 = 0x0202;

    if msg == WM_TRAY {
        let event = (lparam.0 as u32) & 0xFFFF;
        if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
            unsafe { show_menu(hwnd) };
            return LRESULT(0);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(m) => m,
        Err(_) => return,
    };
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE, w!("Pause / resume dictation"));
        let _ = AppendMenuW(menu, MF_STRING, ID_REPORT, w!("Latency report"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Open settings"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, w!("Reload dictionary"));
        let autostart_label = if crate::autostart::is_enabled() {
            w!("Start at login: on")
        } else {
            w!("Start at login: off")
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_AUTOSTART, autostart_label);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit Flow"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
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

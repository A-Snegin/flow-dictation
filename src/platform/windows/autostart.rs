//! Start with Windows, off by default and toggled from the tray.
//!
//! A per-user Run entry, not a service and not a scheduled task. Flow needs a
//! desktop session to hook the keyboard and to paste into the focused window,
//! so anything that runs earlier or in another session would be useless.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Flow");

pub fn is_enabled() -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_READ, &mut key) != ERROR_SUCCESS {
            return false;
        }
        let mut size: u32 = 0;
        let present = RegQueryValueExW(key, VALUE_NAME, None, None, None, Some(&mut size))
            == ERROR_SUCCESS;
        let _ = RegCloseKey(key);
        present
    }
}

/// Flips the setting and returns the new state.
pub fn toggle() -> Result<bool, String> {
    if is_enabled() {
        disable()?;
        Ok(false)
    } else {
        enable()?;
        Ok(true)
    }
}

pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // Quoted: the path contains spaces on any normal installation.
    let command = format!("\"{}\"", exe.display());
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let mut key = HKEY::default();
        let status = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_WRITE, &mut key);
        if status != ERROR_SUCCESS {
            return Err(format!("opening the Run key failed: {status:?}"));
        }
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let status = RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(format!("writing the Run value failed: {status:?}"));
        }
    }
    Ok(())
}

pub fn disable() -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let status = RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_WRITE, &mut key);
        if status != ERROR_SUCCESS {
            return Err(format!("opening the Run key failed: {status:?}"));
        }
        let status = RegDeleteValueW(key, VALUE_NAME);
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(format!("deleting the Run value failed: {status:?}"));
        }
    }
    Ok(())
}

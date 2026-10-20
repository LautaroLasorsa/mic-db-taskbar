//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` entry management.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, Result, w};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn enable() -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| {
        windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string())
    })?;
    let command: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: data points to a NUL-terminated UTF-16 string of the given byte length.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            w!("mic-db-taskbar"),
            REG_SZ.0,
            Some(command.as_ptr().cast()),
            (command.len() * size_of::<u16>()) as u32,
        )
        .ok()
    }
}

pub fn disable() -> Result<()> {
    // SAFETY: deletes one named value; a missing value is not an error for us.
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            w!("mic-db-taskbar"),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        status.ok()
    }
}

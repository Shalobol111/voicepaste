//! "Start with Windows": a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Per-user, so no administrator rights are needed.

use std::path::Path;

use anyhow::{bail, Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("VoicePaste");

/// The registry command for `exe`: the path in quotes, so spaces are safe.
pub fn command_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Turns autostart on (pointing at the running exe) or off. Safe to call repeatedly:
/// enabling again refreshes the path, disabling when it is already off is not an error.
pub fn set_enabled(enabled: bool) -> Result<()> {
    let mut key = HKEY::default();
    // SAFETY: `key` is a valid out-pointer and the subkey name is a NUL-terminated literal.
    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key) };
    if status != ERROR_SUCCESS {
        bail!("cannot open the Run registry key (error {})", status.0);
    }
    let result = if enabled {
        write_value(key)
    } else {
        // SAFETY: `key` is the handle opened above; the value name is a NUL-terminated literal.
        let status = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "cannot remove the autostart entry (error {})",
                status.0
            ))
        }
    };
    // SAFETY: closing the handle opened above exactly once.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn write_value(key: HKEY) -> Result<()> {
    let exe = std::env::current_exe().context("cannot find own exe")?;
    let command = command_for(&exe);
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: reinterpreting the UTF-16 buffer as bytes for the duration of the call.
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    // SAFETY: `key` is open with KEY_SET_VALUE; `bytes` includes the terminating NUL.
    let status = unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)) };
    if status != ERROR_SUCCESS {
        bail!("cannot write the autostart entry (error {})", status.0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_the_path() {
        let p = Path::new(r"C:\Program Files\VoicePaste\voicepaste.exe");
        assert_eq!(
            command_for(p),
            r#""C:\Program Files\VoicePaste\voicepaste.exe""#
        );
    }
}

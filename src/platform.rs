//! Small Win32 helpers: single-instance guard and a message box for fatal errors.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONERROR, MB_OK, SW_RESTORE,
};

/// Returns false if another instance already holds the named mutex.
/// The mutex handle is intentionally leaked: it lives as long as the process.
pub fn acquire_single_instance(name: PCWSTR) -> bool {
    // SAFETY: `name` is a valid NUL-terminated wide string for the call duration.
    let created = unsafe { CreateMutexW(None, true, name) };
    // SAFETY: reading the thread's last error right after the call above.
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    created.is_ok() && !already
}

pub fn acquire_main_instance() -> bool {
    acquire_single_instance(w!("Local\\VoicePaste.Main"))
}

/// Shows a modal error box. Used where a console is not visible (GUI build).
pub fn error_box(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated and outlive the (modal) call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("VoicePaste"),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Brings the top-level window with this exact title to the front (used when a second
/// settings window is requested). Does nothing if there is no such window.
pub fn focus_window_by_title(title: &str) {
    let text: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `text` is NUL-terminated and outlives the calls; the handle comes from the OS.
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, PCWSTR(text.as_ptr())) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

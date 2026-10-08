//! Delivers recognized text: clipboard first, then a simulated Ctrl+V into the
//! active window. The clipboard is intentionally never restored.

use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use arboard::Clipboard;

use crate::config::PasteMethod;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_LWIN, VK_MENU,
    VK_RETURN, VK_RWIN, VK_SHIFT, VK_V,
};

const HELD_KEYS: [VIRTUAL_KEY; 5] = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN];

#[derive(Debug, Clone)]
pub struct DeliverOptions {
    /// false = clipboard only.
    pub paste: bool,
    pub method: PasteMethod,
    pub paste_delay: Duration,
    /// Press Enter after pasting (only when something was pasted).
    pub press_enter: bool,
    /// Pause between the paste and Enter, so the target app has taken the text first.
    pub enter_delay: Duration,
}

impl Default for DeliverOptions {
    fn default() -> Self {
        Self {
            paste: true,
            method: PasteMethod::CtrlV,
            paste_delay: Duration::from_millis(40),
            press_enter: false,
            enter_delay: Duration::from_millis(150),
        }
    }
}

/// Blocks for up to ~1.5 s; call it from a short-lived thread, never from the
/// main (event loop) thread.
pub fn deliver(text: &str, opts: &DeliverOptions) -> Result<()> {
    set_clipboard(text)?;
    if !opts.paste {
        return Ok(());
    }
    // The user may still be holding the hotkey modifiers. Pasting now would send
    // Ctrl+Alt+V instead of Ctrl+V.
    if !wait_modifiers_released(Duration::from_secs(1)) {
        tracing::warn!("modifier keys still held after 1 s, pasting anyway");
    }
    thread::sleep(opts.paste_delay);
    match opts.method {
        PasteMethod::CtrlV => send_keys(&[VK_CONTROL, VK_V])?,
        PasteMethod::ShiftInsert => send_keys(&[VK_SHIFT, VK_INSERT])?,
        PasteMethod::Type => type_text(text)?,
    }
    if opts.press_enter {
        thread::sleep(opts.enter_delay);
        send_keys(&[VK_RETURN])?;
    }
    Ok(())
}

fn set_clipboard(text: &str) -> Result<()> {
    // The clipboard may be locked by another process; retry briefly.
    let mut last_err = None;
    for _ in 0..5 {
        match Clipboard::new().and_then(|mut c| c.set_text(text.to_owned())) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    bail!(
        "cannot write to clipboard: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    )
}

fn any_modifier_down() -> bool {
    HELD_KEYS.iter().any(|vk| {
        // SAFETY: GetAsyncKeyState has no preconditions; it only reads key state.
        let state = unsafe { GetAsyncKeyState(vk.0 as i32) };
        (state as u16) & 0x8000 != 0
    })
}

fn wait_modifiers_released(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while any_modifier_down() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

fn key_event(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> Result<()> {
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs and
    // the size argument is the size of one INPUT.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!(
            "SendInput injected {sent} of {} events (target window may run elevated)",
            inputs.len()
        );
    }
    Ok(())
}

/// Presses `keys` in order, then releases them in reverse order, e.g. Ctrl+V.
/// Virtual-key codes (not characters), so this works in any keyboard layout.
fn send_keys(keys: &[VIRTUAL_KEY]) -> Result<()> {
    let down = keys.iter().map(|&k| key_event(k, false));
    let up = keys.iter().rev().map(|&k| key_event(k, true));
    let inputs: Vec<INPUT> = down.chain(up).collect();
    send_inputs(&inputs)
}

/// Types the text character by character (for fields that block pasting).
fn type_text(text: &str) -> Result<()> {
    let mut inputs = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        for up in [false, true] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: KEYEVENTF_UNICODE
                            | if up {
                                KEYEVENTF_KEYUP
                            } else {
                                KEYBD_EVENT_FLAGS(0)
                            },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    send_inputs(&inputs)
}

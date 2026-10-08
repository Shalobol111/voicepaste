//! Small always-on-top status plate ("● ЗАПИСЬ 0:07" / "РАСПОЗНАЮ…").
//!
//! Plain Win32 + GDI, created on the event loop thread (whose message loop
//! pumps it). It must never take focus: the paste goes to whatever window was
//! active when the user pressed the hotkey. Hence WS_EX_NOACTIVATE, click-through
//! (WS_EX_TRANSPARENT), `SW_SHOWNOACTIVATE` and no SetForegroundWindow/SetFocus.

use std::mem::size_of;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, FillRect,
    FrameRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow, SelectObject,
    SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, FF_MODERN, FIXED_PITCH, FW_BOLD, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, MSG, PM_REMOVE,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_PAINT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

/// Layout is designed at 96 dpi and scaled; these are in 96-dpi pixels.
const BASE_W: i32 = 240;
const BASE_H: i32 = 46;
const BOTTOM_MARGIN: i32 = 80;

// Palette of the settings-window mockup.
const BG: (u8, u8, u8) = (29, 29, 29);
const BORDER: (u8, u8, u8) = (207, 207, 207);
const TEXT: (u8, u8, u8) = (242, 242, 242);
const GREEN: (u8, u8, u8) = (127, 179, 86);
const RED: (u8, u8, u8) = (230, 50, 50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Hidden,
    Recording { started: Instant },
    Transcribing { since: Instant },
}

/// The window procedure has no access to `Overlay`, so the view lives here.
static VIEW: Mutex<View> = Mutex::new(View::Hidden);

pub struct Overlay {
    hwnd: HWND,
    view: View,
    /// Last painted second / animation frame, to repaint only when text changes.
    frame: u64,
}

fn rgb((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn frame_of(view: View, now: Instant) -> u64 {
    match view {
        View::Hidden => 0,
        View::Recording { started } => now.saturating_duration_since(started).as_secs(),
        View::Transcribing { since } => {
            now.saturating_duration_since(since).as_millis() as u64 / 400
        }
    }
}

fn label(view: View, now: Instant) -> String {
    match view {
        View::Hidden => String::new(),
        View::Recording { started } => {
            let s = now.saturating_duration_since(started).as_secs();
            format!("ЗАПИСЬ {}:{:02}", s / 60, s % 60)
        }
        View::Transcribing { since } => {
            let dots = (now.saturating_duration_since(since).as_millis() / 400 % 4) as usize;
            format!("РАСПОЗНАЮ{}", ".".repeat(dots))
        }
    }
}

impl Overlay {
    pub fn new() -> Result<Self> {
        // SAFETY: plain Win32 window creation on the calling (event loop) thread; every
        // pointer passed is a valid wide string literal or null where allowed.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).context("GetModuleHandleW failed")?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: w!("VoicePasteOverlay"),
                ..Default::default()
            };
            // Returns 0 if the class already exists; creating the window then still works.
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                w!("VoicePasteOverlay"),
                w!(""),
                WS_POPUP,
                0,
                0,
                BASE_W,
                BASE_H,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .context("cannot create overlay window")?;
            // A layered window is invisible until its alpha is set.
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA)
                .context("cannot set overlay alpha")?;
            hwnd
        };
        Ok(Self {
            hwnd,
            view: View::Hidden,
            frame: 0,
        })
    }

    /// Shows, updates or hides the plate. Cheap when nothing changed.
    pub fn set(&mut self, view: View) {
        let kind_changed = std::mem::discriminant(&view) != std::mem::discriminant(&self.view);
        if view == self.view {
            return;
        }
        self.view = view;
        if let Ok(mut v) = VIEW.lock() {
            *v = view;
        }
        if view == View::Hidden {
            // SAFETY: `hwnd` is the live window created in `new`.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if kind_changed {
            self.place();
        }
        self.frame = frame_of(view, Instant::now());
        self.repaint();
        // SAFETY: as above. SW_SHOWNOACTIVATE shows the window without activating it.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Repaints when the visible text (timer second / animation frame) changed.
    pub fn tick(&mut self, now: Instant) {
        if self.view == View::Hidden {
            return;
        }
        let frame = frame_of(self.view, now);
        if frame != self.frame {
            self.frame = frame;
            self.repaint();
        }
    }

    /// When the next `tick` is needed, so the event loop can sleep until then.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        match self.view {
            View::Hidden => None,
            View::Recording { started } => {
                Some(started + Duration::from_secs(frame_of(self.view, now) + 1))
            }
            View::Transcribing { since } => {
                Some(since + Duration::from_millis((frame_of(self.view, now) + 1) * 400))
            }
        }
    }

    fn repaint(&self) {
        // SAFETY: `hwnd` is live; a null rect invalidates the whole client area.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Bottom centre of the monitor that holds the active window.
    fn place(&self) {
        // SAFETY: all handles come from the OS just above; MONITORINFO is initialised
        // with its size as the API requires.
        unsafe {
            let fg = GetForegroundWindow();
            let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let dpi = if fg.0.is_null() {
                96
            } else {
                GetDpiForWindow(fg).max(96)
            };
            let scale = dpi as f32 / 96.0;
            let (w, h) = (
                (BASE_W as f32 * scale) as i32,
                (BASE_H as f32 * scale) as i32,
            );
            let work = info.rcWork;
            let x = work.left + (work.right - work.left - w) / 2;
            let y = work.bottom - (BOTTOM_MARGIN as f32 * scale) as i32 - h;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: destroying the window this struct created, on the thread that created it.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Debug helper (`--demo-overlay`): shows each state for a few seconds with its own
/// message pump, so the drawing code can be checked without recording anything.
pub fn demo() -> Result<()> {
    let mut overlay = Overlay::new()?;
    for (name, secs) in [("recording", 4), ("transcribing", 3), ("hidden", 1)] {
        println!("overlay: {name}");
        let start = Instant::now();
        overlay.set(match name {
            "recording" => View::Recording { started: start },
            "transcribing" => View::Transcribing { since: start },
            _ => View::Hidden,
        });
        while start.elapsed() < Duration::from_secs(secs) {
            let mut msg = MSG::default();
            // SAFETY: standard message pump on the thread that owns the window.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            overlay.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // SAFETY: called by the OS on the window's own thread with its handle.
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // SAFETY: forwarding unhandled messages with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// # Safety
/// Must be called from WM_PAINT handling with the window's own handle.
unsafe fn paint(hwnd: HWND) {
    let view = VIEW.lock().map(|v| *v).unwrap_or(View::Hidden);
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: BeginPaint/EndPaint are paired; every GDI object created below is deleted
    // before EndPaint, after being deselected where it was selected.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        // Nothing to draw while hidden (an empty text would also crash DrawTextW,
        // which dereferences the dangling pointer of an empty slice).
        if view == View::Hidden {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let scale = (rc.bottom - rc.top).max(1) as f32 / BASE_H as f32;
        let px = |v: f32| (v * scale).round() as i32;

        let bg = CreateSolidBrush(rgb(BG));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg.into());
        let border = CreateSolidBrush(rgb(BORDER));
        FrameRect(hdc, &rc, border);
        let _ = DeleteObject(border.into());

        let (dot, text_color) = match view {
            View::Recording { .. } => (RED, TEXT),
            _ => (GREEN, GREEN),
        };
        let mid = (rc.bottom - rc.top) / 2;
        let r = px(7.0);
        let cx = px(20.0);
        let dot_brush = CreateSolidBrush(rgb(dot));
        let old_brush = SelectObject(hdc, dot_brush.into());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Ellipse(hdc, cx - r, mid - r, cx + r, mid + r);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(dot_brush.into());

        let font = CreateFontW(
            -px(19.0),
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (FIXED_PITCH.0 | FF_MODERN.0) as u32,
            w!("Consolas"),
        );
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(text_color));
        let mut text: Vec<u16> = label(view, Instant::now()).encode_utf16().collect();
        let mut text_rc = RECT {
            left: px(40.0),
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        windows::Win32::Graphics::Gdi::DrawTextW(
            hdc,
            &mut text,
            &mut text_rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(hdc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = EndPaint(hwnd, &ps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_label_shows_minutes_and_seconds() {
        let t = Instant::now();
        let v = View::Recording { started: t };
        assert_eq!(label(v, t), "ЗАПИСЬ 0:00");
        assert_eq!(label(v, t + Duration::from_secs(7)), "ЗАПИСЬ 0:07");
        assert_eq!(label(v, t + Duration::from_secs(125)), "ЗАПИСЬ 2:05");
    }

    #[test]
    fn transcribing_label_animates_dots() {
        let t = Instant::now();
        let v = View::Transcribing { since: t };
        assert_eq!(label(v, t), "РАСПОЗНАЮ");
        assert_eq!(label(v, t + Duration::from_millis(450)), "РАСПОЗНАЮ.");
        assert_eq!(label(v, t + Duration::from_millis(1250)), "РАСПОЗНАЮ...");
        assert_eq!(label(v, t + Duration::from_millis(1650)), "РАСПОЗНАЮ");
    }

    #[test]
    fn frame_changes_once_per_second_while_recording() {
        let t = Instant::now();
        let v = View::Recording { started: t };
        assert_eq!(frame_of(v, t + Duration::from_millis(900)), 0);
        assert_eq!(frame_of(v, t + Duration::from_millis(1100)), 1);
    }
}

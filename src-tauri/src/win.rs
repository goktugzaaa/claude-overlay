//! Small Win32 helpers: never-activating window style and a message box for CLI mode.
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, MessageBoxW, SetWindowLongPtrW, GWL_EXSTYLE, MB_ICONINFORMATION, MB_OK,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    let h = window.hwnd().ok()?;
    Some(h.0 as isize as HWND)
}

/// WS_EX_NOACTIVATE: clicks on the overlay never take focus away from the game or app
/// underneath. WS_EX_TOOLWINDOW keeps it out of Alt+Tab. tao rewrites the extended style
/// from its own flags whenever something like `set_ignore_cursor_events` is called, so this
/// is re-applied after every such call (cheap: it only writes when the bits are missing).
pub fn ensure_noactivate(window: &tauri::WebviewWindow) {
    if let Some(h) = hwnd_of(window) {
        unsafe {
            let style = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let wanted = style | WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize;
            if wanted != style {
                SetWindowLongPtrW(h, GWL_EXSTYLE, wanted);
            }
        }
    }
}

pub fn message_box(title: &str, text: &str) {
    let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let x: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), x.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION);
    }
}

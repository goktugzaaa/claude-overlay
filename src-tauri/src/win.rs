//! Small Win32 helpers: never-activating window style, a message box for CLI mode, process
//! liveness and token elevation.
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, HANDLE, HWND, STILL_ACTIVE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetTickCount};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, MessageBoxW, SetForegroundWindow, SetWindowLongPtrW,
    GWL_EXSTYLE, MB_ICONINFORMATION, MB_OK, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
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

/// Temporarily let the overlay take keyboard focus (free-text answers).
pub fn allow_activate(window: &tauri::WebviewWindow) {
    if let Some(h) = hwnd_of(window) {
        unsafe {
            let style = GetWindowLongPtrW(h, GWL_EXSTYLE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, style & !(WS_EX_NOACTIVATE as isize));
            SetForegroundWindow(h);
        }
    }
}

pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

pub fn set_foreground_raw(hwnd: isize) {
    if hwnd != 0 {
        unsafe {
            SetForegroundWindow(hwnd as HWND);
        }
    }
}

/// Milliseconds since the last keyboard or mouse input on this PC.
pub fn idle_ms() -> u64 {
    unsafe {
        let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        GetTickCount().wrapping_sub(info.dwTime) as u64
    }
}

pub fn message_box(title: &str, text: &str) {
    let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let x: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), x.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION);
    }
}

/// The Windows system directory (System32), from the OS rather than PATH or the exe folder.
pub fn system_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n > 0 && n < buf.len() {
        return PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
    }
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32")
}

/// True while process `pid` is running. A process we may not query (another account, a
/// protected process) counts as running; only a pid that no longer exists, or one that has
/// exited, counts as gone.
pub fn process_alive(pid: u32) -> bool {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        !ok || code == STILL_ACTIVE as u32
    }
}

/// True when the overlay runs with an elevated (Administrator) token. Cached: elevation
/// cannot change during the life of a process.
pub fn is_elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(|| unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        ) != 0;
        CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    })
}

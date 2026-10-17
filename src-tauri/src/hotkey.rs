//! The global "hide everything" shortcut (hotkeys.hide, default Ctrl+Alt+F9; empty = off). It
//! is registered with RegisterHotKey on a thread of its own, which runs the message loop the
//! hotkey is posted to and registers it again when the config is reloaded. The Settings window
//! shows whether that worked: another app may own the same shortcut.
use crate::{config, events, util, Shared};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY, WM_USER,
};

const HOTKEY_ID: i32 = 1;
/// Posted to the hotkey thread: read hotkeys.hide again.
const WM_RELOAD: u32 = WM_APP + 1;
/// RegisterHotKey's error when another app already has the shortcut.
const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

/// The hotkey thread's id, once its message queue exists (0 before).
static THREAD: AtomicU32 = AtomicU32::new(0);
/// Bumped after every registration attempt: a reload waits for it.
static ROUNDS: AtomicU64 = AtomicU64::new(0);
static STATUS: Mutex<Status> = Mutex::new(Status { state: "off", key: String::new() });

/// What became of hotkeys.hide. `state`: "on" (registered), "off" (empty), "invalid" (not a
/// shortcut HeadsUp accepts, from a hand-edited config.json), "taken" (another app has it) or
/// "failed" (Windows refused it for another reason). `key`: the shortcut as configured, in its
/// canonical spelling when it parses.
#[derive(Clone, Debug, PartialEq)]
struct Status {
    state: &'static str,
    key: String,
}

/// A shortcut: RegisterHotKey's modifier flags and virtual-key code, and how it is written.
#[derive(Debug, PartialEq)]
pub struct Shortcut {
    pub mods: u32,
    pub vk: u32,
    pub name: String,
}

/// Named keys: (spellings, virtual-key code, canonical spelling).
const NAMED: &[(&[&str], u32, &str)] = &[
    (&["space"], 0x20, "Space"),
    (&["enter", "return"], 0x0D, "Enter"),
    (&["tab"], 0x09, "Tab"),
    (&["esc", "escape"], 0x1B, "Esc"),
    (&["backspace"], 0x08, "Backspace"),
    (&["insert", "ins"], 0x2D, "Insert"),
    (&["delete", "del"], 0x2E, "Delete"),
    (&["home"], 0x24, "Home"),
    (&["end"], 0x23, "End"),
    (&["pageup", "pgup"], 0x21, "PageUp"),
    (&["pagedown", "pgdn"], 0x22, "PageDown"),
    (&["up"], 0x26, "Up"),
    (&["down"], 0x28, "Down"),
    (&["left"], 0x25, "Left"),
    (&["right"], 0x27, "Right"),
    (&["pause"], 0x13, "Pause"),
];

/// The key of a shortcut (lower case): (virtual-key code, canonical spelling, is F1–F24).
fn key(part: &str) -> Option<(u32, String, bool)> {
    if part.chars().count() == 1 {
        // Letters and digits: their virtual-key codes are their upper-case ASCII codes.
        let c = part.chars().next()?.to_ascii_uppercase();
        return c.is_ascii_alphanumeric().then(|| (c as u32, c.to_string(), false));
    }
    if let Some(n) = part.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        // No "F01": only the plain spelling.
        if part == format!("f{n}") {
            return Some((0x70 + n - 1, format!("F{n}"), true));
        }
    }
    for prefix in ["numpad", "num"] {
        if let Some(d) = part.strip_prefix(prefix).filter(|d| d.len() == 1).and_then(|d| d.parse::<u32>().ok()) {
            return Some((0x60 + d, format!("Num{d}"), false));
        }
    }
    NAMED
        .iter()
        .find(|(names, _, _)| names.contains(&part))
        .map(|(_, vk, name)| (*vk, name.to_string(), false))
}

/// Parse a shortcut such as "Ctrl+Shift+H" or "Ctrl+Alt+F9": parts joined by "+", any case,
/// spaces around them ignored, in any order. Ctrl (Control), Alt, Shift and Win (Windows) at
/// most once each, and exactly one key: A–Z, 0–9, F1–F24, Num0–Num9, Space, Enter, Tab, Esc,
/// Backspace, Insert, Delete, Home, End, PageUp, PageDown, Up, Down, Left, Right or Pause. A
/// key needs Ctrl, Alt or Win, except F13–F24, which also work with Shift alone or by
/// themselves: a shortcut takes its keys away from every other app, and a lone letter (or
/// Shift and a letter, or F1–F12, which apps use every day) would stop working anywhere else.
/// For the same reason Ctrl+Alt with a letter or digit (AltGr, which types @ { [ on many
/// layouts) needs Win too, and shortcuts Windows itself uses (`reserved`) are refused.
/// Ok(None): empty, the shortcut is off. The canonical spelling is Ctrl+Alt+Shift+Win+Key.
pub fn parse(s: &str) -> Result<Option<Shortcut>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let example = "expected modifiers and one key, such as Ctrl+Shift+H";
    let (mut mods, mut found) = (0u32, None);
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        let flag = match part.as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" => MOD_WIN,
            _ => 0,
        };
        if flag != 0 {
            if mods & flag != 0 {
                return Err(format!("{} appears twice", util::truncate(&part, 20)));
            }
            mods |= flag;
            continue;
        }
        if part.is_empty() {
            return Err(example.to_string());
        }
        let Some(k) = key(&part) else { return Err(format!("unknown key {}", util::truncate(&part, 20))) };
        if found.is_some() {
            return Err(example.to_string());
        }
        found = Some(k);
    }
    let Some((vk, key_name, function_key)) = found else { return Err(example.to_string()) };
    // F13-F24: on no keyboard's main rows, so no app misses them.
    let lone_ok = function_key && vk >= 0x7C;
    if !lone_ok && mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) == 0 {
        return Err("needs Ctrl, Alt or Win (F13-F24 also work alone)".to_string());
    }
    let alnum = matches!(vk, 0x30..=0x39 | 0x41..=0x5A);
    if alnum && mods & (MOD_CONTROL | MOD_ALT) == (MOD_CONTROL | MOD_ALT) && mods & MOD_WIN == 0 {
        return Err("Ctrl+Alt with a letter or digit is AltGr on many keyboards; add Win or use an F key".to_string());
    }
    if reserved(mods, vk) {
        return Err("reserved by Windows".to_string());
    }
    let mut name: Vec<&str> = [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")]
        .iter()
        .filter(|(flag, _)| mods & flag != 0)
        .map(|(_, n)| *n)
        .collect();
    name.push(&key_name);
    Ok(Some(Shortcut { mods, vk, name: name.join("+") }))
}

/// Shortcuts Windows already uses: Alt+F4, Alt+Tab, Alt+Esc, Alt+Space, Alt+Enter, Ctrl+Esc and
/// Ctrl+Shift+Esc (Tab and Esc with or without Shift). RegisterHotKey may take some of them,
/// and then they stop working in every app.
fn reserved(mods: u32, vk: u32) -> bool {
    let no_shift = mods & !MOD_SHIFT;
    match vk {
        0x09 => no_shift == MOD_ALT,
        0x1B => no_shift == MOD_ALT || no_shift == MOD_CONTROL,
        0x73 | 0x20 | 0x0D => mods == MOD_ALT,
        _ => false,
    }
}

/// Register hotkeys.hide on the calling thread (the hotkey thread). True when it is registered.
fn register(shared: &Shared) -> bool {
    let wanted = config::get_str(&shared.cfg(), "hotkeys.hide").unwrap_or_default();
    let status = match parse(&wanted) {
        Ok(None) => Status { state: "off", key: String::new() },
        Err(_) => Status { state: "invalid", key: util::truncate(wanted.trim(), 40) },
        Ok(Some(k)) => {
            // MOD_NOREPEAT: holding the keys down hides once, not over and over.
            if unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, k.mods | MOD_NOREPEAT, k.vk) } != 0 {
                Status { state: "on", key: k.name }
            } else if unsafe { GetLastError() } == ERROR_HOTKEY_ALREADY_REGISTERED {
                Status { state: "taken", key: k.name }
            } else {
                Status { state: "failed", key: k.name }
            }
        }
    };
    let on = status.state == "on";
    let mut current = STATUS.lock().unwrap();
    // Logged when it changes: a reload with the same (failing) shortcut adds nothing.
    if *current != status {
        let tag = if matches!(status.state, "on" | "off") { "hotkey" } else { "hotkey-error" };
        util::log_line(&[tag, status.state, &status.key]);
        *current = status;
    }
    on
}

/// Start the hotkey thread: it registers hotkeys.hide and hides everything when it is pressed.
pub fn start(app: AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        // SAFETY: MSG is plain data, and every call below gets a valid pointer to it.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        unsafe {
            // A thread's message queue comes with its first window-manager call: create it now,
            // so no reload posted from here on is lost.
            PeekMessageW(&mut msg, std::ptr::null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        }
        let mut registered = register(&shared);
        ROUNDS.fetch_add(1, Ordering::SeqCst);
        // GetMessageW returns 0 for WM_QUIT and -1 on an error.
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            match msg.message {
                WM_HOTKEY if msg.wParam == HOTKEY_ID as usize => events::hide_all(&app, &shared),
                WM_RELOAD => {
                    if registered {
                        unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
                    }
                    registered = register(&shared);
                    ROUNDS.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    });
}

/// The config was reloaded: register hotkeys.hide again. Waits a moment for the result, so the
/// Settings window shows it right away.
pub fn reload() {
    let thread = THREAD.load(Ordering::SeqCst);
    if thread == 0 {
        return; // not started yet: it reads the config when it starts
    }
    let round = ROUNDS.load(Ordering::SeqCst);
    if unsafe { PostThreadMessageW(thread, WM_RELOAD, 0, 0) } == 0 {
        return;
    }
    let until = Instant::now() + Duration::from_millis(500);
    while ROUNDS.load(Ordering::SeqCst) == round && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The shortcut that hides everything, while it is registered.
pub fn active() -> Option<String> {
    let s = STATUS.lock().unwrap();
    (s.state == "on").then(|| s.key.clone())
}

/// For the Settings window: {"state": "on" | "off" | "invalid" | "taken" | "failed", "key"}.
pub fn status_json() -> Value {
    let s = STATUS.lock().unwrap();
    json!({"state": s.state, "key": s.key})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> (u32, u32, String) {
        let k = parse(s).unwrap().unwrap();
        (k.mods, k.vk, k.name)
    }

    #[test]
    fn shortcuts_parse_to_canonical_form() {
        assert_eq!(ok("Ctrl+Shift+H"), (MOD_CONTROL | MOD_SHIFT, 0x48, "Ctrl+Shift+H".to_string()));
        assert_eq!(ok(" shift + control + h "), (MOD_CONTROL | MOD_SHIFT, 0x48, "Ctrl+Shift+H".to_string()));
        assert_eq!(ok("ctrl+alt+f9"), (MOD_CONTROL | MOD_ALT, 0x78, "Ctrl+Alt+F9".to_string()));
        assert_eq!(ok("Win+Alt+5"), (MOD_ALT | MOD_WIN, 0x35, "Alt+Win+5".to_string()));
        assert_eq!(ok("Windows+PgDn"), (MOD_WIN, 0x22, "Win+PageDown".to_string()));
        assert_eq!(ok("Ctrl+Numpad7"), (MOD_CONTROL, 0x67, "Ctrl+Num7".to_string()));
        assert_eq!(ok("Ctrl+Alt+Escape"), (MOD_CONTROL | MOD_ALT, 0x1B, "Ctrl+Alt+Esc".to_string()));
        // F13-F24 also work alone or with Shift only; F1-F12 with Ctrl, Alt or Win.
        assert_eq!(ok("F13"), (0, 0x7C, "F13".to_string()));
        assert_eq!(ok("Shift+F24"), (MOD_SHIFT, 0x87, "Shift+F24".to_string()));
        assert_eq!(ok("Ctrl+F5"), (MOD_CONTROL, 0x74, "Ctrl+F5".to_string()));
        // Ctrl+Alt with a letter needs Win; Alt+Shift+F4 is not Alt+F4.
        assert_eq!(ok("Ctrl+Alt+Win+Q"), (MOD_CONTROL | MOD_ALT | MOD_WIN, 0x51, "Ctrl+Alt+Win+Q".to_string()));
        assert_eq!(ok("Alt+Shift+F4"), (MOD_ALT | MOD_SHIFT, 0x73, "Alt+Shift+F4".to_string()));
        assert_eq!(ok("H+Ctrl"), (MOD_CONTROL, 0x48, "Ctrl+H".to_string()));
        // Empty: off.
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("   ").unwrap(), None);
    }

    #[test]
    fn shortcuts_refused() {
        for bad in [
            "H",            // no modifier
            "Shift+H",      // Shift alone would take a capital letter away from typing
            "Ctrl+Shift",   // no key
            "Ctrl+H+J",     // two keys
            "Ctrl+Ctrl+H",  // a modifier twice
            "Ctrl++H",      // empty part
            "Ctrl+H+",
            "Ctrl+F25",
            "Ctrl+F01",
            "Ctrl+Ş",       // not a key HeadsUp knows
            "Ctrl+Plus",
            "Ctrl+Num10",
            "F5",           // F1-F12 alone or with Shift only: every app uses them
            "Shift+F5",
            "Ctrl+Alt+Q",   // AltGr+Q (@ on Turkish Q)
            "Ctrl+Alt+3",
            "Ctrl+Alt+Shift+7",
            "Alt+F4",       // reserved by Windows
            "Alt+Tab",
            "Alt+Shift+Tab",
            "Alt+Escape",
            "Alt+Space",
            "Alt+Enter",
            "Ctrl+Esc",
            "Ctrl+Shift+Esc",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}

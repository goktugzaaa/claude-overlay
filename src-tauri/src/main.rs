// Claude Overlay — always-on-top, click-through notification layer for the Claude desktop
// app (Chat, Cowork, Code) and Claude Code CLI. Tauri 2 / WebView2 edition.
//
// Signals:
//   1. Windows notification center (UserNotificationListener via a PowerShell bridge):
//      every toast the Claude app emits is mirrored as an overlay banner.
//   2. Claude Code hooks (hooks/claude-overlay-send.ps1): Stop, PermissionRequest and
//      AskUserQuestion (both answerable from the banner), Notification.
//   3. Claude Code status line (rate_limits): limit-reset timers.
//
//   4. Remote sessions (remote.rs): the paired Telegram chat can start or continue Claude
//      Code sessions; the overlay runs `claude -p` headless and relays the result.
//
// CLI: ClaudeOverlay.exe [--install | --uninstall | --send <type>]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod events;
mod hooks;
mod http;
mod i18n;
mod installer;
mod phone;
mod remote;
mod telegram;
mod tray;
mod util;
mod win;

use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// A question / permission request whose hook is waiting for a click on the overlay.
pub struct Pending {
    pub req: tiny_http::Request,
    pub kind: String,
    pub event: Value,
    /// Unix ms; the hook gets an empty reply when this passes. Typing an answer extends it.
    pub deadline_ms: i64,
    /// A question card answered through a PermissionRequest hook (the PreToolUse hook was
    /// cancelled or skipped): the answer is sent back in PermissionRequest form.
    pub via_permission: bool,
    /// Answers collected so far when a multi-question card is answered on the phone, one
    /// question at a time (question text -> answer).
    pub partial: serde_json::Map<String, Value>,
}

/// Clickable area of an interactive card, in CSS pixels of the overlay page.
#[derive(Deserialize, Clone, Debug)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

pub struct Shared {
    pub port: u16,
    pub config: Mutex<Value>,
    pub lang: Mutex<&'static i18n::Strings>,
    pub lang_code: Mutex<&'static str>,
    pub state: Mutex<Value>,
    pub pending: Mutex<HashMap<String, Pending>>,
    pub seq: AtomicU64,
    pub hot_rects: Mutex<Vec<Rect>>,
    pub capturing: AtomicBool,
    pub active_items: AtomicI64,
    pub last_hook_banner_ms: AtomicI64,
    pub last_permission_ms: AtomicI64,
    pub foreground: Mutex<String>,
    pub listener_status: Mutex<String>,
    pub listener_child: Mutex<Option<std::process::Child>>,
    pub tray: Mutex<Option<tauri::tray::TrayIcon>>,
    pub quitting: AtomicBool,
    /// True while a free-text answer is being typed: the overlay then holds keyboard focus.
    pub typing: AtomicBool,
    pub prev_foreground: AtomicIsize,
    /// Telegram message_id -> pending card id, for edits and typed replies.
    pub tg_msgs: Mutex<HashMap<i64, (String, String)>>,
    pub tg_last_pending: Mutex<Option<String>>,
    /// When the newest card reached the phone (unix ms); compared with remote activity to
    /// decide whether plain text answers the card or goes to the remote session.
    pub tg_last_pending_ms: AtomicI64,
    /// Every Telegram message id that ever carried a card, open or closed.
    pub tg_card_msgs: Mutex<HashSet<i64>>,
    /// session id -> when its PreToolUse question card expired unanswered (unix ms), so the
    /// PermissionRequest that follows does not bring the same question back as a new card.
    pub expired_questions: Mutex<HashMap<String, i64>>,
    /// (card id, question index) after "Other…" was tapped on the phone: the next text
    /// message is the typed answer to that question.
    pub tg_await_text: Mutex<Option<(String, usize)>>,
    /// Force-reply prompt message id -> (card id, question index) it asked about.
    pub tg_prompt_msgs: Mutex<HashMap<i64, (String, usize)>>,
    /// Bumped per non-interactive phone message; a deferred one only goes out if still newest.
    pub phone_generation: AtomicU64,
    /// Remote sessions driven from the phone (see remote.rs).
    pub remote: Mutex<remote::RemoteState>,
    /// Session ids of remote runs: their cards go to the phone at once, their Stop is not relayed.
    pub remote_sessions: Mutex<HashSet<String>>,
}

impl Shared {
    pub fn t(&self) -> &'static i18n::Strings {
        *self.lang.lock().unwrap()
    }

    pub fn cfg(&self) -> Value {
        self.config.lock().unwrap().clone()
    }

    pub fn reload_config(&self) {
        let c = config::load();
        let (code, strings) = i18n::pick(config::get_str(&c, "language").as_deref(), &util::locale());
        *self.lang.lock().unwrap() = strings;
        *self.lang_code.lock().unwrap() = code;
        *self.config.lock().unwrap() = c;
        // Language or remote switch may have changed: refresh the bot's command menu.
        self.remote.lock().unwrap().commands_registered = false;
    }
}

#[tauri::command]
fn item_done(shared: State<'_, Arc<Shared>>) {
    if shared.active_items.fetch_sub(1, Ordering::SeqCst) <= 1 {
        shared.active_items.store(0, Ordering::SeqCst);
    }
}

#[tauri::command]
fn rlog(message: String) {
    util::log_line(&["renderer", &util::truncate(&message, 500)]);
}

#[tauri::command]
fn hot_rects(rects: Vec<Rect>, shared: State<'_, Arc<Shared>>) {
    *shared.hot_rects.lock().unwrap() = rects
        .into_iter()
        .filter(|r| r.right > r.left && r.bottom > r.top)
        .collect();
}

#[tauri::command]
fn answer(id: String, data: Value, app: AppHandle, shared: State<'_, Arc<Shared>>) {
    events::finish_interactive(&app, &shared, &id, Some(data));
}

/// Keep a pending card alive longer (the user opened the free-text field).
#[tauri::command]
fn extend_wait(id: String, ms: u64, shared: State<'_, Arc<Shared>>) {
    if let Some(p) = shared.pending.lock().unwrap().get_mut(&id) {
        p.deadline_ms = p.deadline_ms.max(util::now_ms() + ms as i64);
    }
}

/// Take keyboard focus for the free-text field (true) and hand it back to the window that
/// had it (false). Only while typing does the overlay ever become the active window.
#[tauri::command]
fn focus_input(active: bool, app: AppHandle, shared: State<'_, Arc<Shared>>) {
    let Some(w) = events::window(&app) else { return };
    if active {
        if !shared.typing.swap(true, Ordering::SeqCst) {
            shared.prev_foreground.store(win::foreground_hwnd(), Ordering::SeqCst);
        }
        win::allow_activate(&w);
        let _ = w.set_focus();
    } else if shared.typing.swap(false, Ordering::SeqCst) {
        win::ensure_noactivate(&w);
        win::set_foreground_raw(shared.prev_foreground.swap(0, Ordering::SeqCst));
    }
}

#[tauri::command]
fn codex_toggle(app: AppHandle, shared: State<'_, Arc<Shared>>) -> bool {
    let t = shared.t();
    let enabled = if installer::codex_installed() {
        installer::uninstall_codex();
        events::notify(&app, &shared, "Claude Overlay", t.codex_disabled, "", "info", None);
        false
    } else {
        match installer::install_codex() {
            Ok(_) => {
                events::notify(&app, &shared, "Claude Overlay", t.codex_enabled, t.codex_enabled_body, "done", None);
                true
            }
            Err(e) => {
                events::notify(&app, &shared, "Claude Overlay", t.error, &e, "permission", None);
                false
            }
        }
    };
    tray::rebuild_menu(&app, &shared);
    enabled
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    config::ensure_user_data();
    hooks::write_scripts();
    let cfg = config::load();
    let port: u16 = std::env::var("CLAUDE_OVERLAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(config::get_u64(&cfg, "port", 47821) as u16);

    if args.iter().any(|a| a == "--install") {
        let text = match installer::install() {
            Ok(lines) => lines.join("\n"),
            Err(e) => format!("Error: {e}"),
        };
        let _ = http::post(port, "reload-config", "{}", 1500);
        let _ = http::post(port, "test-notify", "{}", 1500);
        win::message_box("Claude Overlay installed", &text);
        return;
    }
    if args.iter().any(|a| a == "--install-codex") {
        let text = match installer::install_codex() { Ok(m) => m, Err(e) => format!("Error: {e}") };
        win::message_box("Codex CLI integration", &text);
        return;
    }
    if args.iter().any(|a| a == "--uninstall-codex") {
        win::message_box("Codex CLI integration", &installer::uninstall_codex());
        return;
    }
    if args.iter().any(|a| a == "--uninstall") {
        let lines = installer::uninstall();
        let _ = http::post(port, "quit", "{}", 1500);
        win::message_box("Claude Overlay removed", &lines.join("\n"));
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--send") {
        let kind = args.get(i + 1).cloned().unwrap_or_else(|| "test-notify".to_string());
        if let Err(e) = http::post(port, &kind, "{}", 2500) {
            win::message_box("Overlay did not respond", &e);
        }
        return;
    }

    // Binding the port doubles as the single-instance lock: a second copy just pings the
    // running one and exits.
    let server = match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(_) => {
            let _ = http::post(port, "test-notify", "{}", 1500);
            return;
        }
    };

    let (code, strings) = i18n::pick(config::get_str(&cfg, "language").as_deref(), &util::locale());
    let shared = Arc::new(Shared {
        port,
        config: Mutex::new(cfg),
        lang: Mutex::new(strings),
        lang_code: Mutex::new(code),
        state: Mutex::new(events::load_state()),
        pending: Mutex::new(HashMap::new()),
        // Card ids are part of Telegram button data: start from the clock so a button left over
        // from before a restart can never match a new card.
        seq: AtomicU64::new(util::now_ms().max(0) as u64),
        hot_rects: Mutex::new(Vec::new()),
        capturing: AtomicBool::new(false),
        active_items: AtomicI64::new(0),
        last_hook_banner_ms: AtomicI64::new(0),
        last_permission_ms: AtomicI64::new(0),
        foreground: Mutex::new(String::new()),
        listener_status: Mutex::new("starting".to_string()),
        listener_child: Mutex::new(None),
        tray: Mutex::new(None),
        quitting: AtomicBool::new(false),
        typing: AtomicBool::new(false),
        prev_foreground: AtomicIsize::new(0),
        tg_msgs: Mutex::new(HashMap::new()),
        tg_last_pending: Mutex::new(None),
        tg_last_pending_ms: AtomicI64::new(0),
        tg_card_msgs: Mutex::new(HashSet::new()),
        expired_questions: Mutex::new(HashMap::new()),
        tg_await_text: Mutex::new(None),
        tg_prompt_msgs: Mutex::new(HashMap::new()),
        phone_generation: AtomicU64::new(0),
        remote: Mutex::new(remote::RemoteState::new()),
        remote_sessions: Mutex::new(HashSet::new()),
    });

    let setup_shared = shared.clone();
    tauri::Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![item_done, rlog, hot_rects, answer, extend_wait, focus_input, codex_toggle])
        .setup(move |app| {
            let handle = app.handle().clone();
            let window = handle.get_webview_window("main").ok_or("main window missing")?;
            win::ensure_noactivate(&window);
            events::place_on_cursor_monitor(&handle, true);
            let _ = window.set_ignore_cursor_events(true);
            let _ = window.set_always_on_top(true);
            // The window stays visible (fully transparent) while idle: hiding and re-showing
            // a transparent window on Windows stops clicks from reaching the page.
            let _ = window.show();
            win::ensure_noactivate(&window);
            {
                let w = window.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    win::ensure_noactivate(&w);
                });
            }
            http::serve(server, handle.clone(), setup_shared.clone());
            tray::build(&handle, &setup_shared)?;
            events::start_limit_timer(handle.clone(), setup_shared.clone());
            events::start_toast_listener(handle.clone(), setup_shared.clone());
            events::start_cursor_poll(handle.clone(), setup_shared.clone());
            telegram::start(handle.clone(), setup_shared.clone());
            events::first_run(handle, setup_shared);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Claude Overlay");
}

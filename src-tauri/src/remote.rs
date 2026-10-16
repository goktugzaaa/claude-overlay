//! Remote sessions from the phone. Over the paired Telegram chat the user can start a new
//! Claude Code session in a project the PC already knows, or send a message to an existing
//! session. The overlay runs `claude -p` headless (stream-json), relays progress and the
//! result to Telegram, and the existing hooks send permission requests to the phone.
//!
//! Safety: off by default (`remote.enabled`) and only usable once a chat is paired; only the
//! paired chat is obeyed; only projects that already have Claude Code transcripts (or are
//! listed in `remote.projects`) can be used and the working directory always comes from that
//! list, never from message text; the prompt is passed on stdin, never through a shell; the
//! permission mode is limited to modes that still ask; one run at a time with a watchdog.
use crate::{config, events, telegram, util, Shared};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;

const ALLOWED_MODES: [&str; 4] = ["default", "acceptEdits", "plan", "auto"];
/// Rate-limit windows that the limit-reset timers know how to label.
const KNOWN_WINDOWS: [&str; 2] = ["five_hour", "seven_day"];
/// Telegram allows 4096 characters; leave room for markup added around a chunk.
const CHUNK_MAX: usize = 3300;

#[derive(Clone, Debug)]
pub struct Project {
    pub name: String,
    pub cwd: String,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub project: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub enum Target {
    New { cwd: String, project: String },
    Resume { id: String, cwd: String, project: String, title: String },
}

pub struct Running {
    pub pid: u32,
    pub seq: u64,
    pub session_id: Option<String>,
    pub project: String,
    pub started_ms: i64,
    pub stop_requested: bool,
    /// The result has been sent; the process may still be finishing background work.
    pub result_sent: bool,
    pub activity: String,
    pub status_mid: Option<i64>,
    /// The claude process has exited (its pid may be reused: never kill it again).
    pub exited: bool,
}

#[derive(Default)]
pub struct RemoteState {
    pub target: Option<Target>,
    /// Bumped whenever the user changes the target (/new pick, /sessions pick, /exit).
    pub target_gen: u64,
    pub projects_menu: Vec<Project>,
    pub sessions_menu: Vec<SessionInfo>,
    /// Bumped per menu sent; buttons carry it so taps on an old menu are refused.
    pub menu_gen: u64,
    pub running: Option<Running>,
    /// Prompt held back by the busy guard until the user confirms or cancels: (gen, target, text).
    pub held: Option<(u64, Target, String)>,
    pub held_gen: u64,
    /// session id -> unix ms when our own run on it ended (so the busy guard ignores it).
    pub own_runs: HashMap<String, i64>,
    /// session id -> seq of the newest run on it (so an old cleanup never removes a newer run).
    pub session_runs: HashMap<String, u64>,
    pub run_seq: u64,
    pub commands_registered: bool,
    /// This process set the bot's command menu (so it may delete it again).
    pub commands_set: bool,
    /// Turned off from the tray while config.json could not be written: stay off.
    pub force_off: bool,
    /// Last remote interaction (target picked, run started or ended), unix ms.
    pub last_touch_ms: i64,
    /// Telegram message ids sent by remote sessions (a reply to one of them is a prompt).
    pub msg_ids: HashSet<i64>,
}

impl RemoteState {
    /// Generations start from the clock so buttons from before a restart never match.
    pub fn new() -> Self {
        let seed = util::now_ms().max(0) as u64;
        RemoteState { menu_gen: seed, held_gen: seed, ..Default::default() }
    }
}

pub enum Toggle {
    On,
    Off,
    /// Off for this session, but config.json could not be saved.
    OffNotSaved,
    NeedPairing,
    ConfigUnreadable,
}

// ---------------------------------------------------------------- strings

pub struct S {
    pub disabled: &'static str,
    pub pair_first: &'static str,
    pub help: &'static str,
    pub pick_project: &'static str,
    pub no_projects: &'static str,
    pub project_chosen: &'static str,
    pub pick_session: &'static str,
    pub no_sessions: &'static str,
    pub session_chosen: &'static str,
    pub no_target: &'static str,
    pub busy: &'static str,
    pub finishing: &'static str,
    pub working: &'static str,
    pub done: &'static str,
    pub failed: &'static str,
    pub stopped: &'static str,
    pub nothing_running: &'static str,
    pub active_warn: &'static str,
    pub send_anyway: &'static str,
    pub cancel: &'static str,
    pub cancelled: &'static str,
    pub expired: &'static str,
    pub claude_missing: &'static str,
    pub cleared: &'static str,
    pub model_now: &'static str,
    pub model_default: &'static str,
    pub model_invalid: &'static str,
    pub config_unreadable: &'static str,
    pub off_not_saved: &'static str,
    pub reply_hint: &'static str,
    pub status_running: &'static str,
    pub status_target_new: &'static str,
    pub status_target_resume: &'static str,
    pub status_idle: &'static str,
    pub timeout: &'static str,
    pub truncated: &'static str,
    pub turns: &'static str,
    pub tray_enable: &'static str,
    pub tray_disable: &'static str,
    pub enabled_title: &'static str,
    pub enabled_body: &'static str,
    pub disabled_title: &'static str,
    pub cmd_new: &'static str,
    pub cmd_sessions: &'static str,
    pub cmd_stop: &'static str,
    pub cmd_status: &'static str,
    pub cmd_exit: &'static str,
    pub cmd_model: &'static str,
    pub cmd_help: &'static str,
}

static EN: S = S {
    disabled: "Remote sessions are off. Turn them on from the tray menu: Phone: remote sessions.",
    pair_first: "Pair the Telegram chat first: send /start to your bot, then turn remote sessions on.",
    help: "<b>Remote sessions</b>\n/new: start a new session in a project\n/sessions: pick a recent session; your next message goes there\n/status: what is running\n/stop: stop the running task\n/exit: clear the selected session\n/model [name]: model for remote runs (/model default = your default)\n\nAfter a run, just write to continue the same session. A question or permission card is answered with its buttons, or by replying to that card.",
    pick_project: "Which project? A new session starts there.",
    no_projects: "No known projects yet: open a project in Claude Code on the PC once.",
    project_chosen: "📁 <b>{p}</b>: new session. Send the task.",
    pick_session: "Recent sessions. Your next message goes to the one you pick.",
    no_sessions: "No sessions found.",
    session_chosen: "💬 <b>{p}</b> · {t}\nSend your message.",
    no_target: "No session selected. /new starts one, /sessions picks an existing one.",
    busy: "A remote task is still running. /status shows it, /stop stops it.",
    finishing: "The last task answered but is still finishing background work. Wait a moment or /stop it.",
    working: "⏳ <b>{p}</b> · working…",
    done: "✅ <b>{p}</b> · done",
    failed: "❌ <b>{p}</b> · failed",
    stopped: "⏹ Stopped.",
    nothing_running: "Nothing is running.",
    active_warn: "⚠️ This session changed {s} s ago, so it may be open on the PC. Sending now would mix the two conversations.",
    send_anyway: "Send anyway",
    cancel: "Cancel",
    cancelled: "Cancelled.",
    expired: "That menu is out of date. Send the command again.",
    claude_missing: "Claude Code CLI not found. Set remote.claudePath in config.json.",
    cleared: "Selection cleared.",
    model_now: "Model for remote runs: <b>{m}</b>",
    model_default: "Model for remote runs: your Claude Code default.",
    model_invalid: "Not a valid model name.",
    config_unreadable: "config.json could not be read, so it was not changed. Fix the file and try again.",
    off_not_saved: "Remote sessions are off until the overlay restarts. config.json could not be saved, so fix it to keep them off.",
    reply_hint: "Write to continue this session · /new for a new one",
    status_running: "▶️ <b>{p}</b> · running for {s} s",
    status_target_new: "Next message starts a new session in <b>{p}</b>.",
    status_target_resume: "Next message goes to <b>{p}</b> · {t}",
    status_idle: "Idle. /new or /sessions.",
    timeout: "⏱ Stopped after {m} minutes.",
    truncated: "… (truncated)",
    turns: "turns",
    tray_enable: "Phone: remote sessions (off)",
    tray_disable: "Phone: remote sessions (on)",
    enabled_title: "Remote sessions on",
    enabled_body: "Your paired Telegram chat can now start and continue Claude Code sessions on this PC.",
    disabled_title: "Remote sessions off",
    cmd_new: "Start a new session in a project",
    cmd_sessions: "Continue a recent session",
    cmd_stop: "Stop the running task",
    cmd_status: "What is running",
    cmd_exit: "Clear the selected session",
    cmd_model: "Model for remote runs",
    cmd_help: "Help",
};

static TR: S = S {
    disabled: "Uzaktan oturum kapalı. Tepsi menüsünden aç: Telefon: uzaktan oturum.",
    pair_first: "Önce Telegram sohbetini eşleştir: botuna /start gönder, sonra uzaktan oturumu aç.",
    help: "<b>Uzaktan oturum</b>\n/new: bir projede yeni oturum başlat\n/sessions: son oturumlardan birini seç; sonraki mesajın oraya gider\n/status: ne çalışıyor\n/stop: çalışan görevi durdur\n/exit: seçili oturumu bırak\n/model [ad]: uzaktan çalıştırma modeli (/model default = varsayılanın)\n\nBir çalıştırma bitince yazman yeterli, aynı oturum devam eder. Soru ve izin kartları kendi butonlarıyla ya da o karta yanıt vererek cevaplanır.",
    pick_project: "Hangi proje? Yeni oturum orada başlar.",
    no_projects: "Henüz bilinen proje yok: bir projeyi PC'de Claude Code ile bir kez aç.",
    project_chosen: "📁 <b>{p}</b>: yeni oturum. Görevi yaz.",
    pick_session: "Son oturumlar. Sonraki mesajın seçtiğine gider.",
    no_sessions: "Oturum bulunamadı.",
    session_chosen: "💬 <b>{p}</b> · {t}\nMesajını yaz.",
    no_target: "Seçili oturum yok. /new yeni başlatır, /sessions mevcut birini seçer.",
    busy: "Bir uzaktan görev hâlâ çalışıyor. /status gösterir, /stop durdurur.",
    finishing: "Son görev cevap verdi ama arka plan işlerini bitiriyor. Biraz bekle ya da /stop ile durdur.",
    working: "⏳ <b>{p}</b> · çalışıyor…",
    done: "✅ <b>{p}</b> · bitti",
    failed: "❌ <b>{p}</b> · hata",
    stopped: "⏹ Durduruldu.",
    nothing_running: "Çalışan bir şey yok.",
    active_warn: "⚠️ Bu oturum {s} sn önce değişti, PC'de açık olabilir. Şimdi göndermek iki konuşmayı birbirine karıştırır.",
    send_anyway: "Yine de gönder",
    cancel: "İptal",
    cancelled: "İptal edildi.",
    expired: "Bu menü eskidi. Komutu yeniden gönder.",
    claude_missing: "Claude Code CLI bulunamadı. config.json'da remote.claudePath ayarla.",
    cleared: "Seçim temizlendi.",
    model_now: "Uzaktan çalıştırma modeli: <b>{m}</b>",
    model_default: "Uzaktan çalıştırma modeli: Claude Code varsayılanın.",
    model_invalid: "Geçerli bir model adı değil.",
    config_unreadable: "config.json okunamadı, bu yüzden değiştirilmedi. Dosyayı düzeltip tekrar dene.",
    off_not_saved: "Uzaktan oturum overlay yeniden başlayana kadar kapalı. config.json kaydedilemedi; kapalı kalması için dosyayı düzelt.",
    reply_hint: "Devam etmek için yaz · yeni oturum: /new",
    status_running: "▶️ <b>{p}</b> · {s} sn'dir çalışıyor",
    status_target_new: "Sonraki mesaj <b>{p}</b> içinde yeni oturum başlatır.",
    status_target_resume: "Sonraki mesaj <b>{p}</b> · {t} oturumuna gider",
    status_idle: "Boşta. /new ya da /sessions.",
    timeout: "⏱ {m} dakika sonra durduruldu.",
    truncated: "… (kısaltıldı)",
    turns: "tur",
    tray_enable: "Telefon: uzaktan oturum (kapalı)",
    tray_disable: "Telefon: uzaktan oturum (açık)",
    enabled_title: "Uzaktan oturum açık",
    enabled_body: "Eşleşmiş Telegram sohbetin artık bu PC'de Claude Code oturumu başlatıp sürdürebilir.",
    disabled_title: "Uzaktan oturum kapalı",
    cmd_new: "Bir projede yeni oturum başlat",
    cmd_sessions: "Son bir oturuma devam et",
    cmd_stop: "Çalışan görevi durdur",
    cmd_status: "Ne çalışıyor",
    cmd_exit: "Seçili oturumu bırak",
    cmd_model: "Uzaktan çalıştırma modeli",
    cmd_help: "Yardım",
};

pub fn s(shared: &Shared) -> &'static S {
    if *shared.lang_code.lock().unwrap() == "tr" {
        &TR
    } else {
        &EN
    }
}

fn esc(x: &str) -> String {
    crate::phone::esc(x)
}

fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in pairs {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn paired(cfg: &Value) -> bool {
    telegram::creds(cfg).map(|c| !c.chat_id.is_empty()).unwrap_or(false)
}

/// Remote sessions are usable: switched on and a Telegram chat is paired.
pub fn enabled(shared: &Shared) -> bool {
    let cfg = shared.cfg();
    config::get_bool(&cfg, "remote.enabled", false) && paired(&cfg) && !shared.remote.lock().unwrap().force_off
}

/// Was this Telegram message sent by a remote session (status, result, menus)?
pub fn is_remote_msg(shared: &Shared, mid: i64) -> bool {
    shared.remote.lock().unwrap().msg_ids.contains(&mid)
}

/// Forget the selected session and any held prompt (remote switched off).
pub fn clear_selection(shared: &Shared) {
    let mut st = shared.remote.lock().unwrap();
    st.target = None;
    st.target_gen += 1;
    st.held = None;
}

pub fn last_touch_ms(shared: &Shared) -> i64 {
    shared.remote.lock().unwrap().last_touch_ms
}

/// Is this session id one of our remote runs (running now or finished in the last minutes)?
pub fn is_remote_session(shared: &Shared, session_id: Option<&str>) -> bool {
    let Some(id) = session_id.filter(|s| !s.is_empty()) else { return false };
    shared.remote_sessions.lock().unwrap().contains(id)
}

/// Does a run currently target this session (used to hold its question/permission cards)?
pub fn has_target_or_run(shared: &Shared) -> bool {
    let st = shared.remote.lock().unwrap();
    st.target.is_some() || st.running.is_some()
}

// ---------------------------------------------------------------- discovery

fn claude_config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}

fn projects_root() -> PathBuf {
    claude_config_dir().join("projects")
}

fn usable_exe(p: &Path) -> bool {
    // During an npm update the binary is briefly a tiny placeholder.
    std::fs::metadata(p).map(|m| m.is_file() && m.len() > 1_000_000).unwrap_or(false)
}

/// The Claude Code CLI binary: remote.claudePath, the npm install, the native installer,
/// PATH, then the desktop app's bundled copy.
pub fn claude_path(cfg: &Value) -> Option<PathBuf> {
    if let Some(p) = config::get_str(cfg, "remote.claudePath").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(p);
        if usable_exe(&p) {
            return Some(p);
        }
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(a) = env("APPDATA") {
        candidates.push(a.join("npm").join("node_modules").join("@anthropic-ai").join("claude-code").join("bin").join("claude.exe"));
    }
    if let Some(u) = env("USERPROFILE") {
        candidates.push(u.join(".local").join("bin").join("claude.exe"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("claude.exe"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|p| usable_exe(p)) {
        return Some(found);
    }
    // Desktop app bundle: %APPDATA%\Claude\claude-code\<version>\...\claude.exe (newest).
    let base = env("APPDATA")?.join("Claude").join("claude-code");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![(base, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 4 {
                stack.push((p, depth + 1));
            } else if p.file_name().map(|n| n.eq_ignore_ascii_case("claude.exe")).unwrap_or(false) && usable_exe(&p) {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                    best = Some((t, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

fn temp_dir_lower() -> String {
    std::env::temp_dir().to_string_lossy().to_lowercase()
}

fn path_key(p: &str) -> String {
    p.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

/// (cwd, title) from the head of a transcript: the first real user prompt.
fn read_head(path: &Path) -> Option<(String, String)> {
    let f = File::open(path).ok()?;
    let reader = BufReader::new(f.take(3_000_000));
    let mut cwd = String::new();
    for line in reader.lines().take(600) {
        let Ok(line) = line else { break };
        if !line.contains("\"cwd\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        if v.get("type").and_then(Value::as_str) != Some("user")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let text = match v.pointer("/message/content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(a)) => a
                .iter()
                .find_map(|b| {
                    (b.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| b.get("text").and_then(Value::as_str).unwrap_or("").to_string())
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        let text = text.trim();
        if text.is_empty() || text.starts_with('<') {
            continue;
        }
        if cwd.is_empty() {
            return None;
        }
        let title = util::truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), 48);
        return Some((cwd, title));
    }
    if cwd.is_empty() {
        None
    } else {
        Some((cwd, String::new()))
    }
}

fn project_label(cwd: &str) -> String {
    util::project_name(Some(&Value::String(cwd.to_string())))
}

/// Top-level transcripts, newest first.
fn transcripts() -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(projects_root()) else { return out };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().map(|e| e == "jsonl").unwrap_or(false) && p.is_file() {
                if let Ok(t) = f.metadata().and_then(|m| m.modified()) {
                    out.push((p, t));
                }
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

fn usable_cwd(cwd: &str) -> bool {
    !cwd.is_empty() && Path::new(cwd).is_dir() && !cwd.to_lowercase().starts_with(&temp_dir_lower())
}

pub fn list_projects(cfg: &Value, limit: usize) -> Vec<Project> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for extra in config::get_str_list(cfg, "remote.projects") {
        if Path::new(&extra).is_dir() && seen.insert(path_key(&extra)) {
            out.push(Project { name: project_label(&extra), cwd: extra });
        }
    }
    // One head read per project folder (its newest transcript), newest folders first.
    let mut seen_dirs = HashSet::new();
    for (path, _) in transcripts() {
        if out.len() >= limit {
            break;
        }
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        if seen_dirs.contains(&dir) {
            continue;
        }
        let Some((cwd, _)) = read_head(&path) else { continue };
        seen_dirs.insert(dir);
        if usable_cwd(&cwd) && seen.insert(path_key(&cwd)) {
            out.push(Project { name: project_label(&cwd), cwd });
        }
    }
    out.truncate(limit);
    out
}

/// Only directories from this list may be used as a working directory.
fn is_known_project(cfg: &Value, cwd: &str) -> bool {
    let key = path_key(cwd);
    list_projects(cfg, 500).iter().any(|p| path_key(&p.cwd) == key)
}

pub fn list_sessions(limit: usize) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    for (path, _) in transcripts().into_iter().take(60) {
        if out.len() >= limit {
            break;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
        let Some((cwd, title)) = read_head(&path) else { continue };
        if !usable_cwd(&cwd) || title.is_empty() {
            continue;
        }
        out.push(SessionInfo { project: project_label(&cwd), id, cwd, title });
    }
    out
}

fn transcript_mtime_ms(id: &str) -> Option<i64> {
    let Ok(dirs) = std::fs::read_dir(projects_root()) else { return None };
    for d in dirs.flatten() {
        let p = d.path().join(format!("{id}.jsonl"));
        if let Ok(m) = std::fs::metadata(&p) {
            let t = m.modified().ok()?;
            return Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64);
        }
    }
    None
}

// ---------------------------------------------------------------- environment

/// Names of the user's and machine's persistent environment variables, read from the
/// registry. Anything Claude-related in our own environment that is not among them was
/// inherited from whichever Claude session happened to start the overlay, and must not leak
/// into remote runs. A failed read is not cached.
fn persistent_env_names() -> Option<HashSet<String>> {
    static NAMES: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    if let Some(n) = NAMES.lock().unwrap().clone() {
        return Some(n);
    }
    let mut names = HashSet::new();
    let mut ok = false;
    for key in ["HKCU\\Environment", "HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"] {
        let Ok(o) = Command::new("reg")
            .args(["query", key])
            .creation_flags(util::CREATE_NO_WINDOW)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if !o.status.success() {
            continue;
        }
        ok = true;
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            // "    NAME    REG_SZ    value"
            let t = line.trim_start();
            if line.starts_with("    ") && t.contains("    REG_") {
                if let Some(name) = t.split("    ").next() {
                    names.insert(name.trim().to_uppercase());
                }
            }
        }
    }
    if !ok {
        util::log_line(&["remote-error", "could not read persistent environment names"]);
        return None;
    }
    *NAMES.lock().unwrap() = Some(names.clone());
    Some(names)
}

/// Variables a parent Claude Code session injects into its children.
const SESSION_VARS: [&str; 6] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_CODE_SESSION_ID",
];

fn is_session_scoped(name: &str, persistent: Option<&HashSet<String>>) -> bool {
    let n = name.to_uppercase();
    if n == "CLAUDE_CONFIG_DIR" {
        // Listing and running must use the same Claude directory.
        return false;
    }
    match persistent {
        Some(p) => {
            (n == "CLAUDECODE" || n.starts_with("CLAUDE_") || n.starts_with("ANTHROPIC_") || n.starts_with("MCP_"))
                && !p.contains(&n)
        }
        None => SESSION_VARS.contains(&n.as_str()),
    }
}

// ---------------------------------------------------------------- telegram glue

fn send(shared: &Shared, html: &str, buttons: &[Vec<(String, String)>]) -> Option<i64> {
    match telegram::send(shared, html, buttons) {
        Ok(id) => {
            let mut st = shared.remote.lock().unwrap();
            if st.msg_ids.len() > 2000 {
                st.msg_ids.clear();
            }
            st.msg_ids.insert(id);
            Some(id)
        }
        Err(e) => {
            util::log_line(&["telegram-error", &e]);
            None
        }
    }
}

/// Bot command list shown in Telegram's menu.
pub fn commands(shared: &Shared) -> Vec<(&'static str, &'static str)> {
    let x = s(shared);
    vec![
        ("new", x.cmd_new),
        ("sessions", x.cmd_sessions),
        ("status", x.cmd_status),
        ("stop", x.cmd_stop),
        ("exit", x.cmd_exit),
        ("model", x.cmd_model),
        ("help", x.cmd_help),
    ]
}

fn valid_model(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 64
        && !m.starts_with('-')
        && m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '[' | ']' | ':'))
}

/// A slash command from the paired chat. Returns true when it was a remote command.
pub fn on_command(app: &AppHandle, shared: &Arc<Shared>, text: &str) -> bool {
    let mut parts = text.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let cmd = head.split('@').next().unwrap_or("").to_lowercase();
    let arg = parts.next().unwrap_or("").trim().to_string();
    let known = ["/new", "/projects", "/sessions", "/stop", "/status", "/exit", "/model", "/help"];
    if !known.contains(&cmd.as_str()) {
        return false;
    }
    util::log_line(&["remote-cmd", &cmd]);
    let x = s(shared);
    // /stop must always work, even after remote sessions were switched off mid-run.
    if cmd == "/stop" {
        if stop_running(app, shared, false) {
            send(shared, x.stopped, &[]);
        } else {
            send(shared, x.nothing_running, &[]);
        }
        return true;
    }
    if !enabled(shared) {
        send(shared, x.disabled, &[]);
        return true;
    }
    let cfg = shared.cfg();
    match cmd.as_str() {
        "/help" => {
            send(shared, x.help, &[]);
        }
        "/new" | "/projects" => {
            let projects = list_projects(&cfg, 10);
            if projects.is_empty() {
                send(shared, x.no_projects, &[]);
            } else {
                let gen = {
                    let mut st = shared.remote.lock().unwrap();
                    st.menu_gen += 1;
                    st.projects_menu = projects.clone();
                    st.menu_gen
                };
                let buttons: Vec<Vec<(String, String)>> = projects
                    .iter()
                    .enumerate()
                    .map(|(i, p)| vec![(format!("📁 {}", p.name), format!("rp:{gen}:{i}"))])
                    .collect();
                send(shared, x.pick_project, &buttons);
            }
        }
        "/sessions" => {
            let sessions = list_sessions(8);
            if sessions.is_empty() {
                send(shared, x.no_sessions, &[]);
            } else {
                let gen = {
                    let mut st = shared.remote.lock().unwrap();
                    st.menu_gen += 1;
                    st.sessions_menu = sessions.clone();
                    st.menu_gen
                };
                let buttons: Vec<Vec<(String, String)>> = sessions
                    .iter()
                    .enumerate()
                    .map(|(i, si)| vec![(util::truncate(&format!("{} · {}", si.project, si.title), 60), format!("rs:{gen}:{i}"))])
                    .collect();
                send(shared, x.pick_session, &buttons);
            }
        }
        "/status" => {
            let lines = {
                let st = shared.remote.lock().unwrap();
                let mut lines = Vec::new();
                if let Some(r) = &st.running {
                    let secs = ((util::now_ms() - r.started_ms) / 1000).to_string();
                    lines.push(fill(x.status_running, &[("p", &esc(&r.project)), ("s", &secs)]));
                }
                match &st.target {
                    Some(Target::New { project, .. }) => lines.push(fill(x.status_target_new, &[("p", &esc(project))])),
                    Some(Target::Resume { project, title, .. }) => {
                        lines.push(fill(x.status_target_resume, &[("p", &esc(project)), ("t", &esc(title))]))
                    }
                    None => {}
                }
                if lines.is_empty() {
                    lines.push(x.status_idle.to_string());
                }
                lines
            };
            send(shared, &lines.join("\n"), &[]);
        }
        "/exit" => {
            {
                let mut st = shared.remote.lock().unwrap();
                st.target = None;
                st.target_gen += 1;
                st.held = None;
            }
            send(shared, x.cleared, &[]);
        }
        "/model" => {
            if !arg.is_empty() {
                let value = if arg.eq_ignore_ascii_case("default") || arg == "-" {
                    String::new()
                } else if valid_model(&arg) {
                    arg.clone()
                } else {
                    send(shared, x.model_invalid, &[]);
                    return true;
                };
                if !set_user_config(shared, "model", json!(value)) {
                    send(shared, x.config_unreadable, &[]);
                    return true;
                }
            }
            let m = config::get_str(&shared.cfg(), "remote.model").unwrap_or_default();
            if m.trim().is_empty() {
                send(shared, x.model_default, &[]);
            } else {
                send(shared, &fill(x.model_now, &[("m", &esc(&m))]), &[]);
            }
        }
        _ => {}
    }
    true
}

/// A button from a remote menu. Returns the short text for Telegram's callback toast.
pub fn on_callback(app: &AppHandle, shared: &Arc<Shared>, data: &str) -> Option<String> {
    util::log_line(&["remote-cb", data]);
    let x = s(shared);
    if !enabled(shared) {
        return Some(x.disabled.to_string());
    }
    let mut it = data.split(':');
    let kind = it.next().unwrap_or("");
    let gen: u64 = it.next().and_then(|g| g.parse().ok()).unwrap_or(u64::MAX);
    let idx: usize = it.next().and_then(|i| i.parse().ok()).unwrap_or(usize::MAX);
    match kind {
        "rp" | "rs" => {
            let picked = {
                let mut st = shared.remote.lock().unwrap();
                if gen != st.menu_gen {
                    None
                } else if kind == "rp" {
                    st.projects_menu.get(idx).cloned().map(|p| {
                        let t = Target::New { cwd: p.cwd.clone(), project: p.name.clone() };
                        st.target = Some(t);
                        st.target_gen += 1;
                        st.held = None;
                        st.last_touch_ms = util::now_ms();
                        fill(x.project_chosen, &[("p", &esc(&p.name))])
                    })
                } else {
                    st.sessions_menu.get(idx).cloned().map(|si| {
                        st.target = Some(Target::Resume { id: si.id.clone(), cwd: si.cwd.clone(), project: si.project.clone(), title: si.title.clone() });
                        st.target_gen += 1;
                        st.held = None;
                        st.last_touch_ms = util::now_ms();
                        fill(x.session_chosen, &[("p", &esc(&si.project)), ("t", &esc(&si.title))])
                    })
                }
            };
            match picked {
                Some(msg) => {
                    send(shared, &msg, &[]);
                    Some("✓".to_string())
                }
                None => {
                    send(shared, x.expired, &[]);
                    Some(x.expired.to_string())
                }
            }
        }
        "rf" | "rx" => {
            let held = {
                let mut st = shared.remote.lock().unwrap();
                match &st.held {
                    Some((g, _, _)) if *g == gen => st.held.take(),
                    _ => None,
                }
            };
            match (kind, held) {
                ("rf", Some((_, target, prompt))) => {
                    start_run(app, shared, target, prompt);
                    Some(x.send_anyway.to_string())
                }
                ("rx", Some(_)) => {
                    send(shared, x.cancelled, &[]);
                    Some(x.cancelled.to_string())
                }
                _ => Some(x.expired.to_string()),
            }
        }
        _ => None,
    }
}

/// Plain text that is not an answer to a card: send it to the selected session. Returns false
/// when remote sessions are off (the caller then falls back to its own handling).
pub fn on_text(app: &AppHandle, shared: &Arc<Shared>, text: &str) -> bool {
    if !enabled(shared) {
        return false;
    }
    let x = s(shared);
    let (target, running) = {
        let st = shared.remote.lock().unwrap();
        (st.target.clone(), st.running.as_ref().map(|r| r.result_sent))
    };
    util::log_line(&[
        "remote-text",
        match (&target, running) {
            (_, Some(_)) => "busy",
            (Some(Target::New { .. }), _) => "new",
            (Some(Target::Resume { .. }), _) => "resume",
            (None, _) => "no-target",
        },
    ]);
    if let Some(result_sent) = running {
        send(shared, if result_sent { x.finishing } else { x.busy }, &[]);
        return true;
    }
    let Some(target) = target else {
        send(shared, x.no_target, &[]);
        return true;
    };
    // Busy guard: a session someone is using on the PC right now.
    if let Target::Resume { id, .. } = &target {
        let guard_ms = config::get_u64(&shared.cfg(), "remote.busyGuardSeconds", 90) as i64 * 1000;
        let own_end = shared.remote.lock().unwrap().own_runs.get(id).copied();
        if let Some(mtime) = transcript_mtime_ms(id) {
            let age = util::now_ms() - mtime;
            let ours = own_end.map(|end| mtime <= end + 15_000).unwrap_or(false);
            if guard_ms > 0 && age < guard_ms && !ours {
                let gen = {
                    let mut st = shared.remote.lock().unwrap();
                    st.held_gen += 1;
                    let g = st.held_gen;
                    st.held = Some((g, target.clone(), text.to_string()));
                    g
                };
                let secs = (age / 1000).max(1).to_string();
                send(
                    shared,
                    &fill(x.active_warn, &[("s", &secs)]),
                    &[vec![(x.send_anyway.to_string(), format!("rf:{gen}")), (x.cancel.to_string(), format!("rx:{gen}"))]],
                );
                return true;
            }
        }
    }
    start_run(app, shared, target, text.to_string());
    true
}

/// Write one key of the `remote` section into the user's config.json. Refuses to touch a
/// file that exists but does not parse, so a typo never wipes the user's settings.
fn set_user_config(shared: &Shared, key: &str, value: Value) -> bool {
    let path = util::config_path();
    let mut cfg = if path.exists() {
        match util::read_json(&path) {
            Some(v) if v.is_object() => v,
            _ => {
                util::log_line(&["remote-error", "config.json unreadable, not written"]);
                return false;
            }
        }
    } else {
        json!({})
    };
    if !cfg.get("remote").map(Value::is_object).unwrap_or(false) {
        cfg["remote"] = json!({});
    }
    cfg["remote"][key] = value;
    if util::write_json(&path, &cfg).is_err() {
        return false;
    }
    shared.reload_config();
    true
}

/// Tray toggle. Turning it off always stops a running task and clears the selection, even
/// when config.json cannot be written (then it stays off for this session in memory).
pub fn toggle(app: &AppHandle, shared: &Arc<Shared>) -> Toggle {
    let cfg = shared.cfg();
    let currently_on = enabled(shared);
    if !currently_on {
        if !paired(&cfg) {
            return Toggle::NeedPairing;
        }
        if !set_user_config(shared, "enabled", json!(true)) {
            return Toggle::ConfigUnreadable;
        }
        {
            let mut st = shared.remote.lock().unwrap();
            st.force_off = false;
            st.commands_registered = false;
        }
        util::log_line(&["remote", "enabled"]);
        return Toggle::On;
    }
    stop_running(app, shared, false);
    let written = set_user_config(shared, "enabled", json!(false));
    clear_selection(shared);
    {
        let mut st = shared.remote.lock().unwrap();
        st.commands_registered = false;
        st.force_off = !written;
    }
    util::log_line(&["remote", "disabled"]);
    if written {
        Toggle::Off
    } else {
        Toggle::OffNotSaved
    }
}

// ---------------------------------------------------------------- running claude

pub fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(util::CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Stop the running task (process tree) and close its open cards. Returns false when
/// nothing was running.
pub fn stop_running(app: &AppHandle, shared: &Arc<Shared>, quitting: bool) -> bool {
    let info = {
        let mut st = shared.remote.lock().unwrap();
        st.running.as_mut().map(|r| {
            r.stop_requested = true;
            (r.pid, r.exited, r.session_id.clone())
        })
    };
    let Some((pid, exited, session_id)) = info else { return false };
    util::log_line(&["remote-stop", &pid.to_string()]);
    // pid 0: not spawned yet (start_run checks stop_requested right after spawning).
    if pid != 0 && !exited {
        kill_tree(pid);
    }
    if let Some(id) = &session_id {
        events::cancel_pending_for_session(app, shared, id);
    }
    let _ = quitting; // on quit the process exits right away: no slow network calls here
    true
}

/// Called when the overlay quits.
pub fn shutdown(app: &AppHandle, shared: &Arc<Shared>) {
    stop_running(app, shared, true);
}

/// HTML-escaped chunks of `text`, each at most CHUNK_MAX characters after escaping.
fn escaped_chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    let push_piece = |piece: String, out: &mut Vec<String>, cur: &mut String, cur_len: &mut usize| {
        let len = piece.chars().count();
        if *cur_len > 0 && *cur_len + 1 + len > CHUNK_MAX {
            out.push(std::mem::take(cur));
            *cur_len = 0;
        }
        if *cur_len > 0 {
            cur.push('\n');
            *cur_len += 1;
        }
        cur.push_str(&piece);
        *cur_len += len;
    };
    for line in text.split('\n') {
        let escaped = esc(line);
        if escaped.chars().count() <= CHUNK_MAX {
            push_piece(escaped, &mut out, &mut cur, &mut cur_len);
            continue;
        }
        // A very long line: split the raw text so no escape sequence is cut in half.
        let mut piece = String::new();
        let mut piece_len = 0usize;
        for ch in line.chars() {
            let e = esc(&ch.to_string());
            let l = e.chars().count();
            if piece_len + l > CHUNK_MAX {
                push_piece(std::mem::take(&mut piece), &mut out, &mut cur, &mut cur_len);
                piece_len = 0;
            }
            piece.push_str(&e);
            piece_len += l;
        }
        if !piece.is_empty() {
            push_piece(piece, &mut out, &mut cur, &mut cur_len);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn activity_line(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str) {
        Some("tool_use") => {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
            Some(format!("🔧 {}", util::truncate(&events::describe_tool(name, block.get("input")), 120)))
        }
        Some("text") => {
            let t = block.get("text").and_then(Value::as_str).unwrap_or("").trim();
            if t.is_empty() {
                None
            } else {
                Some(format!("✍️ {}", util::truncate(&t.split_whitespace().collect::<Vec<_>>().join(" "), 120)))
            }
        }
        _ => None,
    }
}

/// Feed the rate-limit windows that headless runs report into the limit-reset timers
/// (only the windows the timers know how to label).
fn feed_rate_limits(app: &AppHandle, shared: &Arc<Shared>, v: &Value) {
    let Some(windows) = v.pointer("/rate_limit_info/unifiedWindows").and_then(Value::as_object) else { return };
    let mut rl = Map::new();
    for (name, info) in windows {
        if !KNOWN_WINDOWS.contains(&name.as_str()) {
            continue;
        }
        let Some(at) = info.get("resetsAt").and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))) else { continue };
        let used = info.get("utilization").and_then(Value::as_f64).map(|u| u * 100.0).unwrap_or(0.0);
        rl.insert(name.clone(), json!({"resets_at": at, "used_percentage": used}));
    }
    if !rl.is_empty() {
        events::handle_status(app, shared, &json!({"rate_limits": Value::Object(rl)}));
    }
}

pub fn start_run(app: &AppHandle, shared: &Arc<Shared>, target: Target, prompt: String) {
    let x = s(shared);
    if shared.remote.lock().unwrap().running.is_some() {
        send(shared, x.busy, &[]);
        return;
    }
    let cfg = shared.cfg();
    let (cwd, project, resume_id) = match &target {
        Target::New { cwd, project } => (cwd.clone(), project.clone(), None),
        Target::Resume { id, cwd, project, .. } => (cwd.clone(), project.clone(), Some(id.clone())),
    };
    // The working directory must be a known project, whatever the menus said earlier.
    if !is_known_project(&cfg, &cwd) {
        util::log_line(&["remote-refused", &cwd]);
        send(shared, x.no_projects, &[]);
        return;
    }
    let Some(exe) = claude_path(&cfg) else {
        send(shared, x.claude_missing, &[]);
        return;
    };
    let mut mode = config::get_str(&cfg, "remote.permissionMode").unwrap_or_else(|| "default".to_string());
    if !ALLOWED_MODES.contains(&mode.as_str()) {
        mode = "default".to_string();
    }
    let model = config::get_str(&cfg, "remote.model")
        .map(|m| m.trim().to_string())
        .filter(|m| valid_model(m));

    let mut cmd = Command::new(&exe);
    cmd.current_dir(&cwd)
        .args(["-p", "--output-format", "stream-json", "--verbose", "--permission-mode", &mode]);
    if let Some(m) = &model {
        cmd.args(["--model", m]);
    }
    if let Some(id) = &resume_id {
        cmd.args(["--resume", id]);
    }
    let persistent = persistent_env_names();
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().to_string();
        if is_session_scoped(&k, persistent.as_ref()) {
            cmd.env_remove(&k);
        }
    }
    cmd.env("MCP_CONNECTION_NONBLOCKING", "true")
        // Do not let leftover background work keep the run open for the default 10 minutes.
        .env("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS", "120000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(util::CREATE_NO_WINDOW);

    // Reserve the slot before spawning so two quick messages cannot both start a run.
    let seq = {
        let mut st = shared.remote.lock().unwrap();
        if st.running.is_some() {
            drop(st);
            send(shared, x.busy, &[]);
            return;
        }
        st.run_seq += 1;
        let seq = st.run_seq;
        st.running = Some(Running {
            pid: 0,
            seq,
            session_id: resume_id.clone(),
            project: project.clone(),
            started_ms: util::now_ms(),
            stop_requested: false,
            result_sent: false,
            activity: String::new(),
            status_mid: None,
            exited: false,
        });
        if let Some(id) = &resume_id {
            st.session_runs.insert(id.clone(), seq);
        }
        seq
    };
    let target_gen = shared.remote.lock().unwrap().target_gen;

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            {
                let mut st = shared.remote.lock().unwrap();
                st.running = None;
                if let Some(id) = &resume_id {
                    if st.session_runs.get(id) == Some(&seq) {
                        st.session_runs.remove(id);
                    }
                }
            }
            util::log_line(&["remote-error", &e.to_string()]);
            send(shared, &format!("{}\n<code>{}</code>", fill(x.failed, &[("p", &esc(&project))]), esc(&e.to_string())), &[]);
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
        // stdin is dropped here, which closes it so claude starts.
    }
    let pid = child.id();
    let started = util::now_ms();
    let stopped_meanwhile = {
        let mut st = shared.remote.lock().unwrap();
        st.last_touch_ms = started;
        match st.running.as_mut() {
            Some(r) if r.seq == seq => {
                r.pid = pid;
                r.started_ms = started;
                r.stop_requested
            }
            _ => true,
        }
    };
    if stopped_meanwhile {
        kill_tree(pid);
    }
    if let Some(id) = &resume_id {
        shared.remote_sessions.lock().unwrap().insert(id.clone());
    }
    util::log_line(&["remote-start", &project, resume_id.as_deref().unwrap_or("new")]);
    let status_mid = send(shared, &fill(x.working, &[("p", &esc(&project))]), &[]);
    if let Some(r) = shared.remote.lock().unwrap().running.as_mut() {
        if r.seq == seq {
            r.status_mid = status_mid;
        }
    }

    // stderr: keep the tail for error reports.
    let stderr_tail = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(err) = child.stderr.take() {
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut t = tail.lock().unwrap();
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 2000;
                    let idx = t.char_indices().find(|(i, _)| *i >= cut).map(|(i, _)| i).unwrap_or(0);
                    *t = t[idx..].to_string();
                }
            }
        });
    }

    let stdout = child.stdout.take();
    let ctx = Arc::new(RunCtx {
        seq,
        project: project.clone(),
        cwd: cwd.clone(),
        started,
        status_mid,
        target_gen,
        title_for_new: util::truncate(&prompt.split_whitespace().collect::<Vec<_>>().join(" "), 48),
        stderr_tail,
        out: std::sync::Mutex::new(RunOut { session_id: resume_id.clone(), ..RunOut::default() }),
        edit_guard: std::sync::Mutex::new(()),
    });

    // Waiter: owns the process. Normally stdout reaches EOF the moment it exits; if a leftover
    // grandchild keeps the pipe open, the run is finished anyway shortly after the exit.
    {
        let app = app.clone();
        let shared = shared.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
            if let Some(r) = shared.remote.lock().unwrap().running.as_mut() {
                if r.seq == ctx.seq {
                    r.exited = true;
                }
            }
            std::thread::sleep(Duration::from_secs(10));
            // If the reader is still sending the answer, let it finish first (bounded).
            for _ in 0..90 {
                let busy = {
                    let o = ctx.out.lock().unwrap();
                    o.reported && !o.report_done && !o.finalized
                };
                if !busy {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            finalize(&app, &shared, &ctx);
        });
    }

    // Watchdog + progress ticker: edits the status message with the latest step every few
    // seconds, and stops the run after remote.maxRunMinutes.
    {
        let app = app.clone();
        let shared = shared.clone();
        let ctx = ctx.clone();
        let max_ms = config::get_u64(&cfg, "remote.maxRunMinutes", 30).max(1) as i64 * 60_000;
        std::thread::spawn(move || {
            let mut shown = String::new();
            loop {
                std::thread::sleep(Duration::from_secs(4));
                let snapshot = {
                    let st = shared.remote.lock().unwrap();
                    match st.running.as_ref() {
                        Some(r) if r.seq == ctx.seq => Some((r.activity.clone(), r.result_sent, r.stop_requested)),
                        _ => None,
                    }
                };
                let Some((activity, result_sent, stop_requested)) = snapshot else { return };
                if util::now_ms() - ctx.started > max_ms && !stop_requested {
                    util::log_line(&["remote-timeout", &ctx.seq.to_string()]);
                    stop_running(&app, &shared, false);
                    let minutes = (max_ms / 60_000).to_string();
                    send(&shared, &fill(s(&shared).timeout, &[("m", &minutes)]), &[]);
                    continue;
                }
                if result_sent || stop_requested || activity == shown || activity.is_empty() {
                    continue;
                }
                if let Some(mid) = ctx.status_mid {
                    // Re-check under the edit guard so a progress edit can never land after the
                    // final "done" edit.
                    let _g = ctx.edit_guard.lock().unwrap();
                    let finalized = ctx.out.lock().unwrap().finalized;
                    let still = !finalized && shared
                        .remote
                        .lock()
                        .unwrap()
                        .running
                        .as_ref()
                        .map(|r| r.seq == ctx.seq && !r.result_sent && !r.stop_requested)
                        .unwrap_or(false);
                    if still {
                        let secs = (util::now_ms() - ctx.started) / 1000;
                        let html = format!("{} ({secs} s)\n<i>{}</i>", fill(s(&shared).working, &[("p", &esc(&ctx.project))]), esc(&activity));
                        telegram::edit(&shared, mid, &html);
                    }
                }
                shown = activity;
            }
        });
    }

    // Reader: parses the stream until EOF (the process may keep writing after the result line
    // while it finishes background work; stopping early would leave its pipe full).
    let app = app.clone();
    let shared = shared.clone();
    std::thread::spawn(move || {
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
                match v.get("type").and_then(Value::as_str).unwrap_or("") {
                    "system" if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                        if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                            ctx.out.lock().unwrap().session_id = Some(id.to_string());
                            shared.remote_sessions.lock().unwrap().insert(id.to_string());
                            let mut st = shared.remote.lock().unwrap();
                            st.session_runs.insert(id.to_string(), ctx.seq);
                            if let Some(r) = st.running.as_mut() {
                                if r.seq == ctx.seq {
                                    r.session_id = Some(id.to_string());
                                }
                            }
                        }
                    }
                    "assistant" if v.get("parent_tool_use_id").map(Value::is_null).unwrap_or(true) => {
                        if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                            for b in blocks {
                                if let Some(a) = activity_line(b) {
                                    if let Some(r) = shared.remote.lock().unwrap().running.as_mut() {
                                        if r.seq == ctx.seq {
                                            r.activity = a;
                                        }
                                    }
                                }
                                if b.get("type").and_then(Value::as_str) == Some("text") {
                                    let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                    if !t.trim().is_empty() {
                                        ctx.out.lock().unwrap().last_text = t.to_string();
                                    }
                                }
                            }
                        }
                    }
                    "rate_limit_event" => feed_rate_limits(&app, &shared, &v),
                    "result" => {
                        // Mark it before waiting for the guard, so a waiter-side finalize waits
                        // for this report instead of declaring the run failed.
                        let first = {
                            let mut o = ctx.out.lock().unwrap();
                            let first = !o.reported && !o.finalized;
                            if first {
                                o.reported = true;
                            }
                            first
                        };
                        let _g = ctx.edit_guard.lock().unwrap();
                        let first = first && !ctx.out.lock().unwrap().finalized;
                        if first {
                            // Not finalized yet, so the run is still registered.
                            let stopped = {
                                let mut st = shared.remote.lock().unwrap();
                                match st.running.as_mut() {
                                    Some(r) if r.seq == ctx.seq => {
                                        r.result_sent = true;
                                        r.stop_requested
                                    }
                                    _ => true,
                                }
                            };
                            if !stopped {
                                report(&shared, &ctx, &v);
                                ctx.out.lock().unwrap().report_ran = true;
                            }
                            ctx.out.lock().unwrap().report_done = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        finalize(&app, &shared, &ctx);
    });
}

/// Fixed facts about one run, shared by its reader, waiter and ticker threads.
struct RunCtx {
    seq: u64,
    project: String,
    cwd: String,
    started: i64,
    status_mid: Option<i64>,
    target_gen: u64,
    title_for_new: String,
    stderr_tail: Arc<std::sync::Mutex<String>>,
    out: std::sync::Mutex<RunOut>,
    /// Serializes status-message edits between the ticker and the final report.
    edit_guard: std::sync::Mutex<()>,
}

#[derive(Default)]
struct RunOut {
    session_id: Option<String>,
    /// The result line was seen.
    reported: bool,
    /// The reader has finished handling the result line.
    report_done: bool,
    /// report() actually ran (the answer was sent).
    report_ran: bool,
    ok: bool,
    last_text: String,
    finalized: bool,
}

/// The result line arrived: final status edit and the answer itself.
fn report(shared: &Arc<Shared>, ctx: &RunCtx, r: &Value) {
    let x = s(shared);
    let elapsed = (util::now_ms() - ctx.started) / 1000;
    let is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    let turns = r.get("num_turns").and_then(Value::as_i64).unwrap_or(0);
    let last_text = {
        let mut o = ctx.out.lock().unwrap();
        if let Some(id) = r.get("session_id").and_then(Value::as_str) {
            o.session_id = Some(id.to_string());
        }
        o.ok = !is_error;
        o.last_text.clone()
    };
    let head = if is_error { fill(x.failed, &[("p", &esc(&ctx.project))]) } else { fill(x.done, &[("p", &esc(&ctx.project))]) };
    if let Some(mid) = ctx.status_mid {
        telegram::edit(shared, mid, &format!("{head} · {turns} {} · {elapsed} s", x.turns));
    }
    let mut body = r.get("result").and_then(Value::as_str).unwrap_or("").to_string();
    if body.trim().is_empty() {
        body = last_text;
    }
    let chunks = escaped_chunks(&body);
    let total = chunks.len();
    for (i, c) in chunks.into_iter().take(4).enumerate() {
        let mut html = c;
        if i == 3 && total > 4 {
            html.push('\n');
            html.push_str(x.truncated);
        }
        send(shared, &html, &[]);
    }
}

/// End of a run (stdout EOF, or shortly after the process exited). Runs once.
fn finalize(app: &AppHandle, shared: &Arc<Shared>, ctx: &RunCtx) {
    let _g = ctx.edit_guard.lock().unwrap();
    let (session_id, reported, ok, last_text, report_ran) = {
        let mut o = ctx.out.lock().unwrap();
        if o.finalized {
            return;
        }
        o.finalized = true;
        (o.session_id.clone(), o.reported, o.ok, o.last_text.clone(), o.report_ran)
    };
    let x = s(shared);
    let elapsed = (util::now_ms() - ctx.started) / 1000;
    let stop_requested = shared
        .remote
        .lock()
        .unwrap()
        .running
        .as_ref()
        .map(|r| r.seq == ctx.seq && r.stop_requested)
        .unwrap_or(false);
    if stop_requested && !report_ran {
        if let Some(mid) = ctx.status_mid {
            telegram::edit(shared, mid, &format!("{} · {elapsed} s", x.stopped));
        }
    } else if !reported {
        if stop_requested {
            if let Some(mid) = ctx.status_mid {
                telegram::edit(shared, mid, &format!("{} · {elapsed} s", x.stopped));
            }
        } else {
            let tail = ctx.stderr_tail.lock().unwrap().clone();
            let detail = if tail.trim().is_empty() { last_text } else { tail };
            // Escape first, then size: the end of the output is the useful part.
            let body = escaped_chunks(detail.trim()).pop().unwrap_or_default();
            let html = format!("{}\n<code>{}</code>", fill(x.failed, &[("p", &esc(&ctx.project))]), body);
            match ctx.status_mid {
                Some(mid) => telegram::edit(shared, mid, &html),
                None => {
                    send(shared, &html, &[]);
                }
            }
        }
    } else if ok && !stop_requested {
        send(shared, &format!("<i>{}</i>", x.reply_hint), &[]);
    }

    // Cards of this run that are still open would otherwise swallow later messages.
    if let Some(id) = &session_id {
        events::cancel_pending_for_session(app, shared, id);
    }

    // The next message continues this session, unless the user picked something else while
    // the run was going.
    {
        let mut st = shared.remote.lock().unwrap();
        if let Some(id) = &session_id {
            st.own_runs.insert(id.clone(), util::now_ms());
            if st.target_gen == ctx.target_gen {
                let title = match &st.target {
                    Some(Target::Resume { id: tid, title, .. }) if tid == id => title.clone(),
                    _ => ctx.title_for_new.clone(),
                };
                st.target = Some(Target::Resume { id: id.clone(), cwd: ctx.cwd.clone(), project: ctx.project.clone(), title });
            }
        }
        if st.running.as_ref().map(|r| r.seq == ctx.seq).unwrap_or(false) {
            st.running = None;
        }
        st.last_touch_ms = util::now_ms();
    }
    util::log_line(&["remote-end", &ctx.project, session_id.as_deref().unwrap_or("-")]);

    // Hooks of this session keep being treated as remote for a little while, unless a newer
    // run on the same session has started meanwhile (that run owns the entry).
    if let Some(id) = session_id {
        let shared2 = shared.clone();
        let seq = ctx.seq;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(120));
            let newer = {
                let st = shared2.remote.lock().unwrap();
                st.session_runs.get(&id).map(|s| *s != seq).unwrap_or(false) || is_current_session(&st, &id)
            };
            if !newer {
                shared2.remote_sessions.lock().unwrap().remove(&id);
            }
        });
    }
}

fn is_current_session(st: &RemoteState, id: &str) -> bool {
    st.running.as_ref().and_then(|r| r.session_id.as_deref()).map(|s| s == id).unwrap_or(false)
}

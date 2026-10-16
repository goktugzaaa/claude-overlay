use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn roaming_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn home_dir() -> PathBuf {
    let var = |n: &str| std::env::var_os(n).filter(|v| !v.is_empty());
    var("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(var("HOMEDRIVE")?).join(var("HOMEPATH")?)))
        .unwrap_or_else(roaming_dir)
}

/// Runtime data lives in %USERPROFILE%\.headsup (shared with the PowerShell hooks), like
/// Claude Code's ~/.claude. Not in %APPDATA%: the Claude desktop app is an MSIX package, and
/// Windows redirects AppData folders created by processes it starts (Code tab hooks, and the
/// overlay a hook starts) into the package's private copy. The same path would then hold
/// different files inside and outside the app (another key, no Telegram pairing).
pub fn app_data_dir() -> PathBuf {
    home_dir().join(".headsup")
}

/// Folders an earlier version kept the same data in: Claude Overlay 2.x's
/// %APPDATA%\claude-notification and early 3.0 builds' %APPDATA%\HeadsUp, both as this
/// process sees AppData and inside the Claude app package's private copy of it.
pub fn legacy_data_dirs() -> Vec<PathBuf> {
    let mut roots = vec![roaming_dir()];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if let Ok(rd) = std::fs::read_dir(PathBuf::from(local).join("Packages")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("claude_") {
                    roots.push(e.path().join("LocalCache").join("Roaming"));
                }
            }
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        for name in ["HeadsUp", "claude-notification"] {
            let d = root.join(name);
            if d.is_dir() && !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// The earlier folder to take the settings from: the most recent install (inside and outside
/// the Claude app the same path can show an old and a current copy). By the `installedAt` the
/// installer stamps, not the file time: disarming rewrites an old copy's config.json.
pub fn legacy_data_source() -> Option<PathBuf> {
    let rank = |d: &PathBuf| {
        let p = d.join("config.json");
        let installed = read_json(&p)
            .and_then(|c| c.get("installedAt").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_default();
        (installed, std::fs::metadata(&p).and_then(|m| m.modified()).ok())
    };
    legacy_data_dirs()
        .into_iter()
        .filter(|d| d.join("config.json").is_file())
        .max_by_key(rank)
}

/// Settings of an earlier version wait to be taken over: there is one and no config yet.
pub fn legacy_data_pending() -> bool {
    !config_path().exists() && legacy_data_source().is_some()
}

/// Take over an earlier version's settings (Telegram pairing, stored status line and Codex
/// notify, state, key) and disarm every earlier folder. Err: config.json could not be copied;
/// nothing was taken over, so the next start tries again.
pub fn migrate_legacy_data() -> Result<(), String> {
    if !legacy_data_pending() {
        return Ok(());
    }
    let Some(src) = legacy_data_source() else { return Ok(()) };
    copy_data(&src, &app_data_dir())?;
    log_line(&["migrate", "settings taken over from", &src.to_string_lossy()]);
    disarm_legacy_data();
    Ok(())
}

/// Copy the files that matter; config.json last and in one step, so its presence means the
/// rest is there too.
fn copy_data(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for f in ["state.json", "secret.key"] {
        if src.join(f).is_file() && !dst.join(f).exists() {
            let _ = std::fs::copy(src.join(f), dst.join(f));
        }
    }
    let cfg = std::fs::read(src.join("config.json")).map_err(|e| format!("config.json could not be read: {e}"))?;
    write_atomic(&dst.join("config.json"), &cfg).map_err(|e| format!("config.json could not be written: {e}"))
}

/// Keep earlier folders from starting an old exe through the hooks of sessions opened before
/// the upgrade: their config loses the exe path those scripts start (they stop there) and
/// the install state, and their hook scripts are deleted. Never creates a folder.
pub fn disarm_legacy_data() {
    for dir in legacy_data_dirs() {
        disarm_legacy_dir(&dir);
    }
}

fn disarm_legacy_dir(dir: &Path) {
    let cfg_path = dir.join("config.json");
    if let Some(mut cfg) = read_json(&cfg_path) {
        if let Some(o) = cfg.as_object_mut() {
            let removed = ["overlayExe", "overlayArgs", "installedVersion"]
                .into_iter()
                .fold(false, |any, k| o.remove(k).is_some() || any);
            if removed {
                let _ = write_json(&cfg_path, &cfg);
            }
        }
    }
    let scripts = crate::hooks::LEGACY.iter().copied().chain(crate::hooks::SCRIPTS.iter().map(|(n, _)| *n));
    for name in scripts {
        let _ = std::fs::remove_file(dir.join("hooks").join(name));
    }
}

/// Claude Overlay 2.x's hook scripts start the old exe only when this stamp is at least 10 s
/// old (they share it with each other).
fn legacy_spawn_stamp() -> PathBuf {
    std::env::var_os("TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("claude-overlay-spawn.stamp")
}

/// Keep 2.x's hooks from starting the old exe for the next 10 s. True when one of them did so
/// within the last 10 s (that copy may not be listening yet).
pub fn hold_legacy_spawns() -> bool {
    let path = legacy_spawn_stamp();
    let recent = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(false, |age| age.as_secs() < 10);
    let _ = std::fs::write(&path, now_ms().to_string());
    recent
}

/// After the data moved: rename the 2.x exe its config named to `<name>.old` (a running or
/// just-exited exe can be renamed), so a 2.x hook that read the old config before the move
/// finds nothing to start. Only Claude Overlay's own exe without extra arguments (not an
/// Electron dev run's electron.exe), and never this exe.
pub fn retire_legacy_exe(old_cfg: &serde_json::Value) {
    let Some(exe) = old_cfg.get("overlayExe").and_then(|v| v.as_str()).map(PathBuf::from) else {
        return;
    };
    let no_args = old_cfg
        .get("overlayArgs")
        .map_or(true, |a| a.is_null() || a.as_array().map_or(false, |a| a.is_empty()));
    let this = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let other = exe.canonicalize().ok().map_or(false, |p| Some(p) != this);
    if !(no_args && other && is_legacy_exe_name(&exe) && exe.is_file()) {
        return;
    }
    let mut to = exe.clone().into_os_string();
    to.push(".old");
    match std::fs::rename(&exe, &to) {
        Ok(()) => log_line(&["migrate", "old exe renamed to *.old"]),
        Err(e) => log_line(&["migrate-error", &format!("old exe not renamed: {e}")]),
    }
}

/// ClaudeOverlay.exe, claude-overlay.exe or Claude Overlay.exe, in any case.
fn is_legacy_exe_name(exe: &Path) -> bool {
    let ext_ok = exe.extension().map_or(false, |e| e.eq_ignore_ascii_case("exe"));
    let stem: String = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect();
    ext_ok && stem == "claudeoverlay"
}

/// An environment override (tests and side-by-side runs), by its current name or by the
/// name Claude Overlay 2.x read: `PORT` is HEADSUP_PORT or CLAUDE_OVERLAY_PORT.
pub fn env_setting(name: &str) -> Option<String> {
    std::env::var(format!("HEADSUP_{name}"))
        .or_else(|_| std::env::var(format!("CLAUDE_OVERLAY_{name}")))
        .ok()
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.json")
}

pub fn state_path() -> PathBuf {
    app_data_dir().join("state.json")
}

pub fn log_path() -> PathBuf {
    app_data_dir().join("events.log")
}

pub fn hooks_dir() -> PathBuf {
    app_data_dir().join("hooks")
}

/// Per-install key for the local endpoint (64 lowercase hex chars, no newline).
pub fn secret_path() -> PathBuf {
    app_data_dir().join("secret.key")
}

/// Marker files: `stopped` (the user quit from the tray) and `uninstalled` (hooks removed).
/// While either exists, hooks do not start the overlay; a manual start removes both.
pub const MARKER_STOPPED: &str = "stopped";
pub const MARKER_UNINSTALLED: &str = "uninstalled";

pub fn marker_path(name: &str) -> PathBuf {
    app_data_dir().join(name)
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// events.log is rotated to events.log.1 at this size.
const LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const LOG_FIELD_MAX: usize = 500;
static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn log_line(fields: &[&str]) {
    let mut line = now_iso();
    for f in fields {
        line.push('\t');
        line.push_str(&log_field(f));
    }
    line.push('\n');
    // One writer at a time, so two threads never both rotate the file.
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = log_path();
    if std::fs::metadata(&path).map(|m| m.len() >= LOG_MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// One log field: secrets masked, control characters (tabs and line breaks included) turned
/// into spaces so a field can never forge a column or a whole line, and the length capped.
fn log_field(f: &str) -> String {
    let masked = crate::mask::secrets(&truncate(f, LOG_FIELD_MAX * 4));
    let clean: String = masked
        .chars()
        .map(|c| if c.is_control() || c == '\u{2028}' || c == '\u{2029}' { ' ' } else { c })
        .collect();
    truncate(&clean, LOG_FIELD_MAX)
}

/// The last 3 characters of an id (a Telegram chat id in the log), the rest hidden.
pub fn tail_id(id: &str) -> String {
    let chars: Vec<char> = id.trim().chars().collect();
    let keep = chars.len().min(3);
    format!("…{}", chars[chars.len() - keep..].iter().collect::<String>())
}

/// Held around every read-change-write of config.json (settings window, Telegram pairing,
/// remote switch, installer), so one writer never overwrites another's change with a stale
/// copy. Only around the read and the write: it is not reentrant.
static CONFIG_WRITE: Mutex<()> = Mutex::new(());

pub fn lock_config() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_WRITE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

pub fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    write_atomic(path, (text + "\n").as_bytes())
}

/// Replace a file's contents in one step: write a temp file next to it, flush it to disk and
/// rename it over the target, so a crash or a concurrent reader never sees a half-written file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // A symlinked file (e.g. a dotfiles-managed settings.json) is replaced at its target.
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let renamed = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        // Another program may hold the target open for a moment (Claude Code reading
        // settings.json): retry briefly.
        let mut last = None;
        for _ in 0..5 {
            match std::fs::rename(&tmp, &target) {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        Err(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "rename failed")))
    })();
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
        // Last resort, as before: write in place.
        return std::fs::write(&target, data);
    }
    Ok(())
}

/// A PowerShell command that never flashes a console window. Full System32 path, so a
/// powershell.exe next to the overlay exe is never picked up.
pub fn powershell() -> Command {
    let mut c = Command::new(crate::win::system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe"));
    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Open a file, folder or URL (ms-settings: links included) with the shell.
pub fn open_path(target: &str) {
    let _ = Command::new(crate::win::system_dir().join("cmd.exe"))
        .args(["/c", "start", "", target])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

pub fn locale() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "en".to_string())
}

pub fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------- local endpoint key

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

/// `n` bytes from the OS CSPRNG, as lowercase hex.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if getrandom::getrandom(&mut buf).is_err() {
        // Practically unreachable on Windows; still never hand out an all-zero value.
        use sha2::{Digest, Sha256};
        let seed = format!("{}-{}-{:p}", now_ms(), std::process::id(), &buf);
        let mut fill = Vec::new();
        let mut round = 0u32;
        while fill.len() < n {
            fill.extend_from_slice(&Sha256::digest(format!("{seed}-{round}").as_bytes()));
            round += 1;
        }
        buf.copy_from_slice(&fill[..n]);
    }
    hex(&buf)
}

/// HMAC-SHA256 (RFC 2104) over the concatenation of `parts`.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

/// Constant-time equality (for MACs).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn valid_secret(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_secret() -> Option<String> {
    let s = std::fs::read_to_string(secret_path()).ok()?;
    let s = s.trim();
    valid_secret(s).then(|| s.to_string())
}

/// The per-install key shared with the hook scripts (HMAC key = the UTF-8 bytes of the hex
/// string). Read from the file on every call (64 bytes), so a key written by another copy is
/// adopted at once; a deleted or damaged file is written back with the last known key, and
/// a fresh key from 32 CSPRNG bytes is created only when none is known yet.
pub fn secret() -> String {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = read_secret() {
        *last = Some(k.clone());
        return k;
    }
    if let Some(k) = last.clone() {
        // Gone while running: put back the key this overlay (and its hooks) were using.
        let _ = write_atomic(&secret_path(), k.as_bytes());
        return k;
    }
    let fresh = random_hex(32);
    let path = secret_path();
    let _ = std::fs::create_dir_all(app_data_dir());
    let tmp = path.with_extension(format!("key.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, &fresh).is_ok() {
        // hard_link publishes the complete file at once and fails if another copy of the
        // overlay created one meanwhile; rename replaces a malformed file.
        if std::fs::hard_link(&tmp, &path).is_err() && read_secret().is_none() {
            let _ = std::fs::rename(&tmp, &path);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    // Whoever won a creation race, the file is what the hooks read.
    let key = read_secret().unwrap_or(fresh);
    *last = Some(key.clone());
    key
}

// ---------------------------------------------------------------- text

/// Strip Markdown markup from a message so it reads well in a banner: fenced code blocks,
/// paired `**` / `__` / inline backticks and leading `#` / `>` line markers. A lone `_`, `*`,
/// `#` or `>` stays: it is part of an identifier or an expression (user_id, x > 5).
pub fn clean_message(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        s.push_str(&rest[..start]);
        s.push(' ');
        let after = &rest[start + 3..];
        match after.find("```") {
            Some(end) => rest = &after[end + 3..],
            None => {
                s.push_str(after);
                rest = "";
            }
        }
    }
    s.push_str(rest);
    let s = s.lines().map(strip_line_marker).collect::<Vec<_>>().join("\n");
    let s = strip_pairs(&s, "`", false);
    let s = strip_pairs(&s, "**", true);
    let s = strip_pairs(&s, "__", true);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop blockquote (`>`) and heading (`#`..`######` + space) markers at the start of a line.
fn strip_line_marker(line: &str) -> &str {
    let mut t = line.trim_start();
    while let Some(r) = t.strip_prefix('>') {
        t = r.trim_start();
    }
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &t[hashes..];
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            t = rest.trim_start();
        }
    }
    t
}

/// Remove matched pairs of `delim`, keeping what is between them. With `flanking`, a pair
/// only counts when it wraps a word (`**bold**`), so `2**10`, `**kwargs` or `__init__.py`
/// stay as they are.
fn strip_pairs(s: &str, delim: &str, flanking: bool) -> String {
    let d = delim.len();
    let before = |pos: usize| s[..pos].chars().next_back();
    let after = |pos: usize| s[pos..].chars().next();
    let opens = |pos: usize| {
        before(pos).map_or(true, |c| c.is_whitespace() || "([{\"'".contains(c))
            && after(pos + d).map_or(false, |c| !c.is_whitespace())
    };
    let closes = |pos: usize| {
        before(pos).map_or(false, |c| !c.is_whitespace())
            && match after(pos + d) {
                None => true,
                Some('.') => after(pos + d + 1).map_or(true, char::is_whitespace),
                Some(c) => c.is_whitespace() || ",;:!?)]}\"'".contains(c),
            }
    };
    let find_from = |from: usize| s[from..].find(delim).map(|x| x + from);
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(open) = find_from(search) {
        if flanking && !opens(open) {
            search = open + d;
            continue;
        }
        let mut close = None;
        let mut j = open + d;
        while let Some(c) = find_from(j) {
            if c > open + d && (!flanking || closes(c)) {
                close = Some(c);
                break;
            }
            j = c + d;
        }
        let Some(close) = close else { break };
        out.push_str(&s[copied..open]);
        out.push_str(&s[open + d..close]);
        copied = close + d;
        search = copied;
    }
    out.push_str(&s[copied..]);
    out
}

pub fn project_name(cwd: Option<&serde_json::Value>) -> String {
    let Some(cwd) = cwd.and_then(|v| v.as_str()) else {
        return String::new();
    };
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // Test case 6: key longer than the block size.
        let key = [0xaau8; 131];
        let mac = hmac_sha256(&key, &[b"Test Using Larger Than Block-Size Key - Hash Key First"]);
        assert_eq!(hex(&mac), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    #[test]
    fn clean_keeps_identifiers() {
        assert_eq!(clean_message("Use **user_id** for x > 5?"), "Use user_id for x > 5?");
        assert_eq!(clean_message("## Plan\n> quoted *note*"), "Plan quoted *note*");
        assert_eq!(clean_message("edit `__init__.py` and __init__.py"), "edit __init__.py and __init__.py");
        assert_eq!(clean_message("2**10 and **kwargs"), "2**10 and **kwargs");
        assert_eq!(clean_message("a ```code``` b #1"), "a b #1");
        assert_eq!(clean_message("__bold__."), "bold.");
        assert_eq!(clean_message("multi-byte **çğü** ok"), "multi-byte çğü ok");
    }

    #[test]
    fn data_is_copied_and_old_folders_disarmed() {
        let base = std::env::temp_dir().join(format!("headsup-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (old, new) = (base.join("claude-notification"), base.join(".headsup"));
        let old_cfg = r#"{"port":47821,"overlayExe":"C:\\x\\ClaudeOverlay.exe","overlayArgs":[],"installedVersion":7,"telegram":{"chatId":"1"}}"#;
        std::fs::create_dir_all(old.join("hooks")).unwrap();
        for f in ["state.json", "secret.key", "events.log", "hooks/x.ps1", "hooks/claude-overlay-send.ps1", "hooks/headsup-send.ps1"] {
            std::fs::write(old.join(f), f).unwrap();
        }
        std::fs::write(old.join("config.json"), old_cfg).unwrap();
        copy_data(&old, &new).unwrap();
        assert_eq!(std::fs::read_to_string(new.join("config.json")).unwrap(), old_cfg);
        assert!(new.join("secret.key").exists() && new.join("state.json").exists() && !new.join("events.log").exists());
        disarm_legacy_dir(&old);
        let left = read_json(&old.join("config.json")).unwrap();
        assert!(left.get("overlayExe").is_none() && left.get("overlayArgs").is_none() && left.get("installedVersion").is_none());
        assert_eq!(left["telegram"]["chatId"], "1");
        assert!(!old.join("hooks/claude-overlay-send.ps1").exists() && !old.join("hooks/headsup-send.ps1").exists());
        assert!(old.join("hooks/x.ps1").exists());
        // A config.json that cannot be read: an error, and no config in the new folder.
        let (bad, dst) = (base.join("bad"), base.join("dst"));
        std::fs::create_dir_all(&bad).unwrap();
        assert!(copy_data(&bad, &dst).is_err());
        assert!(!dst.join("config.json").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_exe_names() {
        assert!(is_legacy_exe_name(Path::new("C:\\p\\ClaudeOverlay\\ClaudeOverlay.exe")));
        assert!(is_legacy_exe_name(Path::new("C:\\r\\target\\release\\claude-overlay.EXE")));
        assert!(is_legacy_exe_name(Path::new("C:\\p\\Claude Overlay.exe")));
        assert!(!is_legacy_exe_name(Path::new("C:\\r\\node_modules\\electron\\dist\\electron.exe")));
        assert!(!is_legacy_exe_name(Path::new("C:\\p\\HeadsUp.exe")));
        assert!(!is_legacy_exe_name(Path::new("C:\\p\\ClaudeOverlay.exe.old")));
    }

    #[test]
    fn tail_id_short() {
        assert_eq!(tail_id("123456789"), "…789");
        assert_eq!(tail_id("12"), "…12");
    }
}

use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runtime data lives in %APPDATA%\claude-notification (shared with the PowerShell hooks).
pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("claude-notification")
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

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn log_line(fields: &[&str]) {
    let mut line = now_iso();
    for f in fields {
        line.push('\t');
        line.push_str(f);
    }
    line.push('\n');
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

pub fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, text + "\n")
}

/// A PowerShell command that never flashes a console window.
pub fn powershell() -> Command {
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Open a file, folder or URL (ms-settings: links included) with the shell.
pub fn open_path(target: &str) {
    let _ = Command::new("cmd")
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

/// Strip markdown-ish noise from a message so it reads well in a banner.
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
    let s: String = s
        .chars()
        .filter(|c| !matches!(c, '`' | '*' | '_' | '#' | '>'))
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn project_name(cwd: Option<&serde_json::Value>) -> String {
    let Some(cwd) = cwd.and_then(|v| v.as_str()) else {
        return String::new();
    };
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

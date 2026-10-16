//! Writes the Claude Code hooks into ~/.claude/settings.json, wraps the status line,
//! records where the overlay exe lives (for the hooks' autostart) and adds a Startup
//! shortcut. Everything is reversible with `uninstall`.
use crate::{config, util};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Bump when the set of hooks changes so existing installs re-run the installer.
pub const INSTALL_VERSION: u64 = 6;
const MARKERS: [&str; 3] = [
    "claude-overlay-",
    "claude-notification/hooks/",
    "ClaudeOverlay/resources/hooks/",
];

fn claude_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
}

pub fn settings_path() -> PathBuf {
    claude_dir().join("settings.json")
}

fn backup_path() -> PathBuf {
    claude_dir().join("settings.json.claude-overlay.bak")
}

fn startup_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| {
        PathBuf::from(a)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
    })
}

fn shortcut_path() -> Option<PathBuf> {
    startup_dir().map(|d| d.join("Claude Overlay.lnk"))
}

fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn ps_command(script: &Path, arg: Option<&str>) -> String {
    let mut s = format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
        fwd(script)
    );
    if let Some(a) = arg {
        s.push(' ');
        s.push_str(a);
    }
    s
}

fn is_our_command(cmd: &Value) -> bool {
    cmd.as_str()
        .map(|s| MARKERS.iter().any(|m| s.contains(m)))
        .unwrap_or(false)
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hs| {
            hs.iter()
                .any(|h| is_our_command(h.get("command").unwrap_or(&Value::Null)))
        })
        .unwrap_or(false)
}

fn strip_our_hooks(settings: &mut Value) {
    if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
        let keys: Vec<String> = hooks.keys().cloned().collect();
        for k in keys {
            let kept: Vec<Value> = hooks[&k]
                .as_array()
                .map(|l| l.iter().filter(|e| !is_ours(e)).cloned().collect())
                .unwrap_or_default();
            if kept.is_empty() {
                hooks.remove(&k);
            } else {
                hooks.insert(k, Value::Array(kept));
            }
        }
    }
    let empty = settings
        .get("hooks")
        .and_then(Value::as_object)
        .map(|o| o.is_empty())
        .unwrap_or(false);
    if empty {
        if let Some(o) = settings.as_object_mut() {
            o.remove("hooks");
        }
    }
}

fn push_hook(settings: &mut Value, event: &str, entry: Value) {
    let root = settings.as_object_mut().expect("settings is an object");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let list = hooks
        .as_object_mut()
        .unwrap()
        .entry(event)
        .or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    list.as_array_mut().unwrap().push(entry);
}

fn psq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn create_shortcut(exe: &Path) -> bool {
    let (Some(dir), Some(lnk)) = (startup_dir(), shortcut_path()) else {
        return false;
    };
    if !dir.exists() {
        return false;
    }
    let workdir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; $s.Arguments = ''; $s.WorkingDirectory = {}; $s.WindowStyle = 7; $s.Description = 'Claude Overlay notifications'; $s.Save()",
        psq(&lnk.to_string_lossy()),
        psq(&exe.to_string_lossy()),
        psq(&workdir.to_string_lossy())
    );
    util::powershell()
        .args(["-Command", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ClaudeOverlay.exe"))
}

pub fn install() -> Result<Vec<String>, String> {
    config::ensure_user_data();
    crate::hooks::write_scripts();
    let mut lines = Vec::new();
    let exe = current_exe();

    let mut cfg = match util::read_json(&util::config_path()) {
        Some(v) if v.is_object() => v,
        _ if util::config_path().exists() => {
            return Err("config.json could not be read, so nothing was changed".to_string());
        }
        _ => json!({}),
    };

    let settings_file = settings_path();
    let _ = std::fs::create_dir_all(claude_dir());
    let mut settings = match util::read_json(&settings_file) {
        Some(v) => v,
        None if settings_file.exists() => {
            return Err("settings.json could not be read, so it was not changed".to_string());
        }
        None => json!({}),
    };
    if !settings.is_object() {
        settings = json!({});
    }
    if settings_file.exists() && !backup_path().exists() {
        let _ = std::fs::copy(&settings_file, backup_path());
    }

    strip_our_hooks(&mut settings);
    let hooks = util::hooks_dir();
    let send = hooks.join("claude-overlay-send.ps1");
    push_hook(
        &mut settings,
        "Stop",
        json!({"hooks": [{"type": "command", "command": ps_command(&send, Some("stop")), "timeout": 15}]}),
    );
    push_hook(
        &mut settings,
        "Notification",
        json!({
            "matcher": "permission_prompt|idle_prompt|agent_needs_input|quota_auto_resume_fired",
            "hooks": [{"type": "command", "command": ps_command(&send, Some("notification")), "timeout": 15}]
        }),
    );
    // Interactive: the hook waits up to interactive.waitMs for a click on the overlay and
    // returns the answer / decision. With no click it prints nothing and Claude Code's own
    // dialog proceeds.
    // Generous: a typed answer or an answer from the phone can take a while.
    let hook_timeout = expected_hook_timeout();
    push_hook(
        &mut settings,
        "PermissionRequest",
        json!({"hooks": [{"type": "command", "command": ps_command(&send, Some("permission")), "timeout": hook_timeout}]}),
    );
    push_hook(
        &mut settings,
        "PreToolUse",
        json!({
            "matcher": "AskUserQuestion",
            "hooks": [{"type": "command", "command": ps_command(&send, Some("question")), "timeout": hook_timeout}]
        }),
    );

    // Status line: wrap whatever the user already had so their status line keeps working.
    let current = settings
        .get("statusLine")
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("command"))
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .map(String::from);
    let is_ours = |c: &str| MARKERS.iter().any(|m| c.contains(m));
    match &current {
        Some(c) if is_ours(c) => {
            // Already wrapped. Make sure the stored original is a real user command; older
            // versions could store one of our own wrappers here. The backup taken on the very
            // first install holds the user's real status line.
            let stored_ok = cfg
                .get("originalStatusLineCommand")
                .and_then(Value::as_str)
                .map(|s| !is_ours(s))
                .unwrap_or(false);
            if !stored_ok {
                let from_backup = util::read_json(&backup_path())
                    .and_then(|b| b.get("statusLine")?.get("command")?.as_str().map(String::from))
                    .filter(|c| !is_ours(c));
                cfg["originalStatusLineCommand"] = from_backup.map(Value::String).unwrap_or(Value::Null);
            }
        }
        _ => {
            cfg["originalStatusLineCommand"] =
                current.clone().map(Value::String).unwrap_or(Value::Null);
        }
    }
    let mut status_line = settings
        .get("statusLine")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    status_line["type"] = json!("command");
    status_line["command"] = json!(ps_command(&hooks.join("claude-overlay-statusline.ps1"), None));
    settings["statusLine"] = status_line;

    util::write_json(&settings_file, &settings).map_err(|e| format!("settings.json: {e}"))?;
    lines.push(format!("Claude Code hooks installed: {}", settings_file.display()));
    if cfg.get("originalStatusLineCommand").and_then(Value::as_str).is_some() {
        lines.push("Existing status line kept and wrapped.".to_string());
    }

    cfg["overlayExe"] = json!(exe.to_string_lossy());
    cfg["overlayArgs"] = json!([]);
    cfg["installedAt"] = json!(util::now_iso());
    cfg["installedVersion"] = json!(INSTALL_VERSION);
    util::write_json(&util::config_path(), &cfg).map_err(|e| format!("config.json: {e}"))?;

    if create_shortcut(&exe) {
        lines.push("Added to Windows startup.".to_string());
    } else {
        lines.push("Could not create the startup shortcut.".to_string());
    }
    Ok(lines)
}

pub fn uninstall() -> Vec<String> {
    let mut lines = Vec::new();
    let settings_file = settings_path();
    let mut settings = match util::read_json(&settings_file) {
        Some(v) if v.is_object() => v,
        _ if settings_file.exists() => {
            lines.push("settings.json could not be read, so it was not changed.".to_string());
            return lines;
        }
        _ => json!({}),
    };
    let mut cfg = match util::read_json(&util::config_path()) {
        Some(v) if v.is_object() => v,
        _ if util::config_path().exists() => {
            lines.push("config.json could not be read, so nothing was changed.".to_string());
            return lines;
        }
        _ => json!({}),
    };
    strip_our_hooks(&mut settings);
    let ours = settings
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .map(is_our_command)
        .unwrap_or(false);
    if ours {
        match cfg.get("originalStatusLineCommand").and_then(Value::as_str) {
            Some(orig) => settings["statusLine"]["command"] = json!(orig),
            None => {
                if let Some(o) = settings.as_object_mut() {
                    o.remove("statusLine");
                }
            }
        }
    }
    let _ = util::write_json(&settings_file, &settings);
    lines.push("Hooks and status line restored.".to_string());
    if let Some(lnk) = shortcut_path() {
        if std::fs::remove_file(&lnk).is_ok() {
            lines.push("Startup shortcut removed.".to_string());
        }
    }
    if let Some(o) = cfg.as_object_mut() {
        o.remove("installedAt");
        o.remove("installedVersion");
    }
    let _ = util::write_json(&util::config_path(), &cfg);
    lines
}

// ---------------------------------------------------------------- Codex CLI (optional)

fn codex_config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("config.toml")
}

fn codex_script() -> PathBuf {
    util::hooks_dir().join("claude-overlay-codex.ps1")
}

fn codex_notify_line() -> String {
    format!(
        "notify = [\"powershell\", \"-NoProfile\", \"-ExecutionPolicy\", \"Bypass\", \"-File\", \"{}\"]",
        fwd(&codex_script())
    )
}

fn is_codex_notify_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("notify") && t[6..].trim_start().starts_with('=')
}

pub fn codex_installed() -> bool {
    std::fs::read_to_string(codex_config_path())
        .map(|s| s.lines().any(|l| is_codex_notify_line(l) && l.contains("claude-overlay-codex.ps1")))
        .unwrap_or(false)
}

/// Adds our `notify` command to ~/.codex/config.toml (top-level key, so it goes before any
/// `[table]`). A different existing `notify` is remembered and restored on disable.
pub fn install_codex() -> Result<String, String> {
    crate::hooks::write_scripts();
    let path = codex_config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let backup = path.with_extension("toml.claude-overlay.bak");
    if path.exists() && !backup.exists() {
        let _ = std::fs::copy(&path, &backup);
    }
    let mut cfg = match util::read_json(&util::config_path()) {
        Some(v) if v.is_object() => v,
        _ if util::config_path().exists() => {
            return Err("config.json could not be read, so nothing was changed".to_string());
        }
        _ => json!({}),
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        if line.trim_start().starts_with('[') {
            in_table = true;
        }
        if !in_table && is_codex_notify_line(line) {
            if !line.contains("claude-overlay-codex.ps1") {
                cfg["originalCodexNotify"] = json!(line.trim());
            }
            continue;
        }
        kept.push(line);
    }
    let mut out = String::new();
    out.push_str(&codex_notify_line());
    out.push('\n');
    out.push_str(&kept.join("\n"));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    let _ = util::write_json(&util::config_path(), &cfg);
    Ok(format!("Codex notify set in {}", path.display()))
}

pub fn uninstall_codex() -> String {
    let path = codex_config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return "Codex config not found.".to_string();
    };
    let cfg = util::read_json(&util::config_path()).unwrap_or_else(|| json!({}));
    let original = cfg.get("originalCodexNotify").and_then(Value::as_str).map(String::from);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if is_codex_notify_line(line) && line.contains("claude-overlay-codex.ps1") {
            if let Some(o) = &original {
                out.push(o.clone());
            }
            continue;
        }
        out.push(line.to_string());
    }
    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    let _ = std::fs::write(&path, joined);
    "Codex notify removed.".to_string()
}

/// Seconds the question/permission hooks may run: longer than the longest card hold plus
/// the time a typed answer may take, so Claude Code never cancels a hook that is waiting.
pub fn expected_hook_timeout() -> u64 {
    let merged = config::load();
    let hold = config::get_u64(&merged, "phone.holdMs", 480000).max(config::get_u64(&merged, "interactive.waitMs", 20000));
    (hold + 999) / 1000 + 260
}

/// Our PreToolUse(AskUserQuestion) and PermissionRequest hooks exist with a long enough timeout.
fn hooks_healthy() -> bool {
    let path = settings_path();
    let Some(settings) = util::read_json(&path) else {
        // An existing settings.json that does not parse (a typo, or caught mid-write) is
        // never rewritten from here: report healthy and leave it alone.
        if path.exists() {
            util::log_line(&["install-skip", "settings.json unreadable"]);
            return true;
        }
        return false;
    };
    let want = expected_hook_timeout();
    let ok = |event: &str| -> bool {
        settings
            .pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .map(|entries| {
                entries.iter().any(|e| {
                    e.get("hooks").and_then(Value::as_array).map(|hs| {
                        hs.iter().any(|h| {
                            is_our_command(h.get("command").unwrap_or(&Value::Null))
                                && h.get("timeout").and_then(Value::as_u64).unwrap_or(0) >= want
                        })
                    }).unwrap_or(false)
                })
            })
            .unwrap_or(false)
    };
    ok("PreToolUse") && ok("PermissionRequest")
}

pub fn is_installed() -> bool {
    let Some(cfg) = util::read_json(&util::config_path()) else {
        return false;
    };
    let version = cfg.get("installedVersion").and_then(Value::as_u64);
    let exe = cfg.get("overlayExe").and_then(Value::as_str).unwrap_or("");
    version == Some(INSTALL_VERSION) && exe == current_exe().to_string_lossy() && hooks_healthy()
}

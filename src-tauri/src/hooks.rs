//! The PowerShell scripts Claude Code runs are compiled into the exe and written to
//! %USERPROFILE%\.headsup\hooks on every start, so the exe is a single file and
//! can be moved freely (settings.json always points at that copy).
use crate::util;

pub const SCRIPTS: [(&str, &str); 4] = [
    ("headsup-send.ps1", include_str!("../../hooks/headsup-send.ps1")),
    ("headsup-statusline.ps1", include_str!("../../hooks/headsup-statusline.ps1")),
    ("headsup-toast-listener.ps1", include_str!("../../hooks/headsup-toast-listener.ps1")),
    ("headsup-codex.ps1", include_str!("../../hooks/headsup-codex.ps1")),
];

/// Claude Overlay 2.x's script names, left behind in a migrated folder. settings.json and
/// config.toml are re-pointed to the new names on start, and sessions opened before the
/// upgrade call the old folder's path, so nothing runs these any more.
pub const LEGACY: [&str; 4] = [
    "claude-overlay-send.ps1",
    "claude-overlay-statusline.ps1",
    "claude-overlay-toast-listener.ps1",
    "claude-overlay-codex.ps1",
];

/// Write every script that differs from the embedded one; true when any was rewritten.
pub fn write_scripts() -> bool {
    let dir = util::hooks_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut wrote = false;
    for (name, content) in SCRIPTS {
        // UTF-8 BOM so Windows PowerShell 5.1 reads non-ASCII characters correctly.
        let wanted = format!("\u{feff}{content}");
        let p = dir.join(name);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(wanted.as_str()) {
            // Replaced in one step: a hook starting meanwhile never reads half a script.
            wrote |= util::write_atomic(&p, wanted.as_bytes()).is_ok();
        }
    }
    for name in LEGACY {
        let _ = std::fs::remove_file(dir.join(name));
    }
    wrote
}

//! The PowerShell scripts Claude Code runs are compiled into the exe and written to
//! %APPDATA%\claude-notification\hooks on every start, so the exe is a single file and
//! can be moved freely (settings.json always points at the AppData copy).
use crate::util;

pub const SCRIPTS: [(&str, &str); 4] = [
    ("claude-overlay-send.ps1", include_str!("../../hooks/claude-overlay-send.ps1")),
    ("claude-overlay-statusline.ps1", include_str!("../../hooks/claude-overlay-statusline.ps1")),
    ("claude-overlay-toast-listener.ps1", include_str!("../../hooks/claude-overlay-toast-listener.ps1")),
    ("claude-overlay-codex.ps1", include_str!("../../hooks/claude-overlay-codex.ps1")),
];

pub fn write_scripts() {
    let dir = util::hooks_dir();
    let _ = std::fs::create_dir_all(&dir);
    for (name, content) in SCRIPTS {
        // UTF-8 BOM so Windows PowerShell 5.1 reads non-ASCII characters correctly.
        let wanted = format!("\u{feff}{content}");
        let p = dir.join(name);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(wanted.as_str()) {
            let _ = std::fs::write(&p, wanted);
        }
    }
}

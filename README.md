# Claude Overlay

[![Release](https://img.shields.io/github/v/release/goktugzaaa/claude-overlay?label=download)](https://github.com/goktugzaaa/claude-overlay/releases/latest)
[![Build](https://github.com/goktugzaaa/claude-overlay/actions/workflows/release.yml/badge.svg)](https://github.com/goktugzaaa/claude-overlay/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

iPhone-style, always-on-top notifications for the **Claude desktop app** (Chat, Cowork, Code) and **Claude Code** CLI on Windows.
Shows up over fullscreen games and videos, where Windows hides its own toasts. One 3.5 MB exe, no installer, no runtime to install.

![Demo: permission answered from the overlay, question answered from the overlay, limit-reset logo, task-finished banner](docs/demo.gif)

- **Reply ready / task finished** → banner at the top of the screen with the conversation or project name and, for Claude Code, a summary of the last message.
- **Asking permission** → banner with the exact tool call (`Bash: npm run build && git push …`) and **Allow / Deny** buttons. Your click is the answer; Claude Code continues without you leaving the game.
- **Asking a question** → banner with the question and its options as buttons. Click one and Claude gets the answer.
- **Usage limit reset** (5-hour, weekly, spend limit) → animated Claude logo in the top-right corner, even if no session is open.
- Click-through, never steals focus, follows the monitor your cursor is on, optional soft chime.

## Install

1. Download `ClaudeOverlay.exe` from [Releases](../../releases/latest) and put it anywhere, e.g. `%LOCALAPPDATA%\Programs\ClaudeOverlay`.
2. Run it. SmartScreen will warn once because the exe is not code-signed (certificates cost money; the code is all here): click **More info → Run anyway**. On first start it:
   - adds its hooks to `~/.claude/settings.json` (backup: `settings.json.claude-overlay.bak`),
   - wraps your existing status line command (its output is preserved),
   - adds itself to the Startup folder.
3. Windows: **Settings → Privacy & security → Notifications → "Let desktop apps access notifications"** must be on. The overlay warns you if it is not; the tray menu opens the setting.
4. Claude app: keep notifications enabled (Settings → Notifications). Chat and Cowork signals come from there.

Needs the WebView2 runtime, which Windows 11 and up-to-date Windows 10 already have ([download](https://developer.microsoft.com/microsoft-edge/webview2/) otherwise).

A tray icon (orange starburst) gives you test buttons, config, log, and uninstall. Uninstall: tray → "Remove hooks", or `ClaudeOverlay.exe --uninstall`. If you move the exe, run it once more and it re-points the hooks.

## How it works

| Signal | Covers | Mechanism |
|---|---|---|
| Windows notification center | Chat, Cowork, Code | Every toast the Claude app emits is read through `UserNotificationListener` and re-rendered by the overlay. Works while Windows itself suppresses toasts (fullscreen, Do Not Disturb). |
| `Stop` hook | Code tab, CLI | Task finished + last assistant message. |
| `PermissionRequest` hook | Code tab, CLI | Tool name and input (command, file, URL); Allow / Deny from the banner. |
| `PreToolUse` hook for `AskUserQuestion` | Code tab, CLI | The question and its options; answer from the banner. |
| `Notification` hook | Code tab, CLI | `permission_prompt`, `agent_needs_input`, `quota_auto_resume_fired` (limit reset while auto-resuming). |
| Status line `rate_limits` | Code tab, CLI | `resets_at` of every window is stored; when it passes, the logo shows. The 5-hour/weekly windows are shared across Chat, Cowork and Code, so one Code session per window is enough to learn the reset time. |

When the same moment produces both a hook event and a toast, only one banner is shown (the hook, which carries more detail).

### Answering from the banner

For `PermissionRequest` and `AskUserQuestion` the hook keeps its request to the overlay open for `interactive.waitMs` (default 20 s). A click on **Allow**, **Deny** or an option is returned as the hook's JSON output (`decision` / `updatedInput.answers`), so Claude Code proceeds immediately. No click → the hook returns nothing and Claude Code's normal dialog appears. If the Claude app, a terminal or an IDE is the foreground window, the overlay skips the wait entirely (`interactive.skipWhenFocused`), because you can see the dialog anyway. The overlay is click-through except over a card with buttons, and it never takes keyboard focus.

![Chat toast mirrored by the overlay](docs/chat-toast.png)

## Configuration

`%APPDATA%\claude-notification\config.json` (tray → "Open config file", then "Reload config").

| Key | Meaning |
|---|---|
| `language` | `auto` (OS locale), `en`, `tr` |
| `sound` | chime on banners and logo |
| `notification.durationMs`, `logo.durationMs` | how long things stay on screen |
| `notification.showLastMessage` | include Claude's last message in the task-finished banner |
| `events.*` | per-event switches: `stop`, `permission_request`, `question`, `permission_prompt`, `idle_prompt`, `agent_needs_input`, `quota_auto_resume_fired` |
| `toasts.watchApps` | app names / AUMIDs to mirror (default `["Claude"]`); add `"Codex"`, `"ChatGPT"`, `"Cursor"` or any app whose toasts you want on top of fullscreen apps |
| `limits.*` | which windows trigger the logo (`five_hour`, `seven_day`, `spend_limit`) and `minUsedPercentage` |
| `interactive.enabled`, `interactive.waitMs`, `interactive.skipWhenFocused` | answer questions and permission requests from the banner; how long to wait; foreground process names that skip the wait |

`events.log` in the same folder records everything the overlay receives. The PowerShell scripts Claude Code runs are written to `hooks\` in that folder on every start.

## Other agents and apps

The notification-center bridge is generic: any app's toasts can be mirrored by adding its name to `toasts.watchApps`. Claude Code's hook format is also used by Codex CLI and others, so the hook scripts can be pointed at them with the same `claude-overlay-send.ps1 stop` command. Built-in support for more agents is on the roadmap; open an issue with the app name.

## Limits

- DirectX **exclusive** fullscreen cannot be drawn over by any window. Use borderless/windowed fullscreen or leave Windows' fullscreen optimizations on (default).
- `rate_limits` only exist for Pro/Max/Team subscriptions and only after a Claude Code session has made a request in that window. A window used purely from Chat has no known reset time.
- The Claude app sends toasts only while its window is not focused, which is exactly when you need them.

## Development

Rust stable + Visual Studio Build Tools (C++ workload) + Windows SDK.

```bash
cd src-tauri && cargo build --release
```

```bash
powershell -ExecutionPolicy Bypass -File scripts/dist.ps1
```

`dist.ps1` builds and writes `dist/ClaudeOverlay.exe` and `dist/ClaudeOverlay-win64.zip`. Run a build without touching your settings: set `CLAUDE_OVERLAY_NO_INSTALL=1` and `CLAUDE_OVERLAY_PORT=47899`, then post to `http://127.0.0.1:47899/test-question` or `/test-permission`.

Structure: `src-tauri/src/main.rs` (window, commands, CLI flags), `events.rs` (hook events, interactive cards, toast bridge, limit timers, cursor polling), `installer.rs` (hooks, status line, Startup shortcut), `http.rs` (127.0.0.1 endpoint), `tray.rs`, `i18n.rs`; `app/overlay.html` (banner + logo UI, embedded in the exe); `hooks/*.ps1` (what Claude Code and the toast listener run, embedded too).

## Privacy and safety

Everything runs locally. The overlay listens only on `127.0.0.1`, reads notifications from your own Windows notification center, and never sends anything anywhere. The hooks it adds only decide something when you click a button on a card; otherwise they print nothing and exit 0, so Claude Code's own dialogs are untouched. You can inspect every script in `hooks/`.

Independent community project, not affiliated with or endorsed by Anthropic.

## License

MIT

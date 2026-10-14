# Claude Overlay

[![Release](https://img.shields.io/github/v/release/goktugzaaa/claude-overlay?label=download)](https://github.com/goktugzaaa/claude-overlay/releases/latest)
[![Build](https://github.com/goktugzaaa/claude-overlay/actions/workflows/release.yml/badge.svg)](https://github.com/goktugzaaa/claude-overlay/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

iPhone-style, always-on-top notifications for the **Claude desktop app** (Chat, Cowork, Code) and **Claude Code** CLI on Windows.
Shows up over fullscreen games and videos, where Windows hides its own toasts.

![Banner and limit-reset logo over a fullscreen video](docs/banner-and-logo.png)

- **Reply ready / task finished** → banner at the top of the screen with the conversation or project name and, for Claude Code, a summary of the last message.
- **Asking permission** → banner with the exact tool call (`Bash: npm run build && git push …`) and **Allow / Deny** buttons. Your click is the answer; Claude Code continues without you leaving the game.
- **Asking a question** → banner with the question and its options as buttons. Click one and Claude gets the answer.
- **Usage limit reset** (5-hour, weekly, spend limit) → animated Claude logo in the top-right corner, even if no session is open.
- Click-through, never steals focus, follows the monitor your cursor is on, optional soft chime.
- No Node, no admin rights. One exe, self-installing.

## Install

1. Download `ClaudeOverlay-win64.zip` from [Releases](../../releases), extract anywhere (e.g. `%LOCALAPPDATA%\Programs\ClaudeOverlay`).
2. Run `ClaudeOverlay.exe`. On first start it:
   - adds its hooks to `~/.claude/settings.json` (backup: `settings.json.claude-overlay.bak`),
   - wraps your existing status line command (its output is preserved),
   - adds itself to the Startup folder.
3. Windows: **Settings → Privacy & security → Notifications → "Let desktop apps access notifications"** must be on. The overlay warns you if it is not; the tray menu opens the setting.
4. Claude app: keep notifications enabled (Settings → Notifications). Chat and Cowork signals come from there.

A tray icon (orange starburst) gives you test buttons, config, log, and uninstall. Uninstall: tray → "Remove hooks", or `ClaudeOverlay.exe --uninstall`.

## How it works

| Signal | Covers | Mechanism |
|---|---|---|
| Windows notification center | Chat, Cowork, Code | Every toast the Claude app emits is read through `UserNotificationListener` and re-rendered by the overlay. Works while Windows itself suppresses toasts (fullscreen, Do Not Disturb). |
| `Stop` hook | Code tab, CLI | Task finished + last assistant message. |
| `PermissionRequest` hook | Code tab, CLI | Tool name and input (command, file, URL). Observer only, never decides. |
| `PreToolUse` hook for `AskUserQuestion` | Code tab, CLI | The question text and options. |
| `Notification` hook | Code tab, CLI | `permission_prompt`, `agent_needs_input`, `quota_auto_resume_fired` (limit reset while auto-resuming). |
| Status line `rate_limits` | Code tab, CLI | `resets_at` of every window is stored; when it passes, the logo shows. The 5-hour/weekly windows are shared across Chat, Cowork and Code, so one Code session per window is enough to learn the reset time. |

When the same moment produces both a hook event and a toast, only one banner is shown (the hook, which carries more detail).

### Answering from the banner

For `PermissionRequest` and `AskUserQuestion` the hook keeps its request to the overlay open for `interactive.waitMs` (default 20 s). A click on **Allow**, **Deny** or an option is returned as the hook's JSON output (`decision` / `updatedInput.answers`), so Claude Code proceeds immediately. No click → the hook returns nothing and Claude Code's normal dialog appears. If the Claude app, a terminal or an IDE is the foreground window, the overlay skips the wait entirely (`interactive.skipWhenFocused`), because you can see the dialog anyway. Banners are click-through except while the pointer is over one with buttons, and the overlay never takes keyboard focus.

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
| `toasts.watchApps` | app names / AUMIDs to mirror (default `["Claude"]`); add other apps if you like |
| `limits.*` | which windows trigger the logo (`five_hour`, `seven_day`, `spend_limit`) and `minUsedPercentage` |
| `interactive.enabled`, `interactive.waitMs`, `interactive.skipWhenFocused` | answer questions and permission requests from the banner; how long to wait; foreground process names that skip the wait |

`events.log` in the same folder records everything the overlay receives.

## Limits

- DirectX **exclusive** fullscreen cannot be drawn over by any window. Use borderless/windowed fullscreen or leave Windows' fullscreen optimizations on (default).
- `rate_limits` only exist for Pro/Max/Team subscriptions and only after a Claude Code session has made a request in that window. A window used purely from Chat has no known reset time.
- The Claude app sends toasts only while its window is not focused, which is exactly when you need them.

## Development

```bash
npm install
npm start              # dev mode (self-installs with dev paths)
npm run test-notify    # banner test
npm run test-logo      # logo test
npm run dist           # icon + package + dist/ClaudeOverlay-win64.zip
```

Running `npm start` points the hooks at the dev build; running the packaged exe points them back. Last one wins.

Structure: `app/main.js` (overlay, HTTP endpoint on 127.0.0.1:47821, limit timers, toast bridge), `app/overlay.html` (banner + logo UI), `app/installer.js` (hooks, status line, startup shortcut), `hooks/claude-overlay-*.ps1` (what Claude Code and the toast listener run).

## Privacy and safety

Everything runs locally. The overlay listens only on `127.0.0.1`, reads notifications from your own Windows notification center, and never sends anything anywhere. The hooks it adds are observers: they print nothing and exit 0, so Claude Code's own permission dialogs and decisions are untouched. You can inspect every script in `hooks/`.

Independent community project, not affiliated with or endorsed by Anthropic.

## License

MIT

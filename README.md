# Claude Overlay

[![Release](https://img.shields.io/github/v/release/goktugzaaa/claude-overlay?label=download)](https://github.com/goktugzaaa/claude-overlay/releases/latest)
[![Build](https://github.com/goktugzaaa/claude-overlay/actions/workflows/release.yml/badge.svg)](https://github.com/goktugzaaa/claude-overlay/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

iPhone-style, always-on-top notifications for the **Claude desktop app** (Chat, Cowork, Code) and **Claude Code** CLI on Windows.
Shows up over fullscreen games and videos, where Windows hides its own toasts. One 4.6 MB exe, no installer, no runtime to install.

![Demo: permission answered from the overlay, question answered from the overlay, limit-reset logo, task-finished banner](docs/demo.gif)

- **Reply ready / task finished** → banner at the top of the screen with the conversation or project name and, for Claude Code, a summary of the last message.
- **Asking permission** → banner with the exact tool call (`Bash: npm run build && git push …`) and **Allow / Deny** buttons. Your click is the answer; Claude Code continues without you leaving the game.
- **Asking a question** → banner with the question and its options as buttons, plus **Other…** to type your own answer. Click or type, and Claude gets the answer.
- **Usage limit reset** (5-hour, weekly, spend limit) → animated Claude logo in the top-right corner, even if no session is open.
- Click-through, never steals focus, follows the monitor your cursor is on, optional soft chime.

## What Claude already does, and what this adds

Claude's desktop app shows Windows toasts, and the Claude mobile app notifies you about cloud sessions and about local sessions you put under **Remote Control**, which also lets you drive those sessions from the phone. If that covers you, you do not need the phone part of this tool for Claude sessions.

What this adds on top:

- Windows suppresses toasts over fullscreen games and videos; the overlay is drawn anyway.
- Permission requests and questions are answered on the spot, without switching to the Claude window.
- A visible signal when your usage window resets; Claude does not notify about that.
- The same banners for Codex and any other app's notifications, and phone relay for sessions that are not under Remote Control (plain CLI, Cowork, Codex), with one-tap Allow / Deny.
- Everything local, no account.

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

For `PermissionRequest` and `AskUserQuestion` the hook keeps its request to the overlay open for `interactive.waitMs` (default 20 s). A click on **Allow**, **Deny** or an option is returned as the hook's JSON output (`decision` / `updatedInput.answers`), so Claude Code proceeds immediately. No click → the hook returns nothing and Claude Code's normal dialog appears. If the Claude app, a terminal or an IDE is the foreground window, the overlay skips the wait entirely (`interactive.skipWhenFocused`), because you can see the dialog anyway. The overlay is click-through except over a card with buttons. It takes keyboard focus only while you type into the **Other…** field (the wait extends to 3 minutes for that) and hands focus back to the window that had it as soon as you send or press Esc.

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
| `phone.*` | Telegram token / chat id, ntfy server / topic, `onlyWhenIdleMs`, `sendUnansweredAfterMs`, `deferWindowMs`, `holdMs`, per-event switches (`stop`, `permission`, `question`, `limit`) |

`events.log` in the same folder records everything the overlay receives. The PowerShell scripts Claude Code runs are written to `hooks\` in that folder on every start.

## Phone (Telegram / ntfy)

The same banners on your phone, and permission requests and questions answered from there, with no server of your own. Off until you add a token or topic. By default a message only goes to the phone once the PC has been idle for 3 minutes (`phone.onlyWhenIdleMs`; 0 = always): if you are at the PC when something happens, nothing is sent; if you walk away afterwards, the latest event is sent as soon as the 3 idle minutes pass (within `phone.deferWindowMs`, 15 minutes). A permission request or question that nobody answers on the PC within a minute goes to the phone even if the PC is not idle (`phone.sendUnansweredAfterMs`). While a phone channel is configured, those cards stay open for up to 8 minutes (`phone.holdMs`) so an answer from the phone can arrive; the PC card can be answered at any time.

**Telegram, two-way**

1. In Telegram open **@BotFather**, send `/newbot`, pick a name; copy the token.
2. Tray → *Open config file* → put the token in `phone.telegram.token`, save, tray → *Reload config*.
3. Open the chat with your new bot and send `/start`. The overlay pairs itself with that chat.
4. Permission requests arrive with **Allow / Deny** buttons and questions with their options; a tap answers exactly like a click on the PC card. A typed reply answers a question with free text (`allow` / `deny`, `evet` / `hayır` work for permissions). Whichever side answers first wins; the PC card shows "Sent" and the Telegram message is updated.

**ntfy, one-way**

Install the ntfy app, subscribe to a long random topic name, put it in `phone.ntfy.topic` (`phone.ntfy.server` for a self-hosted ntfy). Banners arrive as push notifications, without buttons.

Tray → *Phone: send test message* checks the setup, ignoring the idle rule. Messages pass through Telegram's or ntfy's servers; the bot token lives only in your config.json.

## Compatibility

| Agent / app | What you get | How |
|---|---|---|
| Claude desktop app: Chat, Cowork | reply ready, question text (what the app's own notification carries) | notification-center bridge, automatic |
| Claude Code: Code tab, CLI | everything: task finished with summary, Allow / Deny, answer questions (buttons or typed), limit reset | hooks, installed automatically |
| Codex CLI / Codex app | task finished with the last message | `notify` adapter: tray → **Codex CLI: enable notifications** (edits `~/.codex/config.toml`; a `notify` you already had keeps running, and disable restores it) |
| Any other app (Cursor, Windsurf, ChatGPT desktop, …) | its Windows notifications, mirrored over fullscreen apps | add the app name to `toasts.watchApps` |

Codex under WSL: the adapter is a Windows script, so point `notify` in the WSL-side `~/.codex/config.toml` at it through interop:

```toml
notify = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "C:/Users/<you>/AppData/Roaming/claude-notification/hooks/claude-overlay-codex.ps1"]
```

Agents without a hook or notify mechanism cannot offer buttons; when one gains hooks, an adapter is a small script in `hooks/`. Open an issue with the agent name.

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

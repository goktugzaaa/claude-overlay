# HeadsUp

Independent community project, not affiliated with or endorsed by Anthropic.

[![Release](https://img.shields.io/github/v/release/goktugzaaa/headsup?label=download)](https://github.com/goktugzaaa/headsup/releases/latest)
[![Build](https://github.com/goktugzaaa/headsup/actions/workflows/release.yml/badge.svg)](https://github.com/goktugzaaa/headsup/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

Always-on-top notifications for **Claude Code** and the **Claude desktop app** (Chat, Cowork, Code) on Windows: answer permission prompts and questions right from the banner, even over fullscreen games and videos, where Windows hides its own toasts. One exe under 5 MB, no installer, no runtime to install.

![Demo: a permission request and a question answered from the banner, the limit-reset logo and a task-finished banner](docs/demo.gif)

- **Reply ready / task finished** → banner at the top of the screen with the conversation or project name and, for Claude Code, the start of the last message.
- **Asking permission** → banner with the full tool call (the command with its line breaks, the file and a preview of the change for edits, every field of an MCP tool) and **Allow / Deny** buttons. Hidden or text-direction Unicode characters show up as `⟦U+202E⟧`, and a cut is always marked (`… (+N more chars)`). Your click is the answer; Claude Code continues without you leaving the game.
- **Asking a question** → banner with the question and its options as buttons, plus **Other…** to type your own answer. Click or type, and Claude gets the answer.
- **Usage limit reset** (5-hour, weekly, spend limit) → animated logo in the top-right corner, even if no session is open.
- Click-through, never steals focus, follows the monitor your cursor is on, optional soft chime.

## What Claude already does, and what this adds

Claude's desktop app shows Windows toasts, and the Claude mobile app notifies you about cloud sessions and about local sessions you put under **Remote Control**, which also lets you drive those sessions from the phone. If that covers you, you do not need the phone part of this tool for Claude sessions.

What this adds on top:

- Windows suppresses toasts over fullscreen games and videos; the overlay is drawn anyway.
- Permission requests and questions are answered on the spot, without switching to the Claude window.
- A visible signal when your usage window resets; Claude Code only tells you when a session was waiting to auto-resume.
- The same banners for Codex and any other app's notifications, and phone relay for sessions that are not under Remote Control (plain CLI, Cowork, Codex), with one-tap Allow / Deny.
- Runs locally, no account. Nothing leaves the PC unless you set up the phone relay.

## Install

1. Download `HeadsUp.exe` from [Releases](../../releases/latest) and put it anywhere, e.g. `%LOCALAPPDATA%\Programs\HeadsUp`.
2. Run it. SmartScreen will warn once because the exe is not code-signed (certificates cost money; the code is all here): click **More info → Run anyway**. On first start it:
   - adds its hooks to `~/.claude/settings.json` (backup: `settings.json.headsup.bak`, or the `settings.json.claude-overlay.bak` an earlier Claude Overlay install made),
   - wraps your existing status line command (its output is preserved; the command runs through Git Bash, or `cmd` when Git Bash is not installed),
   - adds itself to the Startup folder.
3. Windows: **Settings → Privacy & security → Notifications → "Let desktop apps access notifications"** must be on. The overlay warns you if it is not; the tray menu opens the setting.
4. Claude app: keep notifications enabled (Settings → Notifications). Chat and Cowork signals come from there.

Needs the WebView2 runtime, which Windows 11 and up-to-date Windows 10 already have ([download](https://developer.microsoft.com/microsoft-edge/webview2/) otherwise).

A tray icon (a teal square with two white chevrons and an amber dot) gives you test buttons, config, log, and uninstall. If the overlay is not running, the next hook starts it, except after tray → **Quit** (it then stays off until you start it yourself or log on again) and never from a Claude Code session running as Administrator. If you move the exe, run it once more and it re-points the hooks.

Uninstall: tray → "Remove hooks", or `HeadsUp.exe --uninstall`. This removes the hooks, restores your original status line and Codex `notify`, removes the Startup shortcut and quits; hooks no longer start it. Starting the exe by hand installs everything again. `%USERPROFILE%\.headsup` (config, log, key) is kept; delete it to remove every trace.

### Upgrading from Claude Overlay 2.x

HeadsUp is the new name of Claude Overlay. Run `HeadsUp.exe` once; if the old version is running, HeadsUp asks it to quit first. Your settings and Telegram pairing move over automatically (they are copied from `%APPDATA%\claude-notification` to `%USERPROFILE%\.headsup`), and the hooks, the status line, the Codex `notify` and the Startup shortcut are re-pointed to HeadsUp. The old `ClaudeOverlay.exe` is renamed to `ClaudeOverlay.exe.old`: delete it, and the old `Claude Overlay` Startup shortcut if it is still there. Restart Claude Code sessions that were open during the upgrade: they still call the old hook scripts. If HeadsUp says the old version did not quit, quit it from its tray icon and run `HeadsUp.exe` again; open Claude Code sessions start the old version again through their hooks, so close them first if it keeps coming back. If you pointed a WSL-side Codex `notify` at the adapter, change its path to `C:/Users/<you>/.headsup/hooks/headsup-codex.ps1` (see [Compatibility](#compatibility)).

## How it works

| Signal | Covers | Mechanism |
|---|---|---|
| Windows notification center | Chat, Cowork, Code | Every toast the Claude app emits is read through `UserNotificationListener` and re-rendered by the overlay. Works while Windows itself suppresses toasts (fullscreen, Do Not Disturb). |
| `Stop` hook | Code tab, CLI | Task finished + the start of the last assistant message. |
| `PermissionRequest` hook | Code tab, CLI | Tool name and input (command, file, URL); Allow / Deny from the banner. |
| `PreToolUse` hook for `AskUserQuestion` | Code tab, CLI | The question and its options; answer from the banner. |
| `Notification` hook | Code tab, CLI | `permission_prompt`, `agent_needs_input`, `quota_auto_resume_fired` (limit reset while auto-resuming). |
| Status line `rate_limits` | Code tab, CLI | `resets_at` of every window is stored; when it passes, the logo shows. The 5-hour/weekly windows are shared across Chat, Cowork and Code, so one Code session per window is enough to learn the reset time. |

When the same moment produces both a hook event and a toast, only one banner is shown (the hook, which carries more detail).

### Answering from the banner

For `PermissionRequest` and `AskUserQuestion` the hook keeps its request to the overlay open for `interactive.waitMs` (default 20 s; up to `phone.holdMs`, 8 minutes, when a phone channel is set up). The hooks are installed with a timeout longer than that, and the overlay repairs a shorter one on start; if Claude Code still gives up on the question hook, the open card moves over to the permission request Claude Code sends next, so the answer still arrives. Answers are keyed by the exact question text, so questions with Markdown or special characters are matched correctly. A click on **Allow**, **Deny** or an option is returned as the hook's JSON output (`decision` / `updatedInput.answers`), so Claude Code proceeds immediately. No click → the hook returns nothing and Claude Code's normal dialog appears. Without a phone channel, a focused Claude window, terminal or IDE while you are at the PC (input within the last 30 s, `interactive.skipOnlyIfActiveMs`) skips the card (`interactive.skipWhenFocused`), because you can see the dialog anyway. With a phone channel configured the card always shows: a question often arrives seconds after you sent the prompt, and only an open card can be relayed to the phone later. The overlay is click-through except over a card with buttons. It takes keyboard focus only while you type into the **Other…** field (the wait extends to 3 minutes for that) and hands focus back to the window that had it as soon as you send, press Esc, or the card is answered elsewhere.

A card's buttons ignore clicks for 0.7 s after it appears or moves, and until the mouse has moved over it, so a click meant for the window underneath never answers it (the buttons look dimmed until then). The newest card is on top; when more cards are open than fit on screen, the rest wait behind a **+N** badge and come in as you answer. When Claude Code drops a request (you interrupted it in the terminal, or the hook timed out), its card closes by itself, on the PC and on the phone.

## Configuration

`%USERPROFILE%\.headsup\config.json` (tray → "Open config file", then "Reload config").

The settings live next to `~/.claude` rather than in AppData on purpose: the Claude desktop app is an MSIX package, and Windows redirects the AppData writes of processes it starts (Code tab hooks, and the overlay a hook starts) into the app's private copy, so an AppData folder would hold different files inside and outside the Claude app.

| Key | Meaning |
|---|---|
| `language` | `auto` (OS locale), `en`, `tr` |
| `sound` | chime on banners and logo |
| `notification.durationMs`, `logo.durationMs` | how long things stay on screen |
| `notification.showLastMessage` | include the start of Claude's last message in the task-finished banner |
| `events.*` | per-event switches: `stop`, `permission_request`, `question`, `permission_prompt`, `idle_prompt`, `agent_needs_input`, `quota_auto_resume_fired` |
| `toasts.watchApps` | app names / AUMIDs to mirror (default `["Claude"]`); add `"Codex"`, `"ChatGPT"`, `"Cursor"` or any app whose toasts you want on top of fullscreen apps |
| `limits.*` | which windows trigger the logo (`five_hour`, `seven_day`, `spend_limit`) and `minUsedPercentage` |
| `interactive.enabled`, `interactive.waitMs`, `interactive.skipWhenFocused` | answer questions and permission requests from the banner; how long to wait; foreground process names that skip the wait |
| `remote.*` | remote sessions: `enabled`, `claudePath`, `model`, `permissionMode`, `projects`, `maxRunMinutes`, `busyGuardSeconds` |
| `phone.*` | `enabled`, Telegram token / chat id, ntfy server / topic / token, `onlyWhenIdleMs`, `sendUnansweredAfterMs`, `deferWindowMs`, `holdMs`, per-event switches (`stop`, `permission`, `question`, `limit`) |

`events.log` in the same folder records what the overlay receives: event types, tool names, project folder names, card ids and outcomes (allow / deny / timeout, number of answers), limit reset times and errors. It does not record commands, questions, answers, message or toast text, and other fields are masked for secrets. It rolls over to `events.log.1` at 2 MB. The PowerShell scripts Claude Code runs are written to `hooks\` in that folder on every start.

## Phone (Telegram / ntfy)

The same banners on your phone, and permission requests and questions answered from there, with no server of your own. Off until you add a token or topic. By default a message only goes to the phone once the PC has been idle for 3 minutes (`phone.onlyWhenIdleMs`; 0 = always): if you are at the PC when something happens, nothing is sent; if you walk away afterwards, the latest event is sent as soon as the 3 idle minutes pass (within `phone.deferWindowMs`, 15 minutes). A permission request or question that nobody answers on the PC within a minute goes to the phone even if the PC is not idle (`phone.sendUnansweredAfterMs`). While a phone channel is configured, those cards stay open for up to 8 minutes (`phone.holdMs`) so an answer from the phone can arrive; the PC card can be answered at any time.

**Telegram, two-way**

1. In Telegram open **@BotFather**, send `/newbot`, pick a name; copy the token. Use one bot per PC: two PCs (or Windows accounts) polling the same bot take each other's taps, and the overlay warns when it sees that.
2. Tray → *Open config file* → put the token in `phone.telegram.token`, save, tray → *Reload config*. A banner shows an 8-character pairing code; tray → *Phone: show Telegram pairing code* shows it again.
3. Open a private chat with your new bot and send that code (`/start CODE` works too). The overlay pairs itself with that chat and names the account it paired with. Only a private chat can pair, and only that chat is obeyed. After 10 wrong codes the code is blocked until you show a new one from the tray. To pair a different chat, clear `phone.telegram.chatId` and reload the config.
4. Permission requests arrive with **Allow / Deny** buttons and questions with their options; a tap answers exactly like a click on the PC card. A card with several questions goes through them one at a time (1/3, 2/3, …); a question that allows several choices has ☐ / ☑ buttons and **✓ Done**. **✍️ Other…** opens Telegram's reply box for a typed answer. A permission request is answered only by its buttons or by a reply to that card's message (`allow` / `yes` / `ok` / `evet` / `izin`, or `deny` / `no` / `hayır` / `reddet`); a message that is not a reply never approves anything, it only answers the newest open question. Whichever side answers first wins; the PC card closes and the Telegram message is updated.

**ntfy, one-way**

Install the ntfy app, subscribe to a topic, put it in `phone.ntfy.topic` (`phone.ntfy.server` for a self-hosted ntfy). Banners arrive as push notifications, without buttons. Topics on ntfy.sh are public: anyone who knows or guesses the name can read the messages, which ntfy.sh keeps for about 12 hours. Use a long random topic name, or a reserved or access-controlled topic with its access token in `phone.ntfy.token` (sent as `Authorization: Bearer`). The overlay warns when the topic looks short or guessable and no token is set.

Tray → *Phone: send test message* checks the setup, ignoring the idle rule. What is sent, and how it is protected, is listed under [Privacy and safety](#privacy-and-safety).

## Remote sessions from the phone (Telegram)

With Telegram set up, the paired chat can also **start a new Claude Code session** in a project, or **send a message to an existing session**, while you are away. Off by default: tray → *Phone: remote sessions*.

| Command | What it does |
|---|---|
| `/new` | pick a project (one the PC has already opened in Claude Code, or listed in `remote.projects`); your next message starts a session there |
| `/sessions` | pick one of the recent sessions; your next message goes to it |
| `/status`, `/stop` | what is running; stop it (kills the process tree) |
| `/exit` | clear the selection |
| `/model [name]` | model for remote runs (`/model default` = your Claude Code default) |

After a run, just reply to continue the same session. The overlay runs `claude -p --output-format stream-json` (the Claude Code CLI must be installed; `remote.claudePath` if it is not found), keeps one status message updated with the current step, and sends the final answer. Permission requests from a remote run go to the phone at once with **Allow / Deny**; with no answer the tool is denied. In headless mode Claude cannot open its question picker, so it asks in plain text and you answer by replying.

Safety: only the paired private chat is obeyed; the working directory always comes from the known-project list, never from message text; the prompt goes to the CLI on stdin, never through a shell; `remote.permissionMode` accepts only `default`, `plan` and `acceptEdits` (`acceptEdits` applies file edits without asking; `auto`, `bypassPermissions` and anything else fall back to `default`), so commands still wait for your **Allow**; a run gets your normal user environment, not that of whichever terminal started the overlay; one run at a time, stopped after `remote.maxRunMinutes` (30), and `/stop`, the time limit or quitting the overlay end the run's whole process tree; while the overlay itself runs as Administrator, remote runs are refused. Prompts and results pass through Telegram's servers (masked for secrets, but still your code), so do not use this on code you would not paste into a chat. A session that changed in the last 90 seconds (`remote.busyGuardSeconds`) is probably open on the PC; sending to it asks for confirmation, because Claude Code has no lock and the two conversations would interleave. For live control of a session that stays open in the Claude app, Claude's own Remote Control is the better fit.

## Compatibility

| Agent / app | What you get | How |
|---|---|---|
| Claude desktop app: Chat, Cowork | reply ready, question text (what the app's own notification carries) | notification-center bridge, automatic |
| Claude Code: Code tab, CLI | everything: task finished with the start of the last message, Allow / Deny, answer questions (buttons or typed), limit reset | hooks, installed automatically |
| Codex CLI / Codex app | task finished with the start of the last message | `notify` adapter: tray → **Codex CLI: enable notifications** (edits `~/.codex/config.toml`; a `notify` you already had keeps running, unless it is a `.bat` / `.cmd` script, which is not chained; disable restores it) |
| Any other app (Cursor, Windsurf, ChatGPT desktop, …) | its Windows notifications, mirrored over fullscreen apps | add the app name to `toasts.watchApps` |

Codex under WSL: the adapter is a Windows script, so point `notify` in the WSL-side `~/.codex/config.toml` at it through interop:

```toml
notify = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "C:/Users/<you>/.headsup/hooks/headsup-codex.ps1"]
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

`dist.ps1` builds and writes `dist/HeadsUp.exe` and `dist/HeadsUp-win64.zip`. Releases are built by GitHub Actions from the tagged commit and carry `SHA256SUMS.txt` and a build provenance attestation (`gh attestation verify HeadsUp.exe -R goktugzaaa/headsup`). Run a build without touching your settings: set `HEADSUP_NO_INSTALL=1` and `HEADSUP_PORT=47899` (the 2.x names `CLAUDE_OVERLAY_NO_INSTALL` and `CLAUDE_OVERLAY_PORT` still work), start it, then run `HeadsUp.exe --send test-question` (or `test-permission`, `test-notify`, `test-logo`) with the same `HEADSUP_PORT`. A test run does not move Claude Overlay 2.x's data folder; it stops with a notice until that has happened. The endpoint only accepts requests signed with the key in `secret.key`, so plain `curl` posts are refused.

Structure: `src-tauri/src/main.rs` (window, commands, CLI flags), `events.rs` (hook events, interactive cards, toast bridge, limit timers, cursor polling), `installer.rs` (hooks, status line, Startup shortcut), `http.rs` (signed 127.0.0.1 endpoint), `phone.rs` / `telegram.rs` (phone relay), `remote.rs` (remote sessions), `mask.rs` (secret masking), `tray.rs`, `i18n.rs`; `app/overlay.html` (banner + logo UI, embedded in the exe); `hooks/*.ps1` (what Claude Code and the toast listener run, embedded too).

## Privacy and safety

**On the PC.** The overlay listens only on `127.0.0.1`. Every request to it, except a bare health check, must be signed with a per-install key (32 random bytes in `%USERPROFILE%\.headsup\secret.key`), and its answers to permission requests and questions are signed back. So neither a web page in your browser nor another program that grabs the port can show cards or answer them; requests that come from a browser are refused outright. The hooks only decide something when you click a button on a card or tap one on the phone; otherwise they print nothing and exit 0, so Claude Code's own dialogs are untouched. You can inspect every script in `hooks/`. The toast listener can read every app's notifications in your Windows notification center, but only those of the apps in `toasts.watchApps` are shown or forwarded, and their text is never logged.

**What leaves the PC.** Nothing, unless you set a Telegram token or an ntfy topic. Then the following goes to `api.telegram.org` or your ntfy server: the tool name and the command, path or input of permission requests (up to 900 characters); questions, their options and your answers; the first 180 characters of Claude's last message; the text of notifications from the apps in `toasts.watchApps`; limit-reset notices; and, with remote sessions on, your prompts and the run's progress and results. Before anything is sent, text that looks like a secret is masked: API keys and tokens with known prefixes, `password=…` / `token: …` style values, `Bearer` / `Basic` credentials, passwords in URLs, private key blocks and long random-looking strings. Masking is a safety net, not a guarantee. The PC card is never masked, so you see exactly what you approve. Telegram cloud chats are not end-to-end encrypted, and ntfy.sh topics are public to anyone who knows the name (see [ntfy](#phone-telegram--ntfy)).

**Tokens in config.json.** The Telegram bot token and the ntfy token are stored in plain text in `%USERPROFILE%\.headsup\config.json`. Anyone with the bot token controls the bot and can read your taps and replies, so do not share that file or `secret.key`. Neither appears in `events.log`. If the bot token leaks, revoke it in @BotFather (`/revoke`), put the new one in the config and reload it.

**Control from the phone.** A new pairing needs the one-time code shown on the PC, and only that private chat is obeyed. A permission request is approved only with its button or a reply to that card. Remote sessions are off by default, run only in projects the PC already knows, never in a mode that runs commands without asking (`acceptEdits`, the most permissive one allowed, only applies file edits on its own), and never while the overlay runs as Administrator.

**Administrator sessions.** A hook in a Claude Code session running as Administrator never starts the overlay, so the overlay does not inherit admin rights; start it normally (or let the Startup shortcut start it at logon).

**Removing it.** Tray → *Remove hooks* or `--uninstall` removes the hooks, restores the status line and the Codex `notify`, removes the Startup shortcut and quits; hooks no longer start it. Your settings folder `%USERPROFILE%\.headsup` is kept until you delete it.

## License

MIT

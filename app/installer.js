// Installs / removes the Claude Code hooks, the status line wrapper and the Windows
// startup shortcut. Used by `ClaudeOverlay.exe --install`, by the first run, and by
// `npm run install-hooks` in dev mode.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const P = require('./paths');

const MARKERS = ['claude-overlay-', 'claude-notification/hooks/', 'ClaudeOverlay/resources/hooks/'];
const isOurCommand = (c) => typeof c === 'string' && MARKERS.some((m) => c.includes(m));
// Bump when the set of hooks changes so existing installs re-run the installer on next start.
const INSTALL_VERSION = 3;
const CLAUDE_DIR = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const SETTINGS = path.join(CLAUDE_DIR, 'settings.json');
const BACKUP = path.join(CLAUDE_DIR, 'settings.json.claude-overlay.bak');
const STARTUP_DIR = path.join(process.env.APPDATA || '', 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup');
const SHORTCUT = path.join(STARTUP_DIR, 'Claude Overlay.lnk');

const fwd = (p) => p.replace(/\\/g, '/');
const psq = (s) => "'" + String(s).replace(/'/g, "''") + "'";

function psCommand(script, arg) {
  return `powershell -NoProfile -ExecutionPolicy Bypass -File "${fwd(script)}"${arg ? ' ' + arg : ''}`;
}

function isOurs(entry) {
  return (entry.hooks || []).some((h) => isOurCommand(h.command));
}

function stripOurHooks(settings) {
  if (!settings.hooks) return;
  for (const event of Object.keys(settings.hooks)) {
    const list = Array.isArray(settings.hooks[event]) ? settings.hooks[event] : [];
    const kept = list.filter((e) => !isOurs(e));
    if (kept.length) settings.hooks[event] = kept;
    else delete settings.hooks[event];
  }
  if (!Object.keys(settings.hooks).length) delete settings.hooks;
}

function createShortcut(exe, args) {
  if (!fs.existsSync(STARTUP_DIR)) return false;
  const ps = [
    '$s = (New-Object -ComObject WScript.Shell).CreateShortcut(' + psq(SHORTCUT) + ');',
    '$s.TargetPath = ' + psq(exe) + ';',
    '$s.Arguments = ' + psq(args.map((a) => '"' + a + '"').join(' ')) + ';',
    '$s.WorkingDirectory = ' + psq(path.dirname(exe)) + ';',
    '$s.WindowStyle = 7;',
    '$s.Description = "Claude Code overlay notifications";',
    '$s.Save()',
  ].join(' ');
  const r = spawnSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', ps], { encoding: 'utf8', windowsHide: true });
  return r.status === 0;
}

function install() {
  P.ensureUserData();
  const lines = [];
  const spec = P.launchSpec();

  // Runtime config: where the overlay lives so hooks can autostart it.
  const config = P.readJson(P.CONFIG_PATH, {});
  config.overlayExe = spec.exe;
  config.overlayArgs = spec.args;

  // Claude Code settings.
  fs.mkdirSync(CLAUDE_DIR, { recursive: true });
  const settings = P.readJson(SETTINGS, {});
  if (!fs.existsSync(BACKUP) && fs.existsSync(SETTINGS)) fs.copyFileSync(SETTINGS, BACKUP);
  stripOurHooks(settings);
  settings.hooks = settings.hooks || {};
  settings.hooks.Stop = settings.hooks.Stop || [];
  settings.hooks.Stop.push({
    hooks: [{ type: 'command', command: psCommand(path.join(P.HOOKS_DIR, 'claude-overlay-send.ps1'), 'stop'), timeout: 15 }],
  });
  settings.hooks.Notification = settings.hooks.Notification || [];
  settings.hooks.Notification.push({
    matcher: 'permission_prompt|idle_prompt|agent_needs_input|quota_auto_resume_fired',
    hooks: [{ type: 'command', command: psCommand(path.join(P.HOOKS_DIR, 'claude-overlay-send.ps1'), 'notification'), timeout: 15 }],
  });
  // Observers only: they print nothing and exit 0, so Claude Code's own dialogs proceed unchanged.
  settings.hooks.PermissionRequest = settings.hooks.PermissionRequest || [];
  settings.hooks.PermissionRequest.push({
    hooks: [{ type: 'command', command: psCommand(path.join(P.HOOKS_DIR, 'claude-overlay-send.ps1'), 'permission'), timeout: 10 }],
  });
  settings.hooks.PreToolUse = settings.hooks.PreToolUse || [];
  settings.hooks.PreToolUse.push({
    matcher: 'AskUserQuestion',
    hooks: [{ type: 'command', command: psCommand(path.join(P.HOOKS_DIR, 'claude-overlay-send.ps1'), 'question'), timeout: 10 }],
  });

  const current = settings.statusLine && settings.statusLine.type === 'command' ? settings.statusLine.command : null;
  if (!current || !isOurCommand(current)) config.originalStatusLineCommand = current || null;
  settings.statusLine = { ...(settings.statusLine || {}), type: 'command', command: psCommand(path.join(P.HOOKS_DIR, 'claude-overlay-statusline.ps1')) };

  P.writeJson(SETTINGS, settings);
  lines.push('Claude Code hook\'ları kuruldu: ' + SETTINGS);
  if (config.originalStatusLineCommand) lines.push('Önceki statusline korunup sarmalandı.');

  config.installedAt = new Date().toISOString();
  config.installedVersion = INSTALL_VERSION;
  P.writeJson(P.CONFIG_PATH, config);

  if (createShortcut(spec.exe, spec.args)) lines.push('Windows açılışında otomatik başlatma eklendi.');
  else lines.push('Başlangıç kısayolu oluşturulamadı (elle ekleyebilirsiniz).');

  return lines;
}

function uninstall() {
  const lines = [];
  const settings = P.readJson(SETTINGS, {});
  const config = P.readJson(P.CONFIG_PATH, {});
  stripOurHooks(settings);
  if (settings.statusLine && isOurCommand(settings.statusLine.command)) {
    if (config.originalStatusLineCommand) settings.statusLine.command = config.originalStatusLineCommand;
    else delete settings.statusLine;
  }
  P.writeJson(SETTINGS, settings);
  lines.push('Hook\'lar ve statusline geri alındı.');
  try {
    fs.unlinkSync(SHORTCUT);
    lines.push('Başlangıç kısayolu silindi.');
  } catch {}
  delete config.installedAt;
  delete config.installedVersion;
  P.writeJson(P.CONFIG_PATH, config);
  return lines;
}

function isInstalled() {
  const config = P.readJson(P.CONFIG_PATH, {});
  if (!config.installedAt || config.installedVersion !== INSTALL_VERSION) return false;
  const spec = P.launchSpec();
  return config.overlayExe === spec.exe; // re-install if the exe moved
}

module.exports = { install, uninstall, isInstalled, SETTINGS, SHORTCUT };

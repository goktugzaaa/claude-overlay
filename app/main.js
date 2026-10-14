// Claude Overlay — always-on-top, click-through notification layer for the Claude desktop app
// (Chat, Cowork, Code) and Claude Code CLI.
//
// Signals:
//   1. Windows notification center (UserNotificationListener) — every toast the Claude app
//      emits (Chat / Cowork / Code) is mirrored as an overlay banner. Works in fullscreen.
//   2. Claude Code hooks via hooks/claude-overlay-send.ps1 — Stop (task done + last message),
//      PermissionRequest (tool + command, answerable), PreToolUse:AskUserQuestion (the question,
//      answerable), Notification.
//   3. Claude Code status line (rate_limits) via hooks/claude-overlay-statusline.ps1 — limit reset timers.
//
// Interactive events (question, permission) keep the hook's HTTP request open until the user
// clicks a button on the banner or the wait expires; the reply becomes the hook's JSON output.
//
// CLI:  ClaudeOverlay.exe [--install | --uninstall | --send <type>]

const { app, BrowserWindow, screen, ipcMain, Tray, Menu, nativeImage, shell, dialog } = require('electron');
const http = require('http');
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const P = require('./paths');
const installer = require('./installer');
const i18n = require('./i18n');

const argv = process.argv.slice(P.PACKAGED ? 1 : 2);
const CLI = argv.find((a) => a === '--install' || a === '--uninstall' || a === '--send');

P.ensureUserData();
let config = P.loadConfig();
let L = i18n.pick(config.language, 'en');
const T = () => L.t;
const PORT = Number(process.env.CLAUDE_OVERLAY_PORT || config.port || 47821);
const WINDOW_HEIGHT = 380;

function reloadConfig() {
  config = P.loadConfig();
  L = i18n.pick(config.language, app.isReady() ? app.getLocale() : 'en');
  if (win) win.webContents.send('config', config);
  if (tray) buildTrayMenu();
}

// ---------------------------------------------------------------- CLI mode
if (CLI) {
  app.whenReady().then(async () => {
    try {
      if (CLI === '--install') {
        const lines = installer.install();
        await post('reload-config', {}).catch(() => {});
        await post('test-notify', {}).catch(() => {});
        report('Claude Overlay installed', lines.join('\n'));
      } else if (CLI === '--uninstall') {
        const lines = installer.uninstall();
        await post('quit', {}).catch(() => {});
        report('Claude Overlay removed', lines.join('\n'));
      } else if (CLI === '--send') {
        const type = argv[argv.indexOf('--send') + 1] || 'test-notify';
        await post(type, {}).catch((e) => report('Overlay did not respond', String(e.message)));
      }
    } catch (err) {
      report('Error', String(err && err.stack ? err.stack : err));
    }
    app.quit();
  });
} else {
  startOverlay();
}

function report(title, message) {
  if (P.PACKAGED) dialog.showMessageBoxSync({ type: 'info', title, message: title, detail: message });
  else console.log(title + '\n' + message);
}

function post(type, payload) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify(payload || {});
    const req = http.request(
      { host: '127.0.0.1', port: PORT, method: 'POST', path: '/' + type, timeout: 1500,
        headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) } },
      (res) => { res.resume(); res.on('end', () => resolve(res.statusCode)); }
    );
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', reject);
    req.end(body);
  });
}

// ---------------------------------------------------------------- overlay
let win = null;
let tray = null;
let activeItems = 0;
let hideTimer = null;
let state = { windows: {} };
let lastHookBannerAt = 0;
let lastPermissionAt = 0;
let listenerProc = null;
let listenerStatus = 'starting';
let foregroundProcess = '';

function startOverlay() {
  if (!app.requestSingleInstanceLock()) {
    app.quit();
    return;
  }
  app.commandLine.appendSwitch('disable-renderer-backgrounding');
  app.whenReady().then(() => {
    L = i18n.pick(config.language, app.getLocale());
    loadState();
    createWindow();
    startServer();
    scheduleAllTimers();
    startToastListener();
    win.webContents.on('did-finish-load', () => {
      win.webContents.send('config', config);
      if (!installer.isInstalled()) {
        // First run (or the exe moved, or the hook set changed): wire everything up.
        try {
          const lines = installer.install();
          config = P.loadConfig();
          logLine(['install', ...lines]);
          notify({ title: 'Claude Overlay', subtitle: T().installed, body: T().installedBody, kind: 'done' });
        } catch (err) {
          notify({ title: 'Claude Overlay', subtitle: T().installError, body: String(err.message), kind: 'permission' });
        }
      }
    });
  });
  app.on('second-instance', () => handleEvent({ type: 'test-notify' }));
  app.on('window-all-closed', (e) => e.preventDefault());
  app.on('before-quit', () => {
    for (const id of [...pending.keys()]) finishInteractive(id, null);
    if (listenerProc) { try { listenerProc.kill(); } catch {} }
  });
}

function loadState() {
  state = { windows: {}, ...P.readJson(P.STATE_PATH, {}) };
  if (!state.windows) state.windows = {};
}
function saveState() {
  try { P.writeJson(P.STATE_PATH, state); } catch {}
}
function logLine(fields) {
  try { fs.appendFileSync(P.LOG_PATH, [new Date().toISOString(), ...fields].join('\t') + '\n'); } catch {}
}

function targetDisplay() {
  const point = screen.getCursorScreenPoint();
  return screen.getDisplayNearestPoint(point) || screen.getPrimaryDisplay();
}

function createWindow() {
  const display = targetDisplay();
  win = new BrowserWindow({
    x: display.bounds.x, y: display.bounds.y, width: display.bounds.width, height: WINDOW_HEIGHT,
    transparent: true, frame: false, show: false, focusable: false, resizable: false, movable: false,
    minimizable: false, maximizable: false, fullscreenable: false, skipTaskbar: true, hasShadow: false,
    alwaysOnTop: true, backgroundColor: '#00000000',
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false, backgroundThrottling: false },
  });
  // Click-through by default; `forward` lets the page see mouse moves so it can ask for
  // real mouse events while the pointer is over an interactive banner.
  win.setIgnoreMouseEvents(true, { forward: true });
  win.setAlwaysOnTop(true, 'screen-saver', 1);
  win.setMenu(null);
  win.loadFile(path.join(__dirname, 'overlay.html'));
  win.on('closed', () => { win = null; });
  win.on('hide', () => { try { win.setIgnoreMouseEvents(true, { forward: true }); } catch {} });
}

function showOverlay() {
  if (!win) return;
  clearTimeout(hideTimer);
  const d = targetDisplay();
  const b = win.getBounds();
  if (b.x !== d.bounds.x || b.y !== d.bounds.y || b.width !== d.bounds.width) {
    win.setBounds({ x: d.bounds.x, y: d.bounds.y, width: d.bounds.width, height: WINDOW_HEIGHT });
  }
  win.setAlwaysOnTop(true, 'screen-saver', 1); // last one to set TOPMOST wins
  if (!win.isVisible()) win.showInactive();
  else win.moveTop();
}

function sendToRenderer(channel, payload) {
  if (!win) return;
  activeItems += 1;
  showOverlay();
  win.webContents.send(channel, payload);
}

ipcMain.on('item-done', () => {
  activeItems = Math.max(0, activeItems - 1);
  if (activeItems === 0) {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => { if (win && activeItems === 0) win.hide(); }, 400);
  }
});

ipcMain.on('mouse', (_e, interactive) => {
  if (!win) return;
  logLine(['mouse', interactive ? 'capture' : 'passthrough']);
  try { win.setIgnoreMouseEvents(!interactive, { forward: true }); } catch {}
});

ipcMain.on('answer', (_e, data) => {
  if (!data || !data.id) return;
  const { id, ...rest } = data;
  finishInteractive(String(id), rest);
});

ipcMain.on('tray-icon', (_e, dataUrl) => {
  if (tray) return;
  try {
    tray = new Tray(nativeImage.createFromDataURL(dataUrl));
    tray.setToolTip(`Claude Overlay · 127.0.0.1:${PORT}`);
    buildTrayMenu();
  } catch (err) {
    console.error('tray failed', err);
  }
});

function buildTrayMenu() {
  const t = T().tray;
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: t.testNotify, click: () => handleEvent({ type: 'test-notify' }) },
    { label: t.testLogo, click: () => handleEvent({ type: 'test-logo' }) },
    { type: 'separator' },
    { label: t.reinstall, click: () => { try { installer.install(); reloadConfig(); notify({ title: 'Claude Overlay', subtitle: T().hooksInstalled, kind: 'done' }); } catch (e) { notify({ title: 'Claude Overlay', subtitle: T().error, body: e.message, kind: 'permission' }); } } },
    { label: t.notifAccess, click: () => shell.openExternal('ms-settings:privacy-notifications') },
    { label: t.openConfig, click: () => shell.openPath(P.CONFIG_PATH) },
    { label: t.openLog, click: () => shell.openPath(P.LOG_PATH) },
    { label: t.reload, click: () => reloadConfig() },
    { type: 'separator' },
    { label: t.uninstall, click: () => { installer.uninstall(); notify({ title: 'Claude Overlay', subtitle: T().hooksRemoved, kind: 'info' }); } },
    { label: t.quit, click: () => app.quit() },
  ]));
}

// ---------------------------------------------------------------- banners
function projectName(cwd) {
  return cwd ? path.basename(String(cwd).replace(/[\\/]+$/, '')) : '';
}
function cleanMessage(text) {
  if (!text) return '';
  return String(text).replace(/```[\s\S]*?```/g, ' ').replace(/`([^`]*)`/g, '$1').replace(/[*_#>]+/g, '').replace(/\s+/g, ' ').trim();
}
function withProject(label, cwd) {
  const p = projectName(cwd);
  return p ? `${label} · ${p}` : label;
}
function notify(p) {
  sendToRenderer('notify', {
    title: p.title || T().appName, subtitle: p.subtitle || '', body: p.body || '', kind: p.kind || 'done',
    nowLabel: T().now,
    durationMs: p.durationMs || (config.notification && config.notification.durationMs) || 6000,
    sound: config.sound !== false,
  });
}
function showLogo(caption) {
  sendToRenderer('logo', { caption, durationMs: (config.logo && config.logo.durationMs) || 9000, sound: config.sound !== false });
}

// Human-readable one-liner for a tool call (permission requests).
function describeTool(name, input) {
  const i = input && typeof input === 'object' ? input : {};
  const short = (s, n = 160) => String(s || '').replace(/\s+/g, ' ').trim().slice(0, n);
  switch (name) {
    case 'Bash': case 'PowerShell': return `${name}: ${short(i.command || i.description)}`;
    case 'Edit': case 'Write': case 'Read': case 'NotebookEdit': case 'MultiEdit': return `${name}: ${short(i.file_path || i.notebook_path)}`;
    case 'WebFetch': return `WebFetch: ${short(i.url)}`;
    case 'WebSearch': return `WebSearch: ${short(i.query)}`;
    case 'Agent': case 'Task': return `${name}: ${short(i.description || i.prompt)}`;
    case 'Glob': case 'Grep': return `${name}: ${short(i.pattern)}`;
    default: {
      const keys = Object.keys(i);
      const firstStr = keys.map((k) => i[k]).find((v) => typeof v === 'string');
      return `${name || 'tool'}${firstStr ? ': ' + short(firstStr) : keys.length ? ': ' + short(JSON.stringify(i)) : ''}`;
    }
  }
}

// ---------------------------------------------------------------- interactive (question / permission)
const pending = new Map(); // id -> { res, timer, type, event }
let seq = 0;

function respondJson(res, obj) {
  try {
    res.writeHead(200, { 'Content-Type': 'application/json; charset=utf-8' });
    res.end(JSON.stringify(obj));
  } catch {}
}

function startInteractive(type, event, res) {
  const icfg = config.interactive || {};
  const events = config.events || {};
  const enabledKey = type === 'question' ? 'question' : 'permission_request';
  if (events[enabledKey] === false) return respondJson(res, {});

  const skip = (icfg.skipWhenFocused || ['claude']).map((s) => String(s).toLowerCase());
  if (icfg.enabled === false || skip.includes(String(foregroundProcess).toLowerCase())) {
    // The Claude app is in front: its own dialog is visible, answer there. No banner either.
    logLine([type + '-skip', foregroundProcess || 'disabled']);
    lastHookBannerAt = Date.now();
    if (type === 'permission') lastPermissionAt = Date.now();
    return respondJson(res, {});
  }

  const waitMs = Math.max(3000, Number(icfg.waitMs) || 20000);
  const id = String(++seq);
  const timer = setTimeout(() => finishInteractive(id, null), waitMs);
  pending.set(id, { res, timer, type, event });
  lastHookBannerAt = Date.now();
  if (type === 'permission') lastPermissionAt = Date.now();

  const base = {
    id, kind: type, waitMs, nowLabel: T().now, sound: config.sound !== false,
    labels: { allow: T().allow, deny: T().deny, answerInApp: T().answerInApp, answered: T().answered, ok: 'OK' },
  };
  if (type === 'question') {
    const qs = (event.tool_input && Array.isArray(event.tool_input.questions)) ? event.tool_input.questions : [];
    const questions = qs.map((q) => ({
      question: cleanMessage(q.question),
      header: q.header || '',
      multiSelect: !!q.multiSelect,
      options: (Array.isArray(q.options) ? q.options : []).map((o) => ({ label: String(o.label || ''), description: String(o.description || '') })).filter((o) => o.label),
    })).filter((q) => q.question);
    if (!questions.length) return finishInteractive(id, null);
    sendToRenderer('interactive', { ...base, title: T().codeName, subtitle: withProject(T().question, event.cwd), questions });
  } else {
    sendToRenderer('interactive', { ...base, title: T().codeName, subtitle: withProject(T().permission, event.cwd), body: describeTool(event.tool_name, event.tool_input) });
  }
}

function finishInteractive(id, data) {
  const p = pending.get(id);
  if (!p) return;
  pending.delete(id);
  clearTimeout(p.timer);
  let out = {};
  if (data && p.type === 'question' && data.answers && typeof data.answers === 'object') {
    out = { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'allow',
      updatedInput: { ...(p.event.tool_input || {}), answers: data.answers } } };
  } else if (data && p.type === 'permission' && (data.behavior === 'allow' || data.behavior === 'deny')) {
    out = { hookSpecificOutput: { hookEventName: 'PermissionRequest',
      decision: data.behavior === 'allow' ? { behavior: 'allow' } : { behavior: 'deny', message: 'Denied from Claude Overlay' } } };
  }
  logLine([p.type + '-answer', data ? JSON.stringify(data).slice(0, 200) : 'timeout']);
  respondJson(p.res, out);
  if (win) win.webContents.send('resolve', { id, answered: Object.keys(out).length > 0 });
}

// ---------------------------------------------------------------- limits
const timers = {};
function windowLabel(name) {
  return { five_hour: T().limitFiveHour, seven_day: T().limitSevenDay, spend_limit: T().limitSpend }[name] || T().limitGeneric;
}
function scheduleAllTimers() {
  for (const name of Object.keys(state.windows)) scheduleTimer(name);
}
function scheduleTimer(name) {
  clearTimeout(timers[name]);
  const w = state.windows[name];
  if (!w || !w.resetsAt || w.firedFor === w.resetsAt) return;
  const delay = w.resetsAt * 1000 - Date.now();
  if (delay <= 0) return;
  timers[name] = setTimeout(() => {
    if (w.resetsAt * 1000 - Date.now() > 1000) return scheduleTimer(name);
    fireReset(name, w.resetsAt);
  }, Math.min(delay, 2000000000));
}
function fireReset(name, resetsAt) {
  const w = state.windows[name];
  if (!w || w.firedFor === resetsAt) return;
  w.firedFor = resetsAt;
  saveState();
  const limits = config.limits || {};
  if (limits[name] === false) return;
  const minUsed = Number(limits.minUsedPercentage || 0);
  if (Number(w.usedPercentage || 0) < minUsed) return;
  logLine(['reset', name, String(resetsAt), `used=${w.usedPercentage}`]);
  showLogo(windowLabel(name));
}
function handleStatus(status) {
  const rl = status && status.rate_limits;
  if (!rl || typeof rl !== 'object') return;
  for (const [name, info] of Object.entries(rl)) {
    if (!info || !info.resets_at) continue;
    const incoming = Number(info.resets_at);
    const w = state.windows[name] || (state.windows[name] = {});
    if (w.resetsAt && incoming > w.resetsAt && w.firedFor !== w.resetsAt && w.resetsAt * 1000 <= Date.now()) {
      fireReset(name, w.resetsAt); // window expired while we were not watching
    }
    if (incoming !== w.resetsAt) w.usedPercentage = 0;
    w.resetsAt = incoming;
    const used = Number(info.used_percentage);
    if (!Number.isNaN(used)) w.usedPercentage = Math.max(used, w.usedPercentage || 0);
    scheduleTimer(name);
  }
  saveState();
}

// ---------------------------------------------------------------- toast listener
function startToastListener() {
  const tcfg = config.toasts || {};
  if (tcfg.enabled === false) { listenerStatus = 'disabled'; return; }
  const script = path.join(P.HOOKS_DIR, 'claude-overlay-toast-listener.ps1');
  if (!fs.existsSync(script)) { listenerStatus = 'missing'; return; }
  listenerProc = spawn('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', script], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let buf = '';
  listenerProc.stdout.setEncoding('utf8');
  listenerProc.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (line) handleListenerLine(line);
    }
  });
  listenerProc.stderr.on('data', (d) => logLine(['listener-stderr', String(d).trim().slice(0, 300)]));
  listenerProc.on('exit', (code) => {
    logLine(['listener-exit', String(code)]);
    listenerProc = null;
    if (listenerStatus === 'allowed') setTimeout(startToastListener, 5000); // crashed: restart
  });
}
function handleListenerLine(line) {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.event === 'foreground') { foregroundProcess = String(msg.process || ''); return; }
  if (msg.event === 'access') {
    listenerStatus = msg.status === 'Allowed' ? 'allowed' : 'denied';
    logLine(['listener-access', msg.status]);
    if (listenerStatus !== 'allowed') {
      notify({ title: 'Claude Overlay', subtitle: T().accessDenied, body: T().accessDeniedBody, kind: 'permission', durationMs: 12000 });
    }
    return;
  }
  if (msg.event === 'error') { logLine(['listener-error', String(msg.message).slice(0, 300)]); return; }
  if (msg.event !== 'toast') return;
  handleToast(msg);
}
function handleToast(t) {
  const watch = (config.toasts && config.toasts.watchApps) || ['Claude'];
  const hay = `${t.app || ''} ${t.aumid || ''}`.toLowerCase();
  if (!watch.some((w) => hay.includes(String(w).toLowerCase()))) { logLine(['toast-skip', t.app]); return; }
  const texts = Array.isArray(t.texts) ? t.texts.filter(Boolean) : [];
  logLine(['toast', t.app, texts.join(' | ').slice(0, 200)]);

  const dedupe = Number((config.toasts && config.toasts.dedupeWindowMs) || 6000);
  if (Date.now() - lastHookBannerAt < dedupe) return; // a hook already showed richer info

  // Hold briefly: when a Claude Code hook for the same moment arrives, it wins (it carries
  // the actual message / command). Chat and Cowork have no hooks, so their toast shows.
  const arrivedAt = Date.now();
  const holdMs = Number((config.toasts && config.toasts.holdMs) || 1500);
  setTimeout(() => {
    if (lastHookBannerAt >= arrivedAt - 500) return;
    showToastBanner(t, texts);
  }, holdMs);
}
function showToastBanner(t, texts) {
  const head = texts[0] || '';
  let body = texts.slice(1).join(' · ');
  const probe = `${head} ${body}`.toLowerCase();
  let kind = 'done';
  let label = T().replyReady;
  if (/izin|permission/.test(probe)) { kind = 'permission'; label = T().permission; }
  else if (i18n.GENERIC_WAITING.some((re) => re.test(body))) { kind = 'done'; label = T().replyReady; body = ''; }
  else if (/\?/.test(body)) { kind = 'question'; label = T().question; }
  else if (/tamamland|complete|finished|done/.test(probe)) { kind = 'done'; label = T().taskDone; }
  notify({ title: t.app || T().appName, subtitle: head ? `${label} · ${head}` : label, body, kind });
}

// ---------------------------------------------------------------- events
function handleEvent(event) {
  const type = event.type;
  const events = config.events || {};
  if (type !== 'status') logLine([type, event.notification_type || event.tool_name || '', projectName(event.cwd)]);
  else logLine(['status', summarizeLimits(event.status)]);

  switch (type) {
    case 'stop': {
      if (events.stop === false || event.stop_hook_active) return;
      lastHookBannerAt = Date.now();
      const body = config.notification && config.notification.showLastMessage !== false ? cleanMessage(event.last_assistant_message).slice(0, 180) : '';
      notify({ title: T().codeName, subtitle: withProject(T().taskDone, event.cwd), body, kind: 'done' });
      return;
    }
    case 'notification': {
      const nt = event.notification_type || '';
      if (nt === 'quota_auto_resume_fired') { if (events.quota_auto_resume_fired !== false) showLogo(T().limitResumed); return; }
      if (nt && events[nt] !== true) return;
      if (nt === 'permission_prompt' && Date.now() - lastPermissionAt < 8000) return; // PermissionRequest already shown
      lastHookBannerAt = Date.now();
      const labels = { permission_prompt: T().permission, idle_prompt: T().waitingForYou, agent_needs_input: T().needsInput };
      notify({ title: event.title || T().codeName, subtitle: withProject(labels[nt] || T().notification, event.cwd),
        body: cleanMessage(event.message).slice(0, 160), kind: nt === 'permission_prompt' ? 'permission' : 'info' });
      return;
    }
    case 'status': handleStatus(event.status || event); return;
    case 'test-notify': notify({ title: T().appName, subtitle: `${T().taskDone} · ${T().test}`, body: T().testBody, kind: 'done' }); return;
    case 'test-logo': showLogo(T().limitFiveHour); return;
    case 'reload-config': reloadConfig(); return;
    case 'quit': setTimeout(() => app.quit(), 50); return;
    default: return;
  }
}
function summarizeLimits(status) {
  const rl = status && status.rate_limits;
  if (!rl) return 'no-rate-limits';
  return Object.entries(rl).map(([k, v]) => `${k}=${v && v.used_percentage}%@${v && v.resets_at}`).join(' ');
}

// ---------------------------------------------------------------- server
function startServer() {
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/health') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ ok: true, pid: process.pid, packaged: P.PACKAGED, lang: L.lang, listener: listenerStatus,
        foreground: foregroundProcess, pending: pending.size, windows: state.windows, now: Math.floor(Date.now() / 1000) }));
      return;
    }
    if (req.method !== 'POST') { res.writeHead(405); res.end(); return; }
    let raw = '';
    req.on('data', (c) => { raw += c; if (raw.length > 2000000) req.destroy(); });
    req.on('end', () => {
      let body = {};
      try { body = raw ? JSON.parse(raw) : {}; } catch { res.writeHead(400); res.end('bad json'); return; }
      const type = req.url.replace(/^\/+/, '') || body.type;
      try {
        if (type === 'question' || type === 'permission') {
          logLine([type, body.tool_name || '', projectName(body.cwd)]);
          startInteractive(type, body, res);
          return;
        }
        if (type === 'test-question') {
          startInteractive('question', { cwd: 'C:/demo/project', tool_name: 'AskUserQuestion', tool_input: { questions: [
            { question: 'Which database should we use?', header: 'Database', options: [{ label: 'Postgres', description: 'Managed, scalable' }, { label: 'SQLite', description: 'Single file, zero setup' }, { label: 'MySQL', description: '' }] },
          ] } }, res);
          return;
        }
        if (type === 'test-permission') {
          startInteractive('permission', { cwd: 'C:/demo/project', tool_name: 'Bash', tool_input: { command: 'npm install && npm run build' } }, res);
          return;
        }
        handleEvent({ ...body, type });
        respondJson(res, { ok: true });
      } catch (err) { console.error(err); res.writeHead(500); res.end(String(err)); }
    });
  });
  server.on('error', (err) => { console.error('server error', err); if (err.code === 'EADDRINUSE') app.quit(); });
  server.listen(PORT, '127.0.0.1');
}

// Resolves file locations for both dev mode (electron .) and the packaged exe.
const { app } = require('electron');
const path = require('path');
const fs = require('fs');

const PACKAGED = app.isPackaged;
const ROOT = path.join(__dirname, '..');
// In the packaged build, hooks/ and config.default.json are copied next to the asar
// via --extra-resource, so they live in process.resourcesPath.
const RESOURCES = PACKAGED ? process.resourcesPath : ROOT;
const HOOKS_DIR = path.join(RESOURCES, 'hooks');
const DEFAULT_CONFIG = path.join(RESOURCES, 'config.default.json');

// Runtime data always lives in %APPDATA%\claude-notification so the PowerShell hooks
// can find it without knowing where the app was extracted.
app.setPath('userData', path.join(app.getPath('appData'), 'claude-notification'));
const USER_DATA = app.getPath('userData');
const CONFIG_PATH = path.join(USER_DATA, 'config.json');
const STATE_PATH = path.join(USER_DATA, 'state.json');
const LOG_PATH = path.join(USER_DATA, 'events.log');

function ensureUserData() {
  fs.mkdirSync(USER_DATA, { recursive: true });
  if (!fs.existsSync(CONFIG_PATH)) {
    try {
      fs.copyFileSync(DEFAULT_CONFIG, CONFIG_PATH);
    } catch {
      fs.writeFileSync(CONFIG_PATH, '{}\n');
    }
  }
}

function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8').replace(/^﻿/, ''));
  } catch {
    return fallback;
  }
}
function writeJson(file, data) {
  fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n');
}

function loadConfig() {
  const defaults = readJson(DEFAULT_CONFIG, {});
  const user = readJson(CONFIG_PATH, {});
  return deepMerge(defaults, user);
}

function deepMerge(a, b) {
  const out = { ...a };
  for (const [k, v] of Object.entries(b || {})) {
    out[k] = v && typeof v === 'object' && !Array.isArray(v) && a && typeof a[k] === 'object' ? deepMerge(a[k], v) : v;
  }
  return out;
}

// How to launch the overlay (used by hooks for autostart and by the startup shortcut).
function launchSpec() {
  if (PACKAGED) return { exe: process.execPath, args: [] };
  return { exe: process.execPath, args: [ROOT] };
}

module.exports = {
  PACKAGED, ROOT, RESOURCES, HOOKS_DIR, DEFAULT_CONFIG, USER_DATA, CONFIG_PATH, STATE_PATH, LOG_PATH,
  ensureUserData, readJson, writeJson, loadConfig, launchSpec,
};

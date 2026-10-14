// Runs after electron-packager: trims files the overlay never uses, then builds the
// release archives (7z when available, always zip).
//   node scripts/postpackage.js
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const DIST = path.join(__dirname, '..', 'dist');
const APP = path.join(DIST, 'ClaudeOverlay-win32-x64');
const KEEP_LOCALES = new Set(['en-US.pak', 'tr.pak']);
// Software Vulkan fallback: the overlay composites through ANGLE/D3D11, never Vulkan.
const DROP_FILES = ['vk_swiftshader.dll', 'vk_swiftshader_icd.json', 'vulkan-1.dll'];

function mb(p) {
  let total = 0;
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const f = path.join(d, e.name);
      if (e.isDirectory()) walk(f);
      else total += fs.statSync(f).size;
    }
  };
  walk(p);
  return (total / 1048576).toFixed(1);
}

function trim() {
  const before = mb(APP);
  const locales = path.join(APP, 'locales');
  for (const f of fs.readdirSync(locales)) if (!KEEP_LOCALES.has(f)) fs.unlinkSync(path.join(locales, f));
  for (const f of DROP_FILES) { try { fs.unlinkSync(path.join(APP, f)); } catch {} }
  console.log(`trimmed: ${before} MB -> ${mb(APP)} MB`);
}

function run(cmd, args) {
  const r = spawnSync(cmd, args, { cwd: DIST, stdio: 'inherit', windowsHide: true });
  return r.status === 0;
}

function find7z() {
  for (const c of ['7z', '7za', 'C:\\Program Files\\7-Zip\\7z.exe', 'C:\\Program Files (x86)\\7-Zip\\7z.exe']) {
    const r = spawnSync(c, ['--help'], { windowsHide: true });
    if (!r.error) return c;
  }
  return null;
}

function archive() {
  for (const f of ['ClaudeOverlay-win64.zip', 'ClaudeOverlay-win64.7z']) { try { fs.unlinkSync(path.join(DIST, f)); } catch {} }
  const sevenZip = find7z();
  if (sevenZip) {
    run(sevenZip, ['a', '-t7z', '-mx=9', '-m0=lzma2', '-bso0', '-bsp0', 'ClaudeOverlay-win64.7z', 'ClaudeOverlay-win32-x64']);
    run(sevenZip, ['a', '-tzip', '-mx=9', '-bso0', '-bsp0', 'ClaudeOverlay-win64.zip', 'ClaudeOverlay-win32-x64']);
  } else {
    // bsdtar ships with Windows 10+ and the GitHub Windows runners.
    run(process.platform === 'win32' ? 'C:\\Windows\\System32\\tar.exe' : 'tar', ['-a', '-cf', 'ClaudeOverlay-win64.zip', 'ClaudeOverlay-win32-x64']);
  }
  for (const f of ['ClaudeOverlay-win64.7z', 'ClaudeOverlay-win64.zip']) {
    const p = path.join(DIST, f);
    if (fs.existsSync(p)) console.log(`${f}: ${(fs.statSync(p).size / 1048576).toFixed(1)} MB`);
  }
}

trim();
archive();

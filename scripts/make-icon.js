// Renders the starburst logo to build/icon.png and build/icon.ico (PNG-compressed ICO).
// Run with: electron scripts/make-icon.js
const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');

const OUT = path.join(__dirname, '..', 'build');
const SIZES = [16, 24, 32, 48, 64, 128, 256];

const RAYS = [[0, 1.0, 10.5], [32, 0.78, 9.5], [58, 0.92, 10], [90, 0.7, 9], [118, 0.96, 10.5], [150, 0.74, 9],
  [180, 1.0, 10.5], [212, 0.8, 9.5], [238, 0.9, 10], [270, 0.72, 9], [298, 0.95, 10.5], [330, 0.76, 9]];

function svg() {
  const rays = RAYS.map(([deg, len, w]) => {
    const a = (deg * Math.PI) / 180, inner = 9, r2 = inner + (46 - inner) * len;
    return `<line x1="${50 + Math.cos(a) * inner}" y1="${50 + Math.sin(a) * inner}" x2="${50 + Math.cos(a) * r2}" y2="${50 + Math.sin(a) * r2}" stroke="#fff" stroke-width="${w}" stroke-linecap="round"/>`;
  }).join('');
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="22" fill="#d97757"/><g transform="translate(50 50) scale(0.78) translate(-50 -50)">${rays}</g></svg>`;
}

function ico(pngs) {
  // ICONDIR + ICONDIRENTRY[] + PNG blobs
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(pngs.length, 4);
  const entries = [];
  let offset = 6 + 16 * pngs.length;
  for (const { size, buf } of pngs) {
    const e = Buffer.alloc(16);
    e.writeUInt8(size >= 256 ? 0 : size, 0); e.writeUInt8(size >= 256 ? 0 : size, 1);
    e.writeUInt8(0, 2); e.writeUInt8(0, 3); e.writeUInt16LE(1, 4); e.writeUInt16LE(32, 6);
    e.writeUInt32LE(buf.length, 8); e.writeUInt32LE(offset, 12);
    offset += buf.length;
    entries.push(e);
  }
  return Buffer.concat([header, ...entries, ...pngs.map((p) => p.buf)]);
}

app.whenReady().then(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  const w = new BrowserWindow({ show: false, webPreferences: { offscreen: true } });
  const html = `<html><body style="margin:0;background:transparent"><canvas id="c"></canvas><script>
    const svg = ${JSON.stringify(svg())};
    window.render = (size) => new Promise((res) => {
      const c = document.getElementById('c'); c.width = size; c.height = size;
      const g = c.getContext('2d'); const img = new Image();
      img.onload = () => { g.clearRect(0,0,size,size); g.drawImage(img, 0, 0, size, size); res(c.toDataURL('image/png')); };
      img.src = 'data:image/svg+xml;base64,' + btoa(unescape(encodeURIComponent(svg)));
    });
  </script></body></html>`;
  await w.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));
  const pngs = [];
  for (const size of SIZES) {
    const dataUrl = await w.webContents.executeJavaScript(`window.render(${size})`);
    const buf = Buffer.from(dataUrl.split(',')[1], 'base64');
    pngs.push({ size, buf });
    if (size === 256) fs.writeFileSync(path.join(OUT, 'icon.png'), buf);
  }
  fs.writeFileSync(path.join(OUT, 'icon.ico'), ico(pngs));
  console.log('icon written:', OUT);
  app.quit();
});

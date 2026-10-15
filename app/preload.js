const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('overlay', {
  on(channel, handler) {
    const allowed = ['notify', 'logo', 'config', 'interactive', 'resolve'];
    if (!allowed.includes(channel)) return;
    ipcRenderer.on(channel, (_event, payload) => handler(payload));
  },
  itemDone() {
    ipcRenderer.send('item-done');
  },
  log(message) {
    ipcRenderer.send('rlog', String(message).slice(0, 500));
  },
  sendTrayIcon(dataUrl) {
    ipcRenderer.send('tray-icon', dataUrl);
  },
  // Interactive banners: report their on-screen rectangles (CSS px) so the main process
  // can make just those areas clickable.
  hotRects(rects) {
    ipcRenderer.send('hot-rects', rects);
  },
  // Answer a pending question / permission request. data: { answers } or { behavior }.
  answer(id, data) {
    ipcRenderer.send('answer', { id, ...data });
  },
});

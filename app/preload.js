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
  sendTrayIcon(dataUrl) {
    ipcRenderer.send('tray-icon', dataUrl);
  },
  // Interactive banners: the pointer is over a clickable banner (true) or not (false).
  setInteractive(flag) {
    ipcRenderer.send('mouse', !!flag);
  },
  // Answer a pending question / permission request. data: { answers } or { behavior }.
  answer(id, data) {
    ipcRenderer.send('answer', { id, ...data });
  },
});

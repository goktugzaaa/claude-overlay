const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('overlay', {
  on(channel, handler) {
    const allowed = ['notify', 'logo', 'config'];
    if (!allowed.includes(channel)) return;
    ipcRenderer.on(channel, (_event, payload) => handler(payload));
  },
  itemDone() {
    ipcRenderer.send('item-done');
  },
  sendTrayIcon(dataUrl) {
    ipcRenderer.send('tray-icon', dataUrl);
  },
});

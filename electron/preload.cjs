const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('sessionApi', {
  list: (filter) => ipcRenderer.invoke('sessions:list', filter),
  get: (sessionId) => ipcRenderer.invoke('sessions:get', sessionId),
  remove: (sessionId) => ipcRenderer.invoke('sessions:delete', sessionId),
  restore: (sessionId, target) => ipcRenderer.invoke('sessions:restore', sessionId, target),
  daemonStatus: () => ipcRenderer.invoke('daemon:status'),
  daemonToggle: () => ipcRenderer.invoke('daemon:toggle'),
});

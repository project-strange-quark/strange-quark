const { contextBridge, ipcRenderer } = require("electron");
contextBridge.exposeInMainWorld("markpad", {
  openFile: () => ipcRenderer.invoke("open-file"),
  saveFile: (doc) => ipcRenderer.invoke("save-file", doc),
  recentFiles: () => ipcRenderer.invoke("recent-files"),
  openLink: (url) => ipcRenderer.invoke("open-link", url),
  onMenu: (cb) => ipcRenderer.on("menu", (_e, action) => cb(action)),
});

const { app, BrowserWindow, ipcMain, dialog, Menu, shell } = require("electron");
const fs = require("node:fs/promises");
const path = require("node:path");
const Store = require("electron-store");
const store = new Store({ defaults: { recent: [] } });

function createWindow() {
  const win = new BrowserWindow({ width: 1100, height: 720, title: "MarkPad", webPreferences: { preload: path.join(__dirname, "preload.cjs") } });
  win.loadFile(path.join(__dirname, "../dist/index.html"));
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: "File", submenu: [
      { label: "Open...", accelerator: "CmdOrCtrl+O", click: () => win.webContents.send("menu", "open") },
      { label: "Save", accelerator: "CmdOrCtrl+S", click: () => win.webContents.send("menu", "save") },
    ] },
  ]));
}

ipcMain.handle("open-file", async () => {
  const { canceled, filePaths } = await dialog.showOpenDialog({ filters: [{ name: "Markdown", extensions: ["md"] }] });
  if (canceled) return null;
  const text = await fs.readFile(filePaths[0], "utf8");
  store.set("recent", [filePaths[0], ...store.get("recent").filter((p) => p !== filePaths[0])].slice(0, 5));
  return { path: filePaths[0], text };
});
ipcMain.handle("save-file", async (_e, { path: p, text }) => {
  let target = p;
  if (!target) {
    const r = await dialog.showSaveDialog({ defaultPath: "untitled.md" });
    if (r.canceled) return null;
    target = r.filePath;
  }
  await fs.writeFile(target, text);
  return target;
});
ipcMain.handle("recent-files", () => store.get("recent"));
ipcMain.handle("open-link", (_e, url) => shell.openExternal(url));

app.whenReady().then(createWindow);
app.on("window-all-closed", () => app.quit());

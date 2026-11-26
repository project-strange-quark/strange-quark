# Electron (React renderer) to Strange Quark

The renderer is usually already a client-rendered React app, built with
Vite, electron-vite, webpack or Create React App. Most of the migration
removes the shell, and replaces what the shell gave the renderer.

## Steps

1. **Find the seam.** Read `main` (or the `main` field in `package.json`),
   the preload scripts, and every `contextBridge.exposeInMainWorld` call.
   List each API the renderer gets (`window.api.*`, `window.electron.*`),
   its IPC channel, and what the main process does for it. Also grep the
   renderer for `require(`, `ipcRenderer`, `process.` and `__dirname`. With
   `nodeIntegration: true`, the renderer may call Node directly.
2. **Build only the renderer.**
   - Give it a standalone Vite config (with electron-vite, copy the
     `renderer` section).
   - Make sure `index.html` loads the app without a `file://` assumption:
     set `base` to `/`, not `./` tied to Electron's file protocol.
   - Remove `electron`, `electron-builder` and `electron-vite` from the
     renderer build only. Leave the main-process code in the repo, untouched.
3. **Replace each exposed API, by this table:**

| What the main process did | Strange Quark replacement | Category |
| --- | --- | --- |
| `dialog.showOpenDialog` and reading the chosen file | `<input type="file">` and File or FileReader. Strange Quark shows a native dialog | mechanical or behavioural (no file path, only contents) |
| `dialog.showSaveDialog` and `fs.writeFile` | A Blob, `URL.createObjectURL` and `<a download>`. Strange Quark shows a native save dialog | behavioural (the user picks the location every time) |
| `clipboard.*` | `navigator.clipboard` | mechanical |
| `shell.openExternal(url)` | `window.open(url)`, or `<a target="_blank">`, which opens the system browser | mechanical |
| electron-store, `app.getPath('userData')` with JSON files | `localStorage` or IndexedDB | behavioural (different location, and no migration of existing user data) |
| `net`, or `fetch` from main to avoid CORS | Renderer `fetch` with the host in `network.allow`. Strange Quark has no CORS | mechanical |
| Reading or writing arbitrary paths, watching folders, spawning processes, native addons, SQLite through `better-sqlite3` | No web equivalent. **Stub** | stub |
| `Menu`, `Tray`, `globalShortcut`, `autoUpdater`, `powerMonitor`, `Notification`, deep links, protocol handlers | No equivalent in Strange Quark's first release. **Stub**. In-app keyboard shortcuts on `window` keydown still work | stub |
| `BrowserWindow` size, title, minimum size | `squark.config.ts` `window` | mechanical |
| Several windows | Not supported. One window; see `compatibility.md` | stub, or blocked if central |

4. **Write the stubs** in `src/strange-quark-stubs/electron-api.ts`. Implement
   the same TypeScript interface the preload exposed, and install them
   where the renderer expected `window.api`. Each stub method starts with a
   `// STRANGE-QUARK STUB:` comment naming the IPC channel and what the main
   process did. It either returns clearly fake data or throws
   `NotAvailableInStrangeQuark`, whichever lets the UI render honestly.
5. **Measure the original too,** when you can: `electron .`, or the
   packaged app. Record:
   - time to first window paint (Electron's `ready-to-show`, or the time to
     the first frame on screen);
   - memory: the RSS of the main, GPU and renderer processes together;
   - package size: the installer or the unpacked app directory, including
     Electron's runtime.

   That is the comparison Electron users want. If the app needs a display
   or credentials you do not have, say so instead of guessing.

## Signs of a blocked verdict

- The app's main job is file-system or process work done in main (an IDE's
  language servers, a git client running `git`, a local database UI over
  native SQLite).
- The UI uses `<webview>`, `BrowserView` or iframes as its core.
- The renderer is a WebGL or WebGPU scene.

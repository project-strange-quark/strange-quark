# Tauri (React frontend) to Strange Quark

A Tauri app has a web frontend, usually Vite and React, plus a Rust core in
`src-tauri/`. The frontend reaches the core through `invoke()` and
plugins. Strange Quark replaces the WebView, so it keeps the frontend and loses
the Rust core unless the core can become an HTTP server. The user's
standing choice for this skill is to stub the core.

## Steps

1. **Find the seam.** List every `invoke('<command>', args)` call, every
   `@tauri-apps/api` and `@tauri-apps/plugin-*` import, and every
   `listen()` or `emit()` event. Map each one to its
   `#[tauri::command]` in `src-tauri/src` and say what it does.
2. **Build the frontend alone.** Usually it already is plain Vite. Use
   `vite build` and check `base`. Remove `@tauri-apps/*` from the frontend
   only as each use is replaced. Leave `src-tauri/` in the repo, untouched.
3. **Replace each use, by this table:**

| Tauri | Strange Quark replacement | Category |
| --- | --- | --- |
| `plugin-dialog` `open()` and `plugin-fs` reads of the result | `<input type="file">` and File or FileReader | behavioural (contents, no path) |
| `plugin-dialog` `save()` and `plugin-fs` writes | Blob and `<a download>` | behavioural |
| `plugin-clipboard-manager` | `navigator.clipboard` | mechanical |
| `plugin-shell` `open(url)`, `plugin-opener` | `window.open(url)` | mechanical |
| `plugin-store`, `appDataDir` with JSON | `localStorage` or IndexedDB | behavioural |
| `plugin-http` `fetch` | Global `fetch` with `network.allow`. Strange Quark has no CORS | mechanical |
| `plugin-sql`, `plugin-fs` on arbitrary paths, `plugin-process`, `plugin-shell` `Command`, sidecars | **Stub** | stub |
| Custom `#[tauri::command]`s | **Stub** each one, unless it is pure computation you can port to TypeScript faithfully (then *behavioural*, with a test proving equal output) | stub or behavioural |
| `plugin-notification`, `plugin-global-shortcut`, tray, menus, `plugin-updater`, `plugin-deep-link`, window-state | **Stub** | stub |
| `listen()` events pushed from Rust | **Stub** the source, and keep the UI handler | stub |
| `tauri.conf.json` window size and title | `squark.config.ts` `window` | mechanical |

4. **Write the stubs** in `src/strange-quark-stubs/tauri.ts`. Export `invoke`
   and the plugin functions with the same signatures, and route the
   imports there, by editing the imports or with a Vite alias recorded in
   `MIGRATION.md`. Each stub starts with `// STRANGE-QUARK STUB:` naming the
   command or plugin and what the Rust side did.
5. **Measure the original too,** when you can: `cargo tauri build`, or
   `npm run tauri build`. Tauri renders through the system WebView
   (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows), so the
   comparison is:
   - Strange Quark against Chromium (the skill's standard check);
   - Strange Quark against Tauri: startup, memory, package size, and any
     visible rendering differences.

   If the Rust toolchain or the WebView's build dependencies (on Linux, the
   WebKitGTK dev packages) are missing, say so; do not guess.

## Signs of a blocked verdict

- The core does the app's real work: a file indexer, a local LLM or ML
  runtime, a database, a hardware bridge.
- The frontend is mostly a thin view over constant `invoke` traffic.

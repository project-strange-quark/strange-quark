# Tauri (React frontend) to Strange Quark

A Tauri app has a web frontend, usually Vite and React, plus a Rust core in
`src-tauri/`. The frontend reaches the core through `invoke()`, `listen()`
and plugins. Strange Quark replaces the WebView and keeps both halves, with
its optional Tauri compatibility module:

- the **frontend runs unchanged** through `@strange-quark/tauri`, whose
  prelude provides `window.__TAURI_INTERNALS__` (the surface
  `@tauri-apps/api` and the official plugins call);
- the **Rust core builds as a native module** (architecture §13) against
  the `squark-tauri` crate, which provides the Tauri API its commands use.

## Steps

1. **Find the seam.** List every `invoke('<command>', args)` call, every
   `@tauri-apps/api` and `@tauri-apps/plugin-*` import, and every
   `listen()` or `emit()` event. Map each to its `#[tauri::command]` in
   `src-tauri/src`, and list what the core uses from Tauri besides commands
   (`AppHandle` methods, plugins called from Rust, custom URI schemes,
   menus, window events, `Builder` setup).
2. **Make the core build for both shells.** In `src-tauri/Cargo.toml` make
   `tauri`, `tauri-build` and the plugins optional under a default feature
   (say `tauri-app`), add `squark-tauri` and `squark-native` (path
   dependencies on a Strange Quark checkout) under a `squark` feature, mark
   the binary `required-features = ["tauri-app"]`, and gate
   `tauri_build::build()` in `build.rs`. In `lib.rs`:

   ```rust
   #[cfg(feature = "squark")]
   extern crate squark_tauri as tauri;
   #[cfg(feature = "squark")]
   extern crate squark_tauri as tauri_plugin_dialog; // if the core opens dialogs
   ```

   The command modules then compile unchanged. Move `tauri::Builder`
   setup, plugins, menus and window-event hooks into a `tauri-app`-only
   module, and add a `squark`-only `modules()`:

   ```rust
   pub fn modules() -> Vec<squark_native::Module> {
       vec![squark_native::Module::new("app")
           .manage(AppState::default())
           .commands(squark_tauri::generate_handler![/* the invoke_handler list */])
           .on_close_requested(|app| /* the CloseRequested logic */)
           .on_exit(|_| /* the RunEvent::Exit logic */)]
   }
   ```

   Record each change in `MIGRATION.md`. The Tauri build must keep passing
   its tests.
3. **Map what is left, by this table:**

| Tauri | Strange Quark | Category |
| --- | --- | --- |
| `#[tauri::command]`s, `AppHandle`, `State<'_, T>`, `Emitter`, `Manager`, `async_runtime`, `tauri::http` | `squark-tauri`, unchanged code | mechanical |
| `listen()` events from Rust | `app.emit(...)` reaches the frontend's `listen()` | mechanical |
| `tauri_plugin_dialog` from Rust | `squark-tauri`'s `DialogExt` (`"*"` filters mean "any file") | mechanical |
| `plugin-clipboard-manager` text, `plugin-opener` `open_url`, `webview.setZoom`, `plugin-process` `restart`/`exit` | Handled by `@strange-quark/tauri` | mechanical |
| `plugin-http` `fetch` | Global `fetch` with `network.allow`, or the core's own client | mechanical |
| `on_window_event` close interception | `Module::on_close_requested` + `window.destroy()` / `app.window().close()` | mechanical |
| `RunEvent::Exit` cleanup | `Module::on_exit` | mechanical |
| `plugin-updater`, tray, global shortcuts, deep links, window-state | Not available; the shim rejects them, so the app's error path runs | stub |
| Native menus | Not yet (planned) | stub |
| Custom URI schemes, `convertFileSrc` loads in iframes | Not yet | stub |
| `tauri.conf.json` window size and title | `squark.config.ts` `window` | mechanical |

4. **Configure the build.** Add `@strange-quark/tauri` to the frontend's
   devDependencies and a `squark.config.ts`:

   ```ts
   import { defineConfig } from 'strange-quark';
   import { tauri } from '@strange-quark/tauri/config';

   export default defineConfig({
     ...tauri({ module: 'app' }),
     window: { title: 'App', width: 1280, height: 800 },
     native: { crate: 'src-tauri', modules: ['app'], features: ['squark'],
               defaultFeatures: false, squarkSource: '../strange-quark' },
   });
   ```

   `squark build` builds the app runtime from the Strange Quark checkout
   with its pinned toolchain, which may be newer than the app's.
5. **Measure the original too,** when you can: `cargo tauri build`, or
   `npm run tauri build`. Tauri renders through the system WebView
   (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows), so the
   comparison is:
   - Strange Quark against Chromium (the skill's standard check), with the
     frontend on a fixture (`tauri({ fixture: './replies.js' })`: canned
     replies, no native module) so both engines see the same data;
   - Strange Quark against Tauri: startup, memory, package size (count the
     system WebView's libraries for Tauri), and visible rendering
     differences.

   If the Rust toolchain or the WebView's build dependencies (on Linux, the
   WebKitGTK dev packages) are missing, say so; do not guess.

## Signs of a blocked verdict

- The core's work depends on something with no equivalent: a Tauri plugin
  the app cannot do without, or a frontend built on WebGL,
  `contenteditable` or iframes.
- The core cannot build with the Strange Quark toolchain or links something
  the target platform lacks.

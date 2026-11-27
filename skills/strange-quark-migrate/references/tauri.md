# Tauri (React frontend) to Strange Quark

A Tauri app has a web frontend, usually Vite and React, plus a Rust core in
`src-tauri/`. The frontend reaches the core through `invoke()`, `listen()`
and plugins. Strange Quark replaces the WebView and keeps both halves:

- the **frontend runs unchanged** through `@strange-quark/tauri`, which
  provides `window.__TAURI_INTERNALS__` (the surface `@tauri-apps/api` and
  the official plugins call);
- the **Rust core becomes a native module** (architecture §13): the same
  commands, linked into the app's own runtime by `squark build`.

The command model matches Tauri's on purpose (camelCase JSON arguments,
`AppHandle`, `State<'_, T>`, `Emitter::emit`, close-request interception,
an exit hook), so the core ports by swapping imports, not by rewriting.

## Steps

1. **Find the seam.** List every `invoke('<command>', args)` call, every
   `@tauri-apps/api` and `@tauri-apps/plugin-*` import, and every
   `listen()` or `emit()` event. Map each one to its `#[tauri::command]` in
   `src-tauri/src`, and list what the core uses from Tauri besides commands
   (`AppHandle` methods, plugins called from Rust, custom URI schemes, menus,
   window events).
2. **Make the core build for both shells.** In `src-tauri/Cargo.toml` make
   `tauri`, `tauri-build` and the plugins optional under a default feature
   (say `tauri-app`), add `squark-native` under a `squark` feature, and mark
   the binary `required-features = ["tauri-app"]`. Add a `platform` module
   that re-exports `command`, `AppHandle`, `State`, `Emitter` and
   `spawn_blocking` from `tauri` or from `squark_native` (`export as command`,
   `App as AppHandle`, `type State<'a, T> = squark_native::State<T>`), then
   replace `#[tauri::command]` and `tauri::AppHandle`/`State`/`Emitter` with
   the platform names. Move `tauri::Builder` setup into a `tauri-app`-only
   module, and add a `squark`-only `modules()` that registers the same
   commands with `squark_native::handlers!`, the managed state,
   `on_close_requested` and `on_exit`. Every change is *mechanical*; record
   each in `MIGRATION.md`. The Tauri build must keep passing its tests.
3. **Map what is left, by this table:**

| Tauri | Strange Quark | Category |
| --- | --- | --- |
| Custom `#[tauri::command]`s, `listen()` events from Rust | The same commands in the native module | mechanical |
| `plugin-dialog` called from Rust | `App::dialog(DialogRequest::…)`, off the async workers | mechanical |
| `plugin-dialog` / `plugin-fs` from the frontend | A native command, or `<input type="file">` / `<a download>` | mechanical or behavioural |
| `plugin-clipboard-manager` text, `plugin-opener` `open_url`, `webview.setZoom`, `plugin-process` `restart`/`exit` | Handled by `@strange-quark/tauri` | mechanical |
| `plugin-http` `fetch` | Global `fetch` with `network.allow`, or the core's own client | mechanical |
| `on_window_event` close interception | `Module::on_close_requested` + `app.window().close()` | mechanical |
| `RunEvent::Exit` cleanup | `Module::on_exit` | mechanical |
| `plugin-updater`, tray, global shortcuts, deep links, window-state | Not available; the shim rejects them, so the app's error path runs | stub |
| Native menus | Not yet (planned) | stub |
| Custom URI schemes, `convertFileSrc` loads in iframes | Not yet | stub |
| `tauri.conf.json` window size and title | `squark.config.ts` `window` | mechanical |

4. **Configure the build.** Add `@strange-quark/tauri` to the frontend's
   devDependencies and a `squark.config.ts`:

   ```ts
   export default defineConfig({
     window: { title: 'App', width: 1280, height: 800 },
     native: { crate: 'src-tauri', modules: ['app'], features: ['squark'],
               defaultFeatures: false, squarkSource: '../strange-quark' },
     compat: { tauri: { module: 'app' } },
   });
   ```

   `squark build` needs a Strange Quark source checkout to build the app
   runtime from, with its pinned toolchain.
5. **Measure the original too,** when you can: `cargo tauri build`, or
   `npm run tauri build`. Tauri renders through the system WebView
   (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows), so the
   comparison is:
   - Strange Quark against Chromium (the skill's standard check), with the
     frontend on a fixture transport (`compat.tauri.fixture`: canned replies,
     no native module) so both engines see the same data;
   - Strange Quark against Tauri: startup, memory, package size, and any
     visible rendering differences.

   If the Rust toolchain or the WebView's build dependencies (on Linux, the
   WebKitGTK dev packages) are missing, say so; do not guess.

## Signs of a blocked verdict

- The core's work depends on something with no native-module equivalent:
  a Tauri plugin the app cannot do without, or a frontend built on WebGL,
  `contenteditable` or iframes.
- The core cannot build with the Strange Quark toolchain (Rust 1.98) or
  links something the target platform lacks.

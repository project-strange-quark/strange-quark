# @strange-quark/tauri

Runs a Tauri 2 frontend on Strange Quark without changing its source.
`@tauri-apps/api` and the official plugins reach their backend only through
`window.__TAURI_INTERNALS__`; this package provides it over Strange Quark
native modules.

Set `compat: { tauri: { module: "<native module>" } }` in `squark.config.ts`.
`squark build` and `squark dev` then run the shim before the app's scripts.

| Tauri | Strange Quark |
| --- | --- |
| app commands (`invoke`) | the native module's exported commands |
| `listen` / `once` / `emit` | native module events; frontend emits stay in the page |
| `plugin:clipboard-manager` text | `navigator.clipboard` |
| `plugin:opener\|open_url` | the system browser |
| `plugin:webview\|set_webview_zoom` | page zoom |
| `plugin:process\|restart`, `exit` | relaunch, exit |
| everything else under `plugin:` | rejects with "… is not available in Strange Quark" |

`fixtureTransport(handlers)` runs a frontend with canned replies and no
backend, which is how such an app is compared with Chromium.

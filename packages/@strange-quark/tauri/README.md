# @strange-quark/tauri

An optional compatibility module that runs a Tauri 2 frontend on Strange
Quark without changing its source. Strange Quark itself knows nothing about
Tauri; apps that do not use this package never load any of it.

`@tauri-apps/api` and the official plugins reach their backend only through
`window.__TAURI_INTERNALS__`; this package's prelude provides it over
Strange Quark native modules. Its Rust counterpart, the `squark-tauri`
crate, lets the Tauri backend's commands compile as a native module.

```ts
// squark.config.ts
import { defineConfig } from "strange-quark";
import { tauri } from "@strange-quark/tauri/config";

export default defineConfig({
  ...tauri({ module: "app" }),
  native: { crate: "src-tauri", modules: ["app"], features: ["squark"], defaultFeatures: false },
});
```

`tauri()` only fills in squark.config's generic `preludes` option; with a
JSON config, write it out:

```json
"preludes": [{ "file": "@strange-quark/tauri/dist/tauri.classic.js",
               "globals": { "__STRANGE_QUARK_TAURI__": { "module": "app" } } }]
```

| Tauri | Strange Quark |
| --- | --- |
| app commands (`invoke`) | the native module's commands |
| `listen` / `once` / `emit` | native module events; frontend emits stay in the page |
| `plugin:clipboard-manager` text | `navigator.clipboard` |
| `plugin:opener\|open_url` | the system browser |
| `plugin:webview\|set_webview_zoom` | page zoom |
| `plugin:process\|restart`, `exit` | relaunch, exit |
| everything else under `plugin:` | rejects with "… is not available in Strange Quark" |

`tauri({ fixture: "./fixture.js" })` runs the frontend on canned replies
with no backend, which is how such an app is compared with Chromium.

# Native modules

A native module is Rust code that your app calls from JavaScript by name. It
is linked into your app's own runtime binary, so it adds no second process,
no IPC hop and no WebView bridge. Use one when the app needs something the
web platform doesn't give it: the file system, a PTY, git, a database, a
native library, or a Rust backend you already have.

This guide covers writing a module, building it into an app, calling it from
the page, menus and window control, and what to know per platform. The design
is in [architecture §13](architecture.md#13-native-escape-hatches). For
migrating a Tauri 2 app, see [Tauri apps](#tauri-apps) below.

## What you need

- A **Strange Quark source checkout**. The prebuilt runtime from npm has no
  room for your code, so `squark build` compiles an app runtime from the
  checkout, with the toolchain pinned in its `rust-toolchain.toml` and its
  `Cargo.lock`. Clone it once; [Building from source](../README.md#building-from-source)
  lists the system requirements (on Windows, the Visual Studio Build Tools'
  C++ workload; on macOS, the Xcode command line tools; on Linux,
  `pkg-config` and the fontconfig headers).
- [rustup](https://rustup.rs). It installs the pinned toolchain on the first
  build.

The first build of an app runtime compiles the whole engine and takes several
minutes. Later builds reuse `target/squark-runtime/` in your project.

## Write the module

A module is a library crate that exports a `modules()` function:

```toml
# native/Cargo.toml
[package]
name = "my-app-native"
version = "0.1.0"
edition = "2024"

[dependencies]
serde_json = "1"
squark-native = { path = "../../strange-quark/crates/squark-native" }
tokio = { version = "1", features = ["fs"] }
```

```rust
// native/src/lib.rs
use std::sync::atomic::{AtomicU32, Ordering};

use squark_native::{App, Module, State, export, handlers};

#[derive(Default)]
struct Counter(AtomicU32);

/// JS: invoke("files", "read_text", { path })
#[export]
async fn read_text(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(path).await.map_err(|error| error.to_string())
}

/// JS: invoke("files", "count")
#[export]
fn count(counter: State<Counter>) -> u32 {
    counter.0.fetch_add(1, Ordering::SeqCst) + 1
}

/// JS: invoke("files", "watch", { sessionId }); the page hears "changed" events.
#[export]
fn watch(app: App, session_id: String) {
    std::thread::spawn(move || {
        let _ = app.emit("changed", serde_json::json!({ "session": session_id }));
    });
}

pub fn modules() -> Vec<Module> {
    vec![
        Module::new("files")
            .manage(Counter::default())
            .commands(handlers![read_text, count, watch]),
    ]
}
```

How commands work:

- **The command's name** is the function's name as written (`read_text`).
  `#[export(rename = "readText")]` changes it.
- **Arguments** arrive as one JSON object. Its keys are the parameter names in
  camelCase (`session_id` is `sessionId`), unless you write
  `#[export(rename_all = "snake_case")]`.
- **Return values** are serialized to JSON. A `Result`'s `Err` value rejects
  the page's promise with that value.
- **Parameters typed `App` or `State<T>`** don't come from JSON. `App` is the
  app handle; `State<T>` is the value you passed to `Module::manage`.
- **Sync commands** run on a blocking thread. **Async commands** run on the
  Tokio runtime the network stack already owns, so timers and async I/O work
  without a runtime of your own. `squark_native::runtime()` returns its
  handle for spawning.
- **Events.** `app.emit(event, payload)` works from any thread. Events one
  thread sends arrive at the page in order.

The app handle also gives you:

| Call | What it does |
| --- | --- |
| `app.state::<T>()`, `app.manage(value)` | Read or add managed state at run time |
| `app.window().set_title(..)`, `.set_zoom(..)`, `.close()`, `.relaunch()`, `.exit(code)` | Control the window and the process |
| `app.window().set_menu(Some(menu))` | Set the window's menu (see [Menus](#menus)) |
| `app.dialog(DialogRequest::PickFiles { .. })` | Open a native file, folder or save dialog; blocks until the user answers and returns absolute paths, or `None` if cancelled |
| `app.open_url(url)` | Open an `http`, `https` or `mailto` URL in the system's handler |

And `Module` has hooks besides commands:

| Hook | When it runs |
| --- | --- |
| `.setup(|app| ..)` | Once, when the app starts |
| `.on_close_requested(|app| CloseRequest::Prevent)` | When the user closes the window. Return `Prevent` to keep it open (for example, to ask about unsaved files), then close it later with `app.window().close()` |
| `.on_menu(|app, id| ..)` | When a menu command is activated |
| `.on_exit(|app| ..)` | When the app ends |

## Build it into the app

Name the crate and the modules the app may call in `squark.config.ts`:

```ts
// squark.config.ts
import { defineConfig } from "strange-quark";

export default defineConfig({
  name: "my-app",
  native: {
    crate: "native",                     // the module crate, relative to this file
    modules: ["files"],                  // calls to any other module fail
    squarkSource: "../strange-quark",    // or set SQUARK_SOURCE_DIR
    // features: ["squark"], defaultFeatures: false,
  },
});
```

`squark build` runs your app's build, writes a small runtime crate under
`target/squark-runtime/` that links `native` in, builds it with the `dist`
profile from the checkout, and packs your app onto it. `squark dev` does the
same with the faster-building `dev` profile before it opens the window, so
live reload works with modules too.

Only the modules listed in `native.modules` run. A module the crate provides
but the config doesn't list stays unreachable from the page.

If you build a runtime yourself, `squark pack <web-dir> --runtime <runtime>
--binary dist/my-app --native-module files` packs onto it and enables the
module.

## Call it from the page

The native API is not a web API, so it isn't a global name. It sits under a
registered symbol and exists only in the native runtime, so the same code can
feature-detect it and fall back in a browser:

```ts
const native = (globalThis as any)[Symbol.for("strange-quark.native")];

if (native) {
  native.modules;                                        // ["files"]
  const text = await native.invoke("files", "read_text", { path: "notes.txt" });
  const stop = native.listen("changed", (payload) => console.log(payload));
  await native.invoke("files", "watch", { sessionId: "s1" });
  stop();
} else {
  // Running in a browser: use the web fallback.
}
```

| Member | What it does |
| --- | --- |
| `modules` | The module names this app may call |
| `invoke(module, command, args?)` | Calls a command; resolves with its JSON result or rejects with its error |
| `listen(event, callback)` | Subscribes to events the modules emit; returns an unsubscribe function |
| `window.setTitle(title)`, `.setZoom(factor)`, `.close()`, `.relaunch()`, `.exit(code?)` | Window and process control from the page |
| `window.setMenu(template)` | Sets the window's menu from an Electron-style template, or removes it with `null` |
| `onMenu(callback)` | Hears activated menu commands by id; returns an unsubscribe function |

## Menus

A module (`app.window().set_menu(...)`) or the page (`window.setMenu(...)`)
sets the window's menu. The page uses Electron's template shape:

```ts
await native.window.setMenu([
  {
    label: "&File",
    submenu: [
      { id: "open", label: "Open…", accelerator: "CmdOrCtrl+O" },
      { type: "separator" },
      { id: "wrap", label: "Word Wrap", type: "checkbox", checked: true },
      { label: "Save", accelerator: "CmdOrCtrl+S", click: () => save() },
      { role: "quit" },
    ],
  },
  { label: "&Edit", submenu: [{ role: "undo" }, { role: "redo" }, { role: "selectAll" }] },
]);
native.onMenu((id) => console.log("menu", id));
```

- **Accelerators** use Electron's and Tauri's syntax. `CmdOrCtrl` is Command
  on macOS and Control elsewhere.
- **A key goes to the page first.** An accelerator runs only if the page did
  not cancel the `keydown`, as Chromium does for shortcuts that aren't
  reserved. A terminal component keeps Ctrl+C, and a shortcut the page
  already handles runs once.
- **Roles:** `undo`, `redo`, `cut`, `copy`, `paste`, `selectAll`, `close`,
  `quit` and `about`. Editing roles register no accelerator, because their
  keys already are the page's own default actions.
- An activated command reaches every module's `on_menu` handler and the
  page's `onMenu` listeners; an item with a `click` callback also runs it.

The menu bar itself is not drawn yet on any platform: menus work through
their accelerators. Linux will get a drawn bar, and macOS and Windows the OS
menu bar, in a later release.

## Tauri apps

A Tauri 2 app can keep both halves through the optional Tauri compatibility
module:

- the **Rust core** builds as a native module against the `squark-tauri`
  crate, which provides the Tauri API its commands use (`AppHandle`,
  `State`, `Emitter`, `Manager`, menus, and the dialog and opener plugins);
- the **frontend** runs unchanged through `@strange-quark/tauri`, which
  provides `window.__TAURI_INTERNALS__` as a prelude.

```ts
// squark.config.ts
import { defineConfig } from "strange-quark";
import { tauri } from "@strange-quark/tauri/config";

export default defineConfig({
  ...tauri({ module: "app" }),
  native: {
    crate: "src-tauri",
    modules: ["app"],
    features: ["squark"],
    defaultFeatures: false,
    squarkSource: "../strange-quark",
  },
});
```

The step-by-step port, with the table of Tauri features and their
equivalents, is in the migration skill's
[Tauri reference](../skills/strange-quark-migrate/references/tauri.md).
`@strange-quark/tauri` is not on npm yet: add it from the checkout, for
example `"@strange-quark/tauri": "file:../strange-quark/packages/@strange-quark/tauri"`,
after `npm run build --workspace packages/@strange-quark/tauri` there.

## Platform notes

- **macOS.** `squark build` writes `dist/<name>.app` with the app runtime in
  `Contents/MacOS` and signs the bundle ad hoc, as it does for the stock
  runtime. Sign it with your own identity for distribution.
- **Windows.** The runtime links with MSVC, so the Visual Studio Build
  Tools' C++ workload must be installed. `squark build` writes
  `dist\<name>.exe`.
- **Linux.** The runtime links against the system's fontconfig, like the
  stock runtime.
- An app runtime is built for the machine it is built on. Build on each
  platform you ship for.

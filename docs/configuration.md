# Configuration and CLI reference

This page lists every `squark.config` option, the `squark` commands and their
flags, and the environment variables the CLI and the runtime read. For a
first build, start with [Using it](../README.md#using-it) in the README.

## `squark.config.ts`

`squark build` and `squark dev` look for `squark.config.ts`, `.mjs`, `.js` or
`.json` in the app's directory, in that order, or take the file given with
`--config`. Every option is optional; an app with no config builds with the
defaults below.

```ts
// squark.config.ts
import { defineConfig } from "strange-quark";

export default defineConfig({
  name: "my-app",
  window: { title: "My App", width: 1280, height: 800 },
  build: { command: "npm run build", distDir: "dist" },
  network: { allow: ["api.example.com"] },
});
```

`defineConfig` checks the object when the file loads and gives you type
completion. A config file may also export a function, sync or async, that
returns the object. Node imports `.ts` files with its built-in type stripping,
so the file may use type annotations but not TypeScript-only syntax such as
`enum`.

### Top level

| Option | Type | Default | What it does |
| --- | --- | --- | --- |
| `name` | string | `package.json` `name` without its scope, else the directory name | The app's name: the binary's file name (`dist/<name>`, `dist/<name>.app`, `dist\<name>.exe`) and the default window title. Characters other than letters, digits, `.`, `_` and `-` become `-` |
| `window` | object | | [The window](#window) |
| `build` | object | | [How the app is built and packed](#build) |
| `network` | object | | [What `fetch` and `WebSocket` may reach](#network) |
| `assets` | array | `[]` | [Extra files to pack](#assets) |
| `native` | object | | [Native modules](#native) |
| `preludes` | array | `[]` | [Scripts that run before the app's](#preludes) |

### `window`

| Option | Type | Default | What it does |
| --- | --- | --- | --- |
| `title` | string | `name` | The window title. The native API's `window.setTitle()` changes it at run time ([Native modules](native-modules.md#call-it-from-the-page)) |
| `width` | number | `1280` | Initial width in CSS pixels |
| `height` | number | `800` | Initial height in CSS pixels |
| `resizable` | boolean | `true` | Accepted, but not applied yet: windows are always resizable |

### `build`

| Option | Type | Default | What it does |
| --- | --- | --- | --- |
| `command` | string | `yarn build` if `yarn.lock` exists, else `npm run build` | The shell command that builds the app for the browser |
| `env` | object | `{}` | Extra environment variables for that command, such as `VITE_*` settings |
| `distDir` | string | `dist/`, else `build/` | The build output to pack, relative to the app |
| `entryHtml` | string | `index.html` | The entry page, relative to `distDir` |
| `includeSourcemaps` | boolean | `false` | Pack `.map` files too |
| `runtime` | string | The runtime from the `@strange-quark/cli-<os>-<cpu>` package | A `squark-runtime` binary to pack onto instead |
| `out` | string | `dist/<name>` | Where to write the app, relative to the app's directory |

The output on each platform:

- **Linux:** one executable, `dist/<name>`, with the pack appended to the
  runtime.
- **macOS:** an app bundle, `dist/<name>.app`, with the runtime in
  `Contents/MacOS`, the pack in `Contents/Resources/app.sqpak`, and an ad-hoc
  signature over the whole bundle.
- **Windows:** one executable, `dist\<name>.exe`.

### `network`

| Option | Type | Default | What it does |
| --- | --- | --- | --- |
| `allow` | string[] | `[]` | The hosts and origins `fetch` and `WebSocket` may reach |

Nothing outside the app's own pack is reachable unless it is listed here. An
entry is either:

- a **host**, such as `"api.example.com"` or `"api.example.com:8443"`, which
  allows any scheme the runtime supports (`http`, `https`, `ws`, `wss`) on
  that exact host, and on that port only if one is given; or
- an **origin**, such as `"wss://events.example.com"`, which allows that
  scheme, host and port only.

Matching is exact: `"example.com"` does not allow `sub.example.com`, and there
are no wildcards. In `squark dev`, the Vite dev server is always allowed.

### `assets`

Files the app requests at fixed paths that its build doesn't emit, such as a
font a library loads by URL:

```ts
assets: [{ path: "/assets/Inter-Bold.woff2", file: "./fonts/Inter-Bold.woff2" }],
```

| Field | What it is |
| --- | --- |
| `path` | The absolute path the app requests; must start with `/` |
| `file` | The file to read, relative to the config file |

### `native`

Links the app's own Rust into its runtime. See
[Native modules](native-modules.md) for the whole guide.

| Option | Type | Default | What it does |
| --- | --- | --- | --- |
| `crate` | string | | The native module crate's directory, relative to the config file. Its library's `modules()` returns `Vec<squark_native::Module>` |
| `modules` | string[] | `[]` | The modules the page may call; calls to any other fail |
| `squarkSource` | string | `$SQUARK_SOURCE_DIR` | A Strange Quark source checkout to build the app runtime from |
| `features` | string[] | `[]` | Cargo features of the crate to enable |
| `defaultFeatures` | boolean | `true` | Whether the crate's default features stay on |

### `preludes`

Classic scripts that run, in order, before the app's own scripts. Optional
packages such as `@strange-quark/tauri` use them to provide globals an app
expects.

| Field | What it is |
| --- | --- |
| `file` | A path relative to the project (`./`, `../`, or absolute), or a path inside an installed package (`@scope/pkg/dist/file.js`) |
| `globals` | Values assigned before the script runs, as `globalThis[name] = value` |

## The `squark` CLI

Run it through your project (`npx squark ...`) so it finds the installed
runtime. Every command prints its full options with `--help`.

### `squark build [dir]`

Runs the app's build, then packs its output into a native app.

| Flag | What it does |
| --- | --- |
| `--config <file>` | Use this config file |
| `--out <path>` | Where to write the app (overrides `build.out`) |
| `--sqpak-out <path>` | Also write the pack as a standalone `.sqpak` |
| `--runtime <path>` | Pack onto this `squark-runtime` (or set `SQUARK_RUNTIME_BIN`) |
| `--dist <dir>` | Pack this directory (overrides `build.distDir`) |
| `--skip-app-build` | Don't run the app's build command; pack what is there |
| `--include-sourcemaps` | Pack `.map` files too |

### `squark dev [dir]`

Starts the Vite dev server with `@strange-quark/vite-plugin` and opens the app
in a native window, with live reload and React Fast Refresh. Console output
goes to the terminal.

| Flag | What it does |
| --- | --- |
| `--port <n>` | Vite's port; `0` picks a free one (default `5173`) |
| `--host <addr>` | Vite's bind address (default `127.0.0.1`) |
| `--config <file>` | Use this config file |
| `--runtime <path>` | Run this `squark-runtime` |
| `--width <n>`, `--height <n>` | Window size in CSS pixels (default: `window.width` and `window.height`) |
| `--work-dir <dir>` | Where to write the dev pack and its files |
| `--prepare-only` | Write the dev pack and stop without opening the window |
| `--test-control <socket>`, `--headless` | Drive the app from a test (see `tests/compat/src/squark-driver.mjs`) |

### `squark pack <web-dir>`

Packs a built directory without running a build.

| Flag | What it does |
| --- | --- |
| `--out <file>` | Write a `.sqpak` archive |
| `--binary <file>` | Write a standalone executable: the pack appended to `--runtime` |
| `--runtime <path>` | The runtime for `--binary` |
| `--html <file>` | The entry page (default `<web-dir>/index.html`) |
| `--name <name>` | The app name in the pack's manifest |
| `--native-module <name>` | Enable a native module the runtime provides (repeatable) |
| `--include-sourcemaps` | Pack `.map` files too |

### `squark screenshot <input> --out <png>`

Renders an HTML file, a directory, a `.sqpak` or a packed app headlessly to a
PNG. `--width`, `--height` (default 800×600) and `--dpr` (default 1) set the
viewport, `--timeout` the time limit in seconds (default 30), and
`--gpu-adapter auto|software|hardware` the adapter.

### `squark inspect --dump-pack <file>`

Lists and validates what is in a `.sqpak`, a packed app or a `document.bin`.
`--json` prints JSON.

## Environment variables

| Variable | Read by | What it does |
| --- | --- | --- |
| `SQUARK_BIN` | the `strange-quark` package | A `squark` binary to run instead of the installed one |
| `SQUARK_RUNTIME_BIN` | `squark` | A `squark-runtime` to pack onto or run, like `--runtime` |
| `SQUARK_SOURCE_DIR` | `squark` | A source checkout for native module builds, like `native.squarkSource` |
| `SQUARK_CONSOLE=1` | the runtime | Print `console` output to stderr in a built app (it is always on in `squark dev`) |

## Built apps

- `--third-party-notices` prints the licences of the third-party software in
  the runtime. Ship its output with your app (see
  [Distributing your app](../README.md#distributing-your-app)).
- `localStorage` persists in a `strange-quark` folder in the platform's
  per-user data directory, one folder per app name: under
  `~/.local/share` (or `$XDG_DATA_HOME`) on Linux,
  `~/Library/Application Support` on macOS and `%APPDATA%` on Windows.

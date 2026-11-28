# Changelog

All notable changes to Strange Quark are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, a
minor version may change behaviour that apps rely on; such changes are listed
under **Changed**.

All eight npm packages (`strange-quark`, `@strange-quark/vite-plugin` and the
six `@strange-quark/cli-<os>-<cpu>` binary packages) and the Rust crates share
one version number.

## [Unreleased]

These changes will be released as 0.2.0. They bring the platform features
that desktop apps built on React need beyond a single document: a Rust
backend linked into the app, window menus, Web Workers, Shadow DOM, CSS
tables, Range and Selection, and better font matching. They were checked on
a full desktop IDE (React 19, Monaco, xterm.js, KaTeX and a Tauri 2 Rust
backend) as well as on Excalidraw.

### Added

- **Native modules.** An app can link its own Rust into its runtime. Mark
  functions with `#[squark_native::export]`, group them in
  `squark_native::Module`s, and name the crate in `squark.config.ts` under
  `native.crate`. `squark build` and `squark dev` then build an app runtime
  from a Strange Quark source checkout (`native.squarkSource` or
  `SQUARK_SOURCE_DIR`, with that checkout's toolchain and lockfile).
  JavaScript calls commands by name and listens to events the module emits.
  A module runs only if `native.modules` lists it. Sync commands run on a
  blocking thread and async ones on the runtime the network stack already
  owns. Modules can keep managed state, run setup and exit hooks, handle
  close requests, open file dialogs and URLs, and set the window's title,
  zoom and menu. `squark pack --native-module <name>` enables a module for a
  prebuilt runtime.
- **Tauri 2 compatibility, as an optional module.** The `squark-tauri` crate
  gives a Tauri backend's commands the API they were written against
  (commands, state, events, menu builders and the opener plugin), and
  `@strange-quark/tauri` provides `window.__TAURI_INTERNALS__` over native
  modules, so a Tauri frontend runs without source changes. Apps that don't
  use it never install or link it.
- **Preludes.** `preludes` in `squark.config.ts` runs classic scripts, from
  the project or from an installed package, before the app's own, with
  optional globals.
- **Window menus.** A module (`app.window().set_menu(...)`) or the page
  (`setMenu(template)` on the native API, in Electron's template shape) sets
  the window's menu: commands, `&` mnemonics, accelerators in Electron's and
  Tauri's syntax (`CmdOrCtrl+Shift+F`), check items, submenus, separators and
  standard roles. An accelerator runs only if the page did not cancel the
  keydown, as Chromium handles non-reserved shortcuts.
- **Web Workers.** Dedicated workers run on their own V8 isolates and
  threads, with `postMessage`, structured clone, `importScripts` and module
  workers.
- **Shadow DOM.** Open and closed shadow roots, slots and slot assignment,
  event retargeting, `:host` and `::slotted`, styled and laid out in the flat
  tree. Constructable stylesheets can be adopted by documents and shadow
  roots.
- **Custom elements.** Autonomous custom elements with their lifecycle
  callbacks and `observedAttributes`.
- **Range and Selection.** Live `Range` and `StaticRange`, the Selection API
  and `selectionchange`, range rects (`getClientRects`,
  `getBoundingClientRect`) and caret hit testing (`caretPositionFromPoint`)
  answered from layout.
- **HTML drag and drop** within the page, driven by pointer input, with
  `DragEvent`, `DataTransfer` and the `ondrag*` handlers.
- **CSS tables.** Tables lay out with the CSS table model (anonymous boxes,
  column widths, `vertical-align` in cells, inline-table baselines) instead
  of a grid approximation.
- **Fonts.** `@font-face` rules match by their weight, style and stretch
  descriptors and by `unicode-range`; `font-variation-settings` and
  `font-optical-sizing` apply variation axes; font-relative units (`ex`,
  `ch`, `cap`, `ic`) resolve against the primary font's metrics; bold and
  italic are synthesized as Chromium does when a family lacks the face.
- **CSS.** `contain` (size and paint containment), `backdrop-filter` and
  `overscroll-behavior` are parsed, and stylesheets linked after startup
  load and fire `load` and `error`.
- **DOM and Web APIs.** Per-tag HTML element interfaces, `Attr` nodes in a
  live `NamedNodeMap`, `document.implementation`, `NodeIterator`, parsing
  into inert documents, `UIEvent` and the `MouseEvent` position members,
  `contextmenu` and `auxclick` for non-primary buttons, Chromium's
  `KeyboardEvent.keyCode`, `document.hasFocus()` and the Page Visibility
  API, `requestIdleCallback`, User Timing marks and measures, and a
  streaming `TextDecoder` that honours `fatal` and rejects unknown labels.
- **Console output in development.** `console.log` and friends print to
  stderr, with browser-style formatting, in `squark dev`, test-control runs
  and when `SQUARK_CONSOLE=1` is set. Shipped apps keep a silent console.
- **CSS warnings** in debug builds now also list declarations Stylo drops
  because the property is behind a preference (such as `scrollbar-width`
  or `writing-mode`) and rules that use `::-webkit-scrollbar`.
- **Tests.** WPT cases for custom elements, Range, Selection, Shadow DOM,
  tables, attributes, traversal and UI event constructors; reftests against
  Chromium for tables, Shadow DOM, `@font-face` matching, `unicode-range`,
  variation axes, containment, `backdrop-filter` and synthetic styles; and
  an end-to-end native module test (`tests/native`).

### Changed

- `SecurityError` and `NetworkError` carry their legacy `DOMException`
  codes.
- Assignments to `element.style` set its `cssText`, as in browsers.
- The `strange-quark` package's `bin` path is written the way npm
  publishes it (`dist/cli.js`), so publishing no longer warns.

### Known limitations

- Menus have no visible menu bar yet. They work through their accelerators
  on every platform; a drawn bar on Linux and the OS menu bars on macOS and
  Windows come later.
- Drag and drop works within the page only: no drops from other apps and no
  drag images.
- No shared workers, and `MessagePort`s can't be sent to workers.
- Native modules need a Strange Quark source checkout to build the app
  runtime.
- iframes are designed but not built.

## [0.1.0] - 2026-11-27

The first public release, for Linux (x64, arm64), macOS (arm64, x64) and
Windows (x64, arm64).

### Added

- The `squark` CLI: `build` packs a React app's build output into one native
  binary (an app bundle on macOS), `dev` runs a Vite dev server with live
  reload in a native window, and `pack`, `inspect` and `screenshot` work on
  packs directly.
- `strange-quark` on npm, with the CLI and a prebuilt runtime for each
  platform, and `@strange-quark/vite-plugin` for `squark dev`.
- Compat mode: unmodified `react-dom` on squark-dom, a DOM written in
  JavaScript, running in V8. DOM changes stream as journal records into a
  Rust document that Stylo styles, Taffy and Parley lay out, and wgpu and
  Vello draw on their own thread.
- Upstream Excalidraw v0.18.1 built into a native app without source
  changes, passing the same 20-step end-to-end flow as Chromium on Linux
  (Vulkan), macOS (Metal) and Windows (Direct3D 12).
- Canvas 2D, fonts and `FontFace`, storage, clipboard, file dialogs, text
  input and IME, CSS animations and transitions, scrolling, and the network
  allowlist (`network.allow`).
- `--third-party-notices` on every runtime and app, and
  `THIRD_PARTY_NOTICES.txt` in every binary package.

[Unreleased]: https://github.com/project-strange-quark/strange-quark/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/project-strange-quark/strange-quark/releases/tag/v0.1.0

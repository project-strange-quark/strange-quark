# Project Strange Quark

[![npm](https://img.shields.io/npm/v/strange-quark?logo=npm&label=npm)](https://www.npmjs.com/package/strange-quark)
[![CI](https://github.com/project-strange-quark/strange-quark/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/project-strange-quark/strange-quark/actions/workflows/ci.yml)
[![Licence: MIT](https://img.shields.io/badge/licence-MIT-blue)](LICENSE)
[![Node](https://img.shields.io/node/v/strange-quark?logo=node.js&label=node)](scripts/npm/README.md#minimum-nodejs-version-for-users-22180)
[![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-informational)](#platforms)
[![React](https://img.shields.io/badge/react-18.3%20%7C%2019-61dafb?logo=react)](#react-apps-yes)

**Run your React app as a native desktop app. No WebView, no bundled browser.**

Strange Quark takes the app you already build for the browser and turns it into
a single native binary. Your components, hooks and npm libraries run
unmodified, on the real `react-dom`.

> **Status: v0.2.0, for Linux, macOS and Windows.** It builds upstream
> [Excalidraw](https://github.com/excalidraw/excalidraw) v0.18.1 into one
> native app without changing a line of its source, and the app passes the
> same end-to-end flow as Chromium on Linux (Vulkan), macOS (Metal) and
> Windows (Direct3D 12). 0.2.0 adds what larger desktop apps need: native
> modules that link the app's Rust into its runtime, optional Tauri 2
> compatibility, window menus, Web Workers, Shadow DOM, CSS tables, Range and
> Selection. See the [changelog](CHANGELOG.md), [Excalidraw](#excalidraw) for
> the measurements, [Platforms](#platforms), [What works](#what-works) and
> [Next](#next).

## How it works

Strange Quark keeps the parts of a browser that React apps depend on and
replaces the rest with native code:

1. **Your app and react-dom run unmodified** in V8.
2. **react-dom talks to squark-dom,** a DOM written in JavaScript. React and
   libraries see ordinary DOM nodes, events and CSSOM objects.
3. **Every DOM change becomes a 16-byte record** in a journal. The journal
   streams into a Rust copy of the document.
4. **[Stylo](https://github.com/servo/stylo)** (the CSS engine Servo and
   Firefox share) styles that document.
   **[Taffy](https://github.com/DioxusLabs/taffy)** and
   **[Parley](https://github.com/linebender/parley)** lay it out.
5. **A GPU renderer draws it** through [wgpu](https://wgpu.rs) and
   [Vello](https://github.com/linebender/vello), on its own thread.
   Scrolling and animations stay smooth while your JavaScript is busy.

There is no HTML engine underneath, so nothing is rendered that your app
didn't ask for. The full design is in
[docs/architecture.md](docs/architecture.md).

## Compatibility

### React apps: yes

Strange Quark runs **client-rendered React apps**: React 18.3 or 19.x
single-page apps whose build produces static HTML, JavaScript and CSS. That
includes apps built with Vite, Rsbuild, webpack or Parcel. `squark build`
packs that output. For Vite 6, 7 and 8 apps, `squark dev` adds live reload
with HMR and React Fast Refresh.

The first release targets these libraries. Each one is checked against
Chromium before it is called supported.

| Kind | Libraries |
| --- | --- |
| State and data | Redux, Zustand, Jotai, TanStack Query, SWR |
| Routing (library mode) | React Router, TanStack Router |
| Styling | Tailwind CSS v4, CSS Modules, emotion, styled-components |
| UI | Radix UI, shadcn/ui, Floating UI |
| Forms | React Hook Form, Formik |
| Canvas | Canvas 2D, as used by Excalidraw |
| Editors and content | Monaco Editor, xterm.js, KaTeX, marked, DOMPurify |

Desktop apps whose UI is React and whose backend is Rust can keep that
backend: its commands become **native modules**, linked into the app's own
runtime and called from JavaScript by name. Tauri 2 apps can opt into a
compatibility module (`squark-tauri` and `@strange-quark/tauri`) so their
commands, events and frontend run unchanged. See
[architecture §13](docs/architecture.md#13-native-escape-hatches) and the
`strange-quark-migrate` skill.

### React meta-frameworks: no

**Strange Quark is compatible with React only, not with React meta-frameworks.**
These are not supported:

- **Next.js**, including its static export;
- **TanStack Start**;
- **Remix**, and **React Router in framework mode**;
- any other framework that renders on a server or runs code on one.

A meta-framework is more than React. It expects a server that renders HTML,
streams React Server Components, answers server actions and server
functions, and runs API routes and middleware. Its client code is built to
talk to that server. Strange Quark runs only the browser half of an app, inside
a native process. There is no server for that client to call.

If your app uses a meta-framework mostly for routing, port it to a
single-page app. Use Vite with TanStack Router, or React Router in library or
data mode. Then build it with Strange Quark.

### Also not supported

- **React Native.** Strange Quark runs `react-dom` apps.
- **WebGL and three.js.** A native GPU view is planned instead.
- **iframes, service workers, `document.write`, and cross-origin
  navigation.** iframes are designed but come after 0.2.0.
- **`contenteditable` rich-text editors** (Lexical, Slate, ProseMirror,
  TipTap). They need a full editing host, which comes later. A plain
  `<textarea>` and `<input>` are supported.
- **Shared workers and MessagePorts sent to workers.** Dedicated workers
  work.
- **A visible menu bar.** Menus set by an app work through their
  accelerators on every platform, but no bar is shown yet: Linux will get a
  drawn one, and macOS and Windows the OS menu bar.
- **Visible scrollbars.** Scrolling works by wheel, touchpad, keyboard and
  script, but no scrollbar is drawn. The Chromium reference renders hide
  them too.

Strange Quark reports features honestly. An API it does not implement is left
undefined, as in a browser that lacks it, so your app's fallback path runs.

## Platforms

| Platform | GPU | `squark build` output | Status |
| --- | --- | --- | --- |
| Linux x64 and arm64, glibc 2.35+ (Wayland and X11) | Vulkan | `dist/<name>` | v0.1.0; Excalidraw passes on x64. arm64 is built for release but not yet tested on hardware |
| macOS 12+ (Apple Silicon and Intel) | Metal | `dist/<name>.app`, signed ad hoc | v0.1.0; Excalidraw passes, tested on Apple Silicon |
| Windows 10+ (x64 and arm64) | Direct3D 12 | `dist\<name>.exe` | v0.1.0; Excalidraw passes, tested on x64 with an NVIDIA GPU. arm64 tested in a VM without a GPU |
| Android, iOS, embedded Linux | | | Planned |

On Linux, glibc 2.35 or newer means Ubuntu 22.04, Debian 12, Fedora 36 or
a later release. On macOS, `squark build` writes an app bundle: the runtime in
`Contents/MacOS`, the pack in `Contents/Resources`, and a signature over the
whole bundle, so it passes `codesign --verify --strict` and can be signed
with your own identity and notarized for distribution. Open it with
`open dist/<name>.app`.

## Using it

Strange Quark is on npm as `strange-quark`. It brings the `squark` CLI and a
prebuilt runtime for your platform through an optional dependency
(`@strange-quark/cli-<os>-<cpu>`). It does not touch your app's source or
its build config:

```sh
npm install -D strange-quark
npx squark build      # runs your app's build (e.g. vite build), then packs
                      # its output into one native binary: dist/<name>
./dist/my-app
```

For live reload in a Vite app, add the plugin and run `squark dev`:

```sh
npm install -D @strange-quark/vite-plugin
npx squark dev
```

Run `squark` through your project, as above. A bare `npx squark` outside a
project fetches an unrelated npm package called `squark`; to try Strange
Quark without installing it, run `npx strange-quark build`.

Optional settings go in `squark.config.ts`
([every option](docs/configuration.md)):

```ts
// squark.config.ts
import { defineConfig } from "strange-quark";

export default defineConfig({
  name: "my-app",
  window: { title: "My App", width: 1280, height: 800 },
  build: {
    command: "npm run build",  // how to build your app (detected if omitted)
    distDir: "dist",           // where the build output lands (dist/ or build/)
    env: { VITE_API_URL: "https://api.example.com" },
  },
  network: {
    allow: ["api.example.com"],  // hosts fetch and WebSocket may reach
  },
});
```

### Calling Rust from your app

If your app needs the file system, a PTY, a database or a Rust backend you
already have, write it as a **native module**: Rust functions marked
`#[squark_native::export]`, linked into your app's own runtime and called from
JavaScript by name. Name the crate under `native` in `squark.config.ts`, and
`squark build` builds the runtime with it from a Strange Quark source
checkout. [docs/native-modules.md](docs/native-modules.md) is the guide, with
the menu API and the Tauri 2 path.

### Distributing your app

A built app contains the Strange Quark runtime, which includes third-party
software (V8, Stylo and others) whose licences require their notices to
travel with it. The app prints them itself; ship the file alongside the app:

```sh
./dist/my-app --third-party-notices > THIRD_PARTY_NOTICES.txt                         # Linux
dist/my-app.app/Contents/MacOS/my-app --third-party-notices > THIRD_PARTY_NOTICES.txt  # macOS
```

On Windows, run `.\dist\my-app.exe --third-party-notices > THIRD_PARTY_NOTICES.txt`
in PowerShell. The same file is in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt)
and in each `@strange-quark/cli-<os>-<cpu>` npm package.

## Excalidraw

Upstream **Excalidraw v0.18.1** (`a2ec2889`, zero source patches to
`vendor/excalidraw`, built with `VITE_APP_DISABLE_SENTRY=true`,
`VITE_APP_ENABLE_PWA=false`, `VITE_APP_ENABLE_TRACKING=false`, collaboration
disabled) is the first release's showcase. Every number below comes from
[`tests/results/excalidraw/summary.json`](tests/results/excalidraw/summary.json) on Linux x86_64
(AMD Radeon RX 9060 XT / RADV and Mesa lavapipe):

| Check | Result | Evidence |
| --- | --- | --- |
| Packaging & no patches | **Passing** — single stripped x86-64 ELF (`dist/excalidraw`), **95.53 MiB** (`100,171,122` bytes; `35.6 MB` `xz -9`, `37.8 MB` `zstd -19`, limit 150 MiB), 12 system libraries only, `vendor/excalidraw` clean, isolated headless launch verified | [`tests/results/excalidraw/packaging.json`](tests/results/excalidraw/packaging.json) |
| Startup (windowed, empty scene) | **Passing** — **87.5 ms** median over 5 runs (`86.8–169 ms`) from process start to first presented frame on the stripped `dist` runtime, RX 9060 XT (`CPUWeight=10000`, load `7.1`, limit `< 1,500 ms`) | [`tests/results/excalidraw/startup.json`](tests/results/excalidraw/startup.json) |
| End-to-end flow (20 steps) | **Passing (gating)** — Chromium **20/20**; native **20/20** headless on lavapipe (**20/20** DOM checkpoints matched) and **20/20** windowed on RADV (**20/20** DOM checkpoints matched) across drawing (rectangle, ellipse, arrow, freedraw, text), WYSIWYG edit + IME composition, undo/redo, zoom/pan, dark mode, library sidebar, `.excalidraw` save/open, PNG/SVG export, clipboard image paste, and restart persistence | [`tests/results/excalidraw/e2e.json`](tests/results/excalidraw/e2e.json), [`tests/results/excalidraw/e2e-windowed.json`](tests/results/excalidraw/e2e-windowed.json) |
| Canonical scenes (10 fixtures) | **Passing** — Chromium baselines **10/10**; native **10/10** on lavapipe (**0.174%–0.456%** diff ratio, max `0.456%` on `10-mixed`) and **10/10** on RX 9060 XT / RADV (**0.176%–0.458%** diff ratio, max `0.458%` on `10-mixed`), all within the app screenshot tolerance (`maxChannelDelta: 8`, `maxDiffRatio: 0.005`) | [`tests/results/excalidraw/scenes/summary.json`](tests/results/excalidraw/scenes/summary.json), [`tests/results/excalidraw/scenes/summary-auto.json`](tests/results/excalidraw/scenes/summary-auto.json) |
| 1,000-element pan (60 Hz) | **Passing (gating)** — **300/300** on-time fresh commit slots in each of 3 valid nested-Hyprland runs (target ≥ 95%); every run `wheelEvents = pageWheelEvents = canvasWheelEvents = 300` and `scrollY: 310 → -754` as scripted, `1280×800`, RX 9060 XT / RADV, `CPUWeight=10000`, load `5.9–7.5`. Earlier runs at load 57–61 scored 100%, 100% and 99.7% | [`tests/results/excalidraw/pan.json`](tests/results/excalidraw/pan.json) |
| Suite & lavapipe checks | **Passing** — `scripts/check.sh` and `scripts/check-lavapipe.sh` passed on the commit these results were recorded at, in the history before this repository was published | [`tests/results/excalidraw/summary.json`](tests/results/excalidraw/summary.json) |

The release verdict in [`tests/results/excalidraw/summary.json`](tests/results/excalidraw/summary.json)
is **`passing`** (`blockers: []`), with every native check required to pass.

### On macOS

The same Excalidraw build runs on macOS. These numbers come from an Apple M4
Pro (Metal 4) on macOS 26, with the stripped `dist` runtime. They are
recorded separately from the Linux verdict above:

| Check | Result | Evidence |
| --- | --- | --- |
| Packaging & no patches | **Passing**: app bundle `dist/excalidraw.app` with a Mach-O arm64 executable, **85.1 MiB** runtime plus pack (`89,237,826` bytes, limit 150 MiB). Passes `codesign --verify --strict` and links macOS system frameworks only. `vendor/excalidraw` clean; isolated launch verified | [`tests/results/excalidraw/packaging-macos.json`](tests/results/excalidraw/packaging-macos.json) |
| End-to-end flow (20 steps) | **Passing**: Chromium **20/20**. Native **20/20** headless and **20/20** in real windows on Metal, with **20/20** DOM checkpoints matched in each | [`tests/results/excalidraw/e2e-macos.json`](tests/results/excalidraw/e2e-macos.json), [`tests/results/excalidraw/e2e-windowed-macos.json`](tests/results/excalidraw/e2e-windowed-macos.json) |
| Native smoke | **Passing**: the packed app mounts, renders and reports its scene | [`tests/results/platform/excalidraw-native-macos.json`](tests/results/platform/excalidraw-native-macos.json) |

### On Windows

The same build also runs on Windows. These numbers come from an NVIDIA
GeForce RTX 4060 Ti through Direct3D 12 on Windows x64, with the stripped
`dist` runtime. Like the macOS numbers, they are recorded separately from the
Linux verdict:

| Check | Result | Evidence |
| --- | --- | --- |
| Packaging & no patches | **Passing**: one PE32+ x86-64 GUI executable, `dist\excalidraw.exe`, **90.8 MiB** (`95,169,906` bytes, limit 150 MiB). `vendor/excalidraw` clean; isolated launch verified. Its DLL imports are not checked yet | [`tests/results/excalidraw/packaging-windows.json`](tests/results/excalidraw/packaging-windows.json) |
| End-to-end flow (20 steps) | **Passing**: Chromium **20/20**. Native **20/20** headless and **20/20** in real windows on Direct3D 12, with **20/20** DOM checkpoints matched in each | [`tests/results/excalidraw/e2e-windows.json`](tests/results/excalidraw/e2e-windows.json), [`tests/results/excalidraw/e2e-windowed-windows.json`](tests/results/excalidraw/e2e-windowed-windows.json) |

## What works

- **Static HTML and CSS** render natively and match Chromium.
- **React apps** run on the real `react-dom`, driven by real input:
  TodoMVC, a counter, shadcn/ui and a 10,000-row virtual list.
- **Interactive features:** text editing and IME, scrolling, CSS animations
  and transitions, CSS-in-JS, and live reload with `squark dev`.
- **Excalidraw's platform:** Canvas 2D, fonts, storage, clipboard and file
  dialogs.
- **A full desktop IDE** (React 19, Monaco, xterm.js, KaTeX, a Tauri 2 Rust
  backend) runs with its backend linked in as native modules: editing,
  completion, hover and the diff editor in Monaco, a terminal on a real PTY,
  git, search, plots, tab drag and drop, and its menu's accelerators.
- **Platform features those apps rely on:** Web Workers, HTML drag and drop,
  Range and Selection with caret hit testing, custom elements and Shadow
  DOM, constructable stylesheets, CSS tables, `@font-face` weights and
  styles, synthetic bold and italic, window focus and visibility, streaming
  `TextDecoder`, window title, zoom and close handling.
- **Excalidraw v0.18.1 ships as one native Linux binary, a signed macOS
  app and a Windows executable,** each passing the same flow against
  Chromium.
- **macOS:** Metal rendering, real windows and input (IME composition is
  tested through test control only), storage
  in `~/Library/Application Support`, ⌘ shortcuts (apps see a Mac
  `navigator.platform`), and live reload with `squark dev`.

## Next

- Windows: check the executable's DLL imports in the packaging check, and
  match Chromium's `<input>` widths in Segoe UI.
- More platforms: Android and iOS.
- Direct mode: a React host that writes journal records directly, without
  react-dom and squark-dom, for apps that want less memory per node.
- QuickJS-ng as a smaller JavaScript engine option.
- iframes (the design is recorded), then a drawn menu bar on Linux built on
  them, and OS menu bars on macOS and Windows.
- Drops from other apps, painted drag images, and scrollbars.

## Building from source

You need:

- **Linux** with a Vulkan driver, `pkg-config` and the fontconfig
  headers (`libfontconfig1-dev` on Debian and Ubuntu); or **macOS 12+** with
  the Xcode command line tools (`xcode-select --install`); or **Windows
  10+** with the Visual Studio Build Tools' "Desktop development with C++"
  workload;
- Rust through [rustup](https://rustup.rs), which installs the pinned
  toolchain automatically;
- Node.js 26.

```sh
git clone https://github.com/project-strange-quark/strange-quark.git
cd strange-quark
git submodule update --init   # only needed for the Excalidraw target
npm install
npm run build --workspaces --if-present   # the JS packages; squark dev needs
                                          # the built Vite plugin
cargo build --workspace       # target/debug/squark and squark-runtime
node node_modules/playwright-core/cli.js install chromium
                              # the pinned Chromium the tests compare against
scripts/check.sh              # the full test and lint suite
                              # (node scripts/check.mjs on Windows)
```

`cargo build --profile dist -p squark-host --bin squark-runtime -p squark-cli --bin squark`
builds the stripped runtime that `squark build` packs by default, in
`target/dist/`.

## Documentation

- [Configuration and CLI reference](docs/configuration.md): every
  `squark.config` option, command, flag and environment variable.
- [Native modules](docs/native-modules.md): calling Rust from your app,
  menus, window control, and porting a Tauri 2 app.
- [Architecture](docs/architecture.md): the design, module by module.
- [Migration skill](skills/strange-quark-migrate/SKILL.md): a guide (and an
  agent skill) for porting an existing React, Electron, Tauri or
  meta-framework app, with measurement scripts.
- [Changelog](CHANGELOG.md): what each release added and changed.
- [Releasing](scripts/npm/README.md): how the npm packages are built,
  staged and published.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), which includes our policy on AI
tools. Report security problems privately, as [SECURITY.md](SECURITY.md)
describes. The rules for working on this repository are in
[AGENTS.md](AGENTS.md), and the design is in
[docs/architecture.md](docs/architecture.md).

## History

Strange Quark was developed as a private project under a different name.
This repository starts from a snapshot of that work, without its earlier
commit history.

## Licence

MIT. See [LICENSE](LICENSE). Third-party code in this repository is listed in
[NOTICE](NOTICE), and the third-party software in the shipped binaries, with
its licences, in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).

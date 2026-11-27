# What Strange Quark supports

This file describes what v0.1.0, the first release, targets, with the
0.2.0 work added, not the support in any particular build. It covers the six desktop targets under
[Platforms](#platforms), in Compat mode, on V8. Run `scripts/probe.mjs`
against the installed runtime to learn what that binary actually supports.
Its results take precedence over this target snapshot. `scripts/compat.json` is the machine-readable form that `scan.mjs`
uses. Keep the two in step.

The target intends to report features truthfully. An API it lacks is left
undefined, as in a browser without it, so feature-detecting code takes its
fallback. Code that calls an API without checking first throws.

## Works (tier 1)

| Area | What is supported |
| --- | --- |
| React | React 18.3 and 19.x on the real `react-dom`, unmodified. Fiber, concurrent features, Suspense, portals, refs, controlled inputs, synthetic events |
| DOM | Node, Element, Text, Comment, DocumentFragment, Document; selectors; MutationObserver; `innerHTML`; `template`; `DOMParser`; `document.implementation`; NodeIterator and TreeWalker; custom elements; Shadow DOM (open and closed, slots, `:host`, `::slotted`, `::part`, event retargeting); Range with `getClientRects`; the Selection API; `caretPositionFromPoint` and `caretRangeFromPoint` |
| Events | Capture, bubble, `passive`, `once`, `signal`. Pointer, mouse, keyboard, focus, input, composition (IME), wheel, touch, `contextmenu` and custom events. Clipboard events with data. HTML drag and drop within the page. Window focus, blur and `visibilitychange` |
| CSSOM | `element.style`, `CSSStyleSheet` and `insertRule` (emotion, styled-components), constructable sheets and `adoptedStyleSheets`, `getComputedStyle`, `matchMedia`, `CSS.supports` |
| Geometry | Rects, offset, client and scroll metrics; `scrollTo`; `scrollIntoView`; ResizeObserver; IntersectionObserver; `elementFromPoint` |
| Timing | Timers, `queueMicrotask`, MessageChannel, rAF, `requestIdleCallback`, `performance` (with `mark` and `measure`) |
| Workers | Dedicated Web Workers, module and classic, from pack, `blob:` and dev-server URLs (Vite's `?worker` included), each a V8 isolate on its own thread |
| Network | `fetch`, Headers, Request, Response, AbortController, WebSocket, EventSource. Every host must be listed in `squark.config` `network.allow`; there are no cookies and no CORS |
| Storage and data | File-backed `localStorage`, IndexedDB (enough for idb-keyval and similar), `structuredClone`, `crypto.getRandomValues`, `crypto.randomUUID`, `crypto.subtle` (SHA digests, AES-GCM), Blob, File, FileReader, `blob:` URLs, TextEncoder, TextDecoder (streaming, every WHATWG encoding) |
| Graphics | `<img>` (PNG, JPEG, WebP, AVIF, GIF), inline SVG, Canvas 2D on Vello (the method set Excalidraw uses: paths, `Path2D`, text, `drawImage`, transforms, clip, dashes, alpha, filter, `getImageData`, `toBlob`), FontFace and `document.fonts` with WOFF2 |
| Forms | Text, number, password, checkbox, radio and range inputs; textarea; select; button; label; form submit |
| Files | `<input type=file>` opens a native open dialog. `<a download>` opens a native save dialog. `navigator.clipboard` |
| Navigation | History API (in memory), `location`, `<a href>` routed to the app or to the system browser |
| WebAssembly | Yes, through V8 |
| CSS | Everything Stylo parses for selectors and values, including `:has()`, `@layer`, `@property`, custom properties, `color-mix()` and `oklch()`. Layout: block, inline, flex, grid, tables, all positions, overflow, sticky, `aspect-ratio`. Text: `@font-face` matching by weight, style and `unicode-range`, synthetic bold and italic, `ex`/`ch`/`cap` from the font. Paint: gradients, borders, radius, shadows, transforms, opacity, filter, `backdrop-filter`, blend modes, `clip-path`. Transitions and `@keyframes`. Tailwind v4 |

## Not yet (tier 2: planned)

These are undefined or ignored today, and a later release may add them:

- SharedWorker, and MessagePorts sent to a worker
- Declarative shadow DOM (`<template shadowrootmode>`), `moveBefore`
- Container queries (`@container`), floats, subgrid, anchor positioning,
  `mask-image`, `border-image`; in tables, collapsed borders, captions and
  `<col>` widths
- `contenteditable`, so Lexical, Slate, ProseMirror, TipTap and other
  rich-text editors do not work
- Drops from other apps (files from the desktop) and painted drag images
- iframes and nested browsing contexts, designed for after 0.2.0.
  `HTMLIFrameElement` exists, but loads nothing.
- A drawn menu bar on Linux: menus set by native modules or the page work
  through their accelerators only
- Visible scrollbars: scrolling works, but none is drawn
- The Web Animations API (`element.animate`), scroll-driven animations,
  view transitions
- `<video>` and `<audio>`
- `navigator.gpu` (WebGPU)
- Multiple windows

## Not planned

- **Server-side anything:** SSR, React Server Components, server actions,
  route handlers, middleware. This is why meta-frameworks need migrating.
- WebGL (three.js, react-three-fiber, map renderers built on WebGL)
- Service workers, `document.write`, XPath, cross-origin navigation,
  cookies (`document.cookie` reads `""`), CORS
- Node.js APIs in the UI (`require('fs')`, `process`, `Buffer` from Node)
- Electron and Electrobun shell APIs. A Tauri 2 app can keep its Rust
  backend as native modules (`docs/architecture.md` §13) through the
  optional `squark-tauri` crate and `@strange-quark/tauri` package; see
  `tauri.md`.

## Platforms

v0.1.0 ships a prebuilt runtime for six desktop targets:

| Target | GPU API | `squark build` output |
| --- | --- | --- |
| macOS 12+, arm64 (Apple Silicon) and x64 (Intel) | Metal | `dist/<name>.app`, signed ad hoc |
| Linux x64 and arm64, glibc 2.35+, Wayland and X11 | Vulkan | `dist/<name>` |
| Windows 10+, x64 and arm64 | Direct3D 12 | `dist\<name>.exe` |

glibc 2.35 or newer means Ubuntu 22.04, Debian 12, Fedora 36 or a later
release. The targets are not all tested to the same depth. Excalidraw
passes against Chromium on Linux x64, macOS on Apple Silicon and Windows
x64. Linux arm64 is built but not yet tested on hardware, and Windows arm64
was tested in a VM without a GPU.

Android, iOS and embedded Linux are planned, not in v0.1.0. Say so in a
report whose user needs one of them, or an older Linux.

A measurement holds for the platform it ran on. Name that platform in the
report, and list the user's other target platforms under "Not tested".

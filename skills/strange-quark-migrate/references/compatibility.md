# What Strange Quark supports

This file describes the first release's target, not the support in any
particular build. The target platform is Linux desktop, Compat mode, V8.
Run `scripts/probe.mjs` against the installed runtime to learn what that
binary actually supports. Its results take precedence over this target
snapshot. `scripts/compat.json` is the machine-readable form that `scan.mjs`
uses. Keep the two in step.

The target intends to report features truthfully. An API it lacks is left
undefined, as in a browser without it, so feature-detecting code takes its
fallback. Code that calls an API without checking first throws.

## Works (tier 1)

| Area | What is supported |
| --- | --- |
| React | React 18.3 and 19.x on the real `react-dom`, unmodified. Fiber, concurrent features, Suspense, portals, refs, controlled inputs, synthetic events |
| DOM | Node, Element, Text, Comment, DocumentFragment, Document; selectors; MutationObserver; `innerHTML`; `template`; `DOMParser` |
| Events | Capture, bubble, `passive`, `once`, `signal`. Pointer, mouse, keyboard, focus, input, composition (IME), wheel, touch and custom events. Clipboard events with data |
| CSSOM | `element.style`, `CSSStyleSheet` and `insertRule` (emotion, styled-components), `getComputedStyle`, `matchMedia`, `CSS.supports` |
| Geometry | Rects, offset, client and scroll metrics; `scrollTo`; `scrollIntoView`; ResizeObserver; IntersectionObserver; `elementFromPoint` |
| Timing | Timers, `queueMicrotask`, MessageChannel, rAF, `requestIdleCallback`, `performance` |
| Network | `fetch`, Headers, Request, Response, AbortController, WebSocket, EventSource. Every host must be listed in `squark.config` `network.allow`; there are no cookies and no CORS |
| Storage and data | File-backed `localStorage`, IndexedDB (enough for idb-keyval and similar), `structuredClone`, `crypto.getRandomValues`, `crypto.randomUUID`, `crypto.subtle` (SHA digests, AES-GCM), Blob, File, FileReader, `blob:` URLs, TextEncoder |
| Graphics | `<img>` (PNG, JPEG, WebP, AVIF, GIF), inline SVG, Canvas 2D on Vello (the method set Excalidraw uses: paths, `Path2D`, text, `drawImage`, transforms, clip, dashes, alpha, filter, `getImageData`, `toBlob`), FontFace and `document.fonts` with WOFF2 |
| Forms | Text, number, password, checkbox, radio and range inputs; textarea; select; button; label; form submit |
| Files | `<input type=file>` opens a native open dialog. `<a download>` opens a native save dialog. `navigator.clipboard` |
| Navigation | History API (in memory), `location`, `<a href>` routed to the app or to the system browser |
| WebAssembly | Yes, through V8 |
| CSS | Everything Stylo parses for selectors and values, including `:has()`, `@layer`, `@property`, custom properties, `color-mix()` and `oklch()`. Layout: block, inline, flex, grid, all positions, overflow, sticky, `aspect-ratio`. Paint: gradients, borders, radius, shadows, transforms, opacity, filter, `backdrop-filter`, blend modes, `clip-path`. Transitions and `@keyframes`. Tailwind v4 |

## Not yet (tier 2: planned)

These are undefined or ignored today, and a later release may add them:

- Web Workers and SharedWorker (`typeof Worker === "undefined"`)
- Shadow DOM, custom elements, Range, `moveBefore`
- Container queries (`@container`), floats, real table layout (tables are
  approximated with grid), subgrid, anchor positioning, `mask-image`,
  `border-image`
- `contenteditable` and the full Selection API, so Lexical, Slate,
  ProseMirror, TipTap and other rich-text editors do not work
- Drag and drop
- The Web Animations API (`element.animate`), scroll-driven animations,
  view transitions
- `<video>` and `<audio>`
- `navigator.gpu` (WebGPU)
- Multiple windows

## Not planned

- **Server-side anything:** SSR, React Server Components, server actions,
  route handlers, middleware. This is why meta-frameworks need migrating.
- WebGL (three.js, react-three-fiber, map renderers built on WebGL)
- iframes and nested browsing contexts. `HTMLIFrameElement` exists, but
  loads nothing.
- Service workers, `document.write`, XPath, cross-origin navigation,
  cookies (`document.cookie` reads `""`), CORS
- Node.js APIs in the UI (`require('fs')`, `process`, `Buffer` from Node)
- Electron, Tauri or Electrobun shell APIs. The native-module escape hatch
  (architecture map §13) is not in the first release.

## Platforms

The first release targets Linux x86_64: Wayland and X11, Vulkan. Windows,
macOS and mobile are planned. Say so in a report whose user needs another
platform.

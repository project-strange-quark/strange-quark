# Strange Quark

*Architecture map · design proposal · rev 0.1*

> **This is the design document.** It records the intended design of Strange Quark and the reasoning behind it. Sections that describe parts not built yet say so. For what works today, see [What works](../README.md#what-works) in README.md.

Strange Quark runs real React apps and their npm libraries without a browser engine. Unmodified react-dom talks to a DOM written in JavaScript. That DOM streams its mutations into a Rust document, which Stylo styles, Taffy and Parley lay out, and wgpu draws on Vulkan, Metal or D3D12 (GLES is planned).

- **Input:** React 18.3 and 19.x apps, npm libraries, CSS from Tailwind, CSS Modules and CSS-in-JS

- **Output:** One native binary per target (an app bundle on macOS), no WebView

- **Core crates:** Stylo · Taffy · Parley · Vello · wgpu · winit · AccessKit (planned)

- **JS engine:** V8 by default; QuickJS-ng for small and JIT-less targets is planned

## §1 System map

Writes flow from JavaScript down to the GPU, once per frame. Two paths flow back up. Input enters on the main thread and reaches squark-dom as a queued task. Layout reads flush the journal and run style and layout before they return, so JavaScript always sees a consistent document. Services feed the document thread, and decoded pixels and glyphs reach the GPU renderer through an ordered resource stream.

- **react-dom stays unmodified.** Compatibility is the DOM’s job. A library that works on react-dom in Chrome and uses only tier 1 APIs (§11) works here without patches or aliases.

- **Writes batch, reads are synchronous.** A DOM mutation appends a record and never leaves JavaScript. Only reads that need style or layout cross into Rust, and they flush first.

- **The render thread never waits for JavaScript.** It always holds the last committed frame. Scrolling, transform and opacity animations, and presentation continue while the document thread is busy.

## §2 Modules

Fifteen modules. Each one runs on one thread, or splits cleanly between two, as its chip shows. Package names are npm packages; the rest are Rust crates.

### react + react-dom

Document · JS

react-dom/client · upstream

Upstream React, run exactly as published to npm. Strange Quark does not fork, patch or alias it.

- **Does:** Fiber reconciliation, lane-based scheduling with 5 ms time slices, commit, synthetic events, and React 19 hoisting of `<title>`, `<meta>` and `<link>`.

- **Needs:** `createElement(NS)`, `createTextNode`, `insertBefore`, `removeChild`, `setAttribute`, `node.style`, `addEventListener` on the root container, `MessageChannel`, `queueMicrotask`.

- **Relies on:** Expando properties on nodes (`__reactFiber$…`, `__reactProps$…`) and a `value` accessor on the input prototype, which its controlled-input tracker reads. Both work because squark-dom nodes are ordinary JS objects.

### squark-dom

Document · JS

@strange-quark/dom

A DOM written in JavaScript. It is the only thing React and libraries talk to.

- **Owns:** The node tree, attributes, listeners, CSSOM objects, observer registrations, focus and tab order.

- **Does:** WHATWG DOM core, the HTML element interfaces apps use, UI Events and Pointer Events dispatch, `element.style`, `CSSStyleSheet.insertRule`, `getComputedStyle`, MutationObserver, ResizeObserver and IntersectionObserver (entries arrive from Rust after layout), timers, `requestAnimationFrame`, History API.

- **Crosses:** Every mutation appends a journal record. Layout reads call `__squark.query()`. `innerHTML` calls `__squark.parseFragment()`.

### Journal

Document · JS → Rust

@strange-quark/dom/journal → squark-journal

The only write path from JavaScript into Rust.

- **Owns:** A growable `ArrayBuffer` of 16-byte records, a string table, and atom tables for tag, attribute and CSS property names.

- **Does:** Encodes each mutation as one record: opcode, flags and three u32 operands. The buffer is shared with Rust without copying.

- **Flushes:** At frame step 4, before any synchronous read, and whenever a 64 KiB chunk fills.

### Document mirror

Document · Rust

squark-mirror

The Rust copy of the document that style, layout and paint read.

- **Owns:** Node arena (slot map keyed by u32 id and generation), child links, attributes, parsed inline declarations, element state (hover, focus, active, checked, disabled), stylesheet registry.

- **Does:** Applies records and sets restyle, relayout, repaint and accessibility dirty bits, propagating them to ancestors.

- **Built on:** Implements Stylo’s `TDocument`, `TNode` and `TElement` traits, so Stylo traverses it directly.

### Style

Document + pool

squark-style

Selector matching and the cascade, delegated to Stylo.

- **Does:** Restyles dirty subtrees, in parallel on the worker pool above a configurable size. Bloom-filter selector matching, the style sharing cache, origins, `@layer`, specificity, `!important`, custom properties, media queries against the window. Turns class, attribute and state changes into restyle hints. Starts CSS transitions and animations.

- **Outputs:** Computed values per element, split into layout, text, paint and animation groups.

- **Built on:** Stylo, the style engine Servo and Firefox share.

### Layout

Document · Rust

squark-layout

Turns computed styles into positioned boxes.

- **Does:** Converts computed values to Taffy styles and runs block, flexbox and grid. Builds an inline formatting context with Parley when a block has inline children. Places absolute and fixed boxes against their CSS containing block, records sticky constraints, creates scroll frames.

- **Caches:** Per-node results. Dirty bits stop at layout boundaries: `contain: size layout`, or a fixed-size box with `overflow: hidden`.

- **Outputs:** Fragment tree: border-box rects, padding and border widths, baselines, scrollable overflow, positioned glyph runs.

### Text

Document + pool

squark-text

Fonts, shaping, line breaking, glyph rasterization and text editing.

- **Does:** Font discovery and fallback (system fonts and bundled `@font-face`), shaping, bidi, line breaking, hinted glyph rasterization on the pool, color glyphs (COLRv1, CBDT, sbix), and the editing state of every text field: text, selection, IME preedit.

- **Built on:** Parley, fontique, HarfRust, swash and skrifa.

### Paint

Document · Rust

squark-paint

Records what to draw. It never touches the GPU.

- **Does:** Walks fragments in CSS painting order (stacking contexts, then the layers of CSS 2.1 Appendix E), emits display items, builds property trees (transform, clip, effect, scroll) that items reference by id, builds the hit-test tree with listener regions, and diffs against the previous frame for damage.

- **Outputs:** `Frame { display_list, property_trees, hit_test, resource_updates, damage }`

### Compositor

Render thread

squark-compositor

Keeps the screen moving while JavaScript is busy.

- **Owns:** Scroll offsets, running compositor animations, the layer tree, per-tile damage.

- **Does:** Takes frames from a latest-wins mailbox. Applies scroll offsets, sticky offsets and transform, opacity and filter animations to the property trees without repainting. Groups items into layers by scroll frame and effect, tracks damage per tile, and publishes hit-test data and scroll offsets.

### GPU renderer

Render thread

squark-gpu

Turns layers into wgpu command buffers.

- **Owns:** wgpu device and queue, pipelines, glyph atlases, image atlas, gradient ramp texture, effect surfaces, and the tile cache's textures.

- **Does:** Uploads through a staging belt, batches by pipeline and bind group, draws opaque items front to back with a depth test and translucent items back to front, renders paths with Vello, composites tiles to the swapchain, presents.

- **See:** §8 for pipelines and passes.

### Main thread host

Main thread

squark-host

Owns the window and everything the OS sends to it.

- **Does:** Runs the winit event loop, which macOS and iOS require on the main thread. Converts physical to CSS pixels, hit-tests against the compositor’s latest data, sends scroll gestures straight to the compositor, queues DOM events on the document thread, and handles IME, cursor, clipboard, resize and DPI changes.

- **Built on:** winit, arboard, raw-window-handle.

### Accessibility

Document → main

squark-a11y (planned, not built)

Exposes the document to screen readers and other assistive technology. Neither the crate nor the AccessKit integration exists yet.

- **Does:** Builds an AccessKit tree from element semantics, ARIA attributes, label relationships and layout bounds on the document thread, then sends incremental updates to the adapter on the main thread. Assistive-technology actions (focus, click, set value, scroll into view) come back as DOM events.

- **Built on:** AccessKit adapters for UI Automation, NSAccessibility, AT-SPI and Android.

### Assets

Worker pool

squark-assets

Loads and decodes everything that is not code.

- **Does:** Memory-maps the asset pack in release builds and fetches from the dev server in development. Decodes PNG, JPEG, WebP, AVIF and GIF frames on the pool, loads fonts, reports intrinsic sizes to layout and pixels to the renderer as resource updates, and evicts least-recently-used textures over budget. AVIF decoding is not built yet.

### Network & IO

IO pool

squark-net

Backs fetch, WebSocket and EventSource.

- **Does:** Streaming `fetch` with `AbortController`, `WebSocket`, `EventSource`, and file reads for native modules once they exist (§13). Requests are checked against the host allowlist, `network.allow` in `squark.config.ts`. Completions are queued as tasks on the document thread.

- **Built on:** tokio, reqwest (over hyper), rustls, tokio-tungstenite.

### JS engine adapter

Document thread

squark-js

One interface over the engines Strange Quark can embed.

- **Does:** Module loading, microtask checkpoints, the shared journal buffer, fast native calls for `__squark.query`, `FinalizationRegistry` hooks, startup snapshots or bytecode, and the inspector protocol for Chrome DevTools.

- **Backends:** V8 through rusty_v8 by default. QuickJS-ng for small and JIT-less targets.

- **Built so far:** V8 only, with compiled modules kept in an on-disk code cache. Fast native calls, startup snapshots, the inspector and the QuickJS-ng backend are planned.

## §3 Frame lifecycle

A frame starts at vsync. The document thread runs steps 1 to 9 and must commit before a deadline. The render thread and the GPU finish the frame while the document thread moves on to the next one.

The budgets below are for 60 Hz. They are targets for a mid-range laptop, not measurements. At 120 Hz the frame is 8.3 ms, and the commit deadline is computed per display from the render and GPU times of recent frames.

| # | Step | Thread | Target | What happens |
| --- | --- | --- | --- | --- |
| 1 | Input | Document | 0.5 ms | Drain queued input. `pointermove` is coalesced to one dispatch per frame. |
| 2 | Tasks and microtasks | Document | ≤ 4 ms | Timers, `MessageChannel` tasks, network completions. React’s scheduler yields every 5 ms, so a long render spreads over several frames instead of blocking one. |
| 3 | Rendering events, rAF | Document | 1 ms | `resize` and `scroll` events, media query changes, then `requestAnimationFrame` callbacks. |
| 4 | Flush | Document | 0.2 ms | Apply journal records to the mirror. |
| 5 | Style | Document + pool | 1.5 ms | Restyle dirty elements. Start CSS transitions and animations. |
| 6 | Layout | Document | 2 ms | Relayout dirty subtrees. |
| 7 | Observers | Document | 0.5 ms | ResizeObserver callbacks. If they dirty layout, steps 4 to 7 repeat for deeper targets, as the spec requires, and then the loop error is reported. IntersectionObserver entries follow. |
| 8 | Paint | Document | 1 ms | Display list, property trees, hit-test tree, damage. |
| 9 | Commit | Document → render | 0.1 ms | Move the frame into the mailbox before the deadline (11 ms at 60 Hz). |
| 10 | Layerize | Render | 0.4 ms | Apply scroll and animation values, compute layers and tile damage. |
| 11 | Upload | Render | 1 ms | Glyphs, images and gradient ramps through the staging belt. |
| 12 | Batch and encode | Render | 1.5 ms | Fill instance buffers and record the passes in §8. |
| 13 | Submit | Render | 0.3 ms | `queue.submit()` |
| 14 | Execute | GPU | ~2 ms | Vello compute, effect surfaces, tile passes, composite. |
| 15 | Present | GPU | vsync | The frame reaches the screen. |

**Missed deadline.** The compositor presents the previous frame again, with current scroll offsets and compositor animations applied, so the screen keeps moving. The late frame shows at the following vsync. Only content changes are delayed.

**Forced layout.** A layout read in step 2 or 3 runs steps 4 to 6 early for the dirty subtree only. When step 4 comes, there is less left to do. Interleaving writes and reads in a loop repeats that work, which the planned forced-layout detector reports (§16).

### Startup

- The process starts on the main thread and creates the window, hidden.

- In parallel: the render thread requests a wgpu adapter and device, the document thread starts the JS engine (from a startup snapshot, once those are built), and the asset pack is memory-mapped.

- `document.bin` builds the initial document from `index.html` (head, body, `#root`) without parsing HTML at runtime.

- The app's scripts run. squark-dom is installed in the realm before them, so `window` and `document` exist before any library checks `typeof window`. Stylo parses the app's stylesheets.

- `createRoot(…).render(<App />)` commits, and frame steps 4 to 15 produce the first frame.

- The window becomes visible when that frame presents, so nobody sees an empty window.

## §4 The JS–Rust boundary

react-dom makes thousands of DOM calls to mount a screen, and many of them are reads: `parentNode`, `nodeType`, `ownerDocument`, `firstChild`. If the DOM lived in Rust, each of those would be a native call with wrapper bookkeeping. Strange Quark keeps the tree in JavaScript, where those reads are property lookups, and sends Rust a journal of writes. Only questions about style and layout cross, and they flush the journal first.

When a Radix popover opens, Floating UI measures in a layout effect, right after React’s commit. The first read pays for the flush, restyle and relayout. The second is a cache hit. Writing `transform` costs one record and no layout, because transforms live in the property trees.

**Three calls carry the document traffic from JavaScript to Rust:** `__squark.flush(buffer, length)`, `__squark.query(kind, nodeId, arg)` and `__squark.parseFragment(html)`. Other `__squark` functions back Web API facades such as Canvas 2D, fonts, crypto and the network. In the other direction, Rust queues tasks: DOM events with a target id, observer entries, scroll offsets, and resource events such as an image’s `load` with its natural size.

### Journal records

Each record is 16 bytes: an opcode byte, a flags byte, two bytes of padding and three u32 operands. Strings go into a side table and are referenced by index. Tag, attribute and property names are atoms, interned once per session.

| Code | Record | Operands | Written by |
| --- | --- | --- | --- |
| 0x01 | CREATE_ELEMENT | id, tag atom, namespace atom | `createElement`, `createElementNS` (SVG) |
| 0x02 | CREATE_TEXT | id, string | `createTextNode` |
| 0x03 | INSERT | parent, child, before (0 appends) | `appendChild`, `insertBefore` |
| 0x04 | REMOVE | parent, child | `removeChild`, `remove()` |
| 0x05 | SET_ATTR | id, name atom, string | `setAttribute`, `className`, `id` |
| 0x06 | REMOVE_ATTR | id, name atom | `removeAttribute` |
| 0x07 | SET_TEXT | id, string | `textContent`, `nodeValue` |
| 0x08 | SET_STYLE | id, property atom, string (empty removes) | `style[prop] = v`, `style.setProperty('--x', v)` |
| 0x09 | SET_STATE | id, state bits, mask | `checked`, `disabled`, `selected` |
| 0x0A | SET_VALUE | id, string | `input.value = v` from controlled inputs |
| 0x0B | SHEET_INSERT | sheet id, index, string | `insertRule`, text of a `<style>` element |
| 0x0C | SHEET_DELETE | sheet id, index | `deleteRule` |
| 0x0D | SCROLL_TO | id, x (f32 bits), y (f32 bits) | `scrollTop = v`, `scrollTo()`, `scrollIntoView()` |
| 0x0E | FOCUS | id, flags (blur, preventScroll) | `focus()`, `blur()` |
| 0x0F | RELEASE | id | `FinalizationRegistry` callback for a detached node |

The code adds records 0x10 to 0x17: atom definitions, Canvas 2D submits and sizes, font faces, other node types, listener flags, text selection and IME preedit. `crates/squark-types/src/journal.rs` has the full list.

### Synchronous reads

| Read | Forces | Notes |
| --- | --- | --- |
| `getBoundingClientRect()`, `getClientRects()` | flush, style, layout | Viewport-relative, using the scroll offsets the compositor last synced. |
| `offset*`, `client*`, `scrollWidth`, `scrollHeight` | flush, style, layout |  |
| `getComputedStyle(el)[prop]` | flush, style; layout only for resolved-value properties | `width`, `height`, `margin-*`, `padding-*` and insets return used values, per CSSOM. |
| `querySelector(All)`, `matches`, `closest` | flush | Matched in Rust by Stylo’s selector engine, so the result is the same one style uses. |
| `elementFromPoint()` | flush, style, layout, hit-test rebuild |  |
| `focus()` | flush, style, layout unless `preventScroll` | Focusing may scroll the element into view. |
| `getBBox()`, `getComputedTextLength()` | flush, style, layout | SVG geometry for chart libraries. |
| `scrollTop`, `scrollLeft` | nothing | Cached in JS, updated from the compositor each frame and before each `scroll` event. |
| `matchMedia()`, `CSS.supports()` | nothing | Answered from window state and the support tables in §6. |

### Node identity and lifetime

The JS object is the node, so `===`, expandos and `WeakMap` keys behave as they do in a browser. Each node gets a u32 id at creation. Rust keeps a node alive as long as JavaScript can reach it, attached or not, so a library can remove a subtree and insert it again later. When the garbage collector reclaims a detached node, a `FinalizationRegistry` callback writes RELEASE. Ids are reused only after Rust confirms the release, and a generation counter rejects any stale record that names a reused slot.

## §5 Compat and Direct modes

Compat mode is the default and the reason Strange Quark exists. Direct mode is an opt-in for apps that do not need the npm DOM ecosystem and want less memory per node. A root uses one mode or the other.

Only Compat mode is built. Direct mode and `@strange-quark/reconciler` are planned.

The two modes differ in one layer. Direct mode replaces react-dom and squark-dom with a react-reconciler host config whose `createInstance`, `appendChild` and `commitUpdate` write journal records directly.

|  | Compat (default) | Direct |
| --- | --- | --- |
| Renderer | `react-dom/client` | `@strange-quark/reconciler` |
| Host nodes in JS | One squark-dom object per element and text node | None. `ref.current` is a `NodeHandle` with `measure()`, `focus()`, `scrollTo()` |
| Libraries that use DOM refs, portals, CSS-in-JS | Work | Do not work |
| Styling | Any CSS | Bundled CSS, `className` and inline `style` only |
| Use it for | Apps built from the npm ecosystem | Kiosks, embedded panels, apps written for Strange Quark |

## §6 Style

Stylo parses and computes nearly every property Firefox supports, so the cascade is not the limit. What Strange Quark can show is limited by what layout and paint implement. The support table below is about those.

Five sources feed one cascade, which has four consumers. Inline styles arrive as SET_STYLE records; React writes custom properties with `setProperty` and everything else by assignment, and both produce the same record. Stylesheets from CSS-in-JS arrive as SHEET_INSERT records, one per `insertRule`.

**Invalidation.** A class, attribute or state change produces a restyle hint scoped by Stylo’s invalidation map, so toggling `.open` on a menu restyles the elements whose selectors mention `.open`, not the document. Changing a custom property on an ancestor restyles the descendants that use it. Computed values are split into groups, and only changed groups dirty their consumer: a color change repaints without relayout, a `transform` change updates a property tree node without repaint.

| Area | Tier 1 · v1 | Tier 2 · planned | Not planned |
| --- | --- | --- | --- |
| Selectors, cascade | Everything Stylo supports, including `:has()`, `:is()`, `:where()`, `:focus-visible`, `@layer`, `@supports`, `@property` |  |  |
| Values | Custom properties, `calc()` `min()` `max()` `clamp()`, `color-mix()`, `oklch()` `oklab()` `lab()`, `rem` `dvh` `svh` `lvh` |  |  |
| Layout | block, inline, inline-block, flex, grid, `contents`, all `position` values, overflow, `aspect-ratio`, `gap`, `z-index`, `contain` | Container queries (needs style and layout to interleave), floats, true table layout, subgrid, anchor positioning | Multi-column, vertical writing modes, regions, shapes |
| Text | `font-*`, variable fonts, `@font-face` (WOFF2, TTF, OTF), `line-height`, spacing, `white-space`, `word-break`, `overflow-wrap`, `text-overflow: ellipsis`, `-webkit-line-clamp`, decorations, `text-shadow`, bidi | `hyphens`, `text-wrap: balance \| pretty`, ruby | Vertical text |
| Paint | Backgrounds with linear, radial and conic gradients; borders (solid, dashed, dotted, double); `border-radius`; `box-shadow`; `outline`; `opacity`; 2D and 3D transforms; `filter`; `backdrop-filter`; `mix-blend-mode`; `clip-path`; `accent-color`; `cursor`; `pointer-events` | `mask-image`, `border-image`, `background-blend-mode` |  |
| Motion | Transitions, `@keyframes`, `will-change`. The compositor runs `transform`, `opacity` and `filter`; the document thread runs the rest | Scroll-driven animations, view transitions |  |
| Media queries | Width, height, orientation, `prefers-color-scheme`, `prefers-reduced-motion`, `prefers-contrast`, resolution, `hover`, `pointer` |  | Print, `@page` |

Tailwind CSS v4 output needs `@layer`, `@property`, `oklch()` and `color-mix()`, all tier 1. Its container-query utilities wait for tier 2. When layout or paint ignores a computed value, the dev build warns once per property and stylesheet location.

## §7 Layout

The layout tree is built from the mirror and computed styles: `display: none` produces no box, `display: contents` promotes its children, and `::before`, `::after` and `::marker` add generated boxes. Each box then goes to the algorithm its `display` asks for.

| CSS | Algorithm | Implementation |
| --- | --- | --- |
| `block`, `flow-root`, `list-item` | Block layout with margin collapsing | Taffy block, plus a generated marker box for list items |
| `flex`, `inline-flex` | Flexbox | Taffy flexbox |
| `grid`, `inline-grid` | Grid without subgrid | Taffy grid |
| `inline`, `inline-block`, text | Inline formatting context | Parley lays out the text runs. Each inline-block, inline-flex or replaced element is an inline box whose size comes from a nested Taffy pass through Taffy’s measure hook. |
| `table` and its parts | Table layout | v1 maps tables onto Taffy grid with auto columns and `colspan` as spans, and warns. True table layout is tier 2. |
| `position: absolute` | Out of flow, containing block is the nearest positioned ancestor | A second pass after Taffy, because Taffy positions absolute children against their parent |
| `position: fixed` | Containing block is the viewport, or the nearest transformed ancestor | Same second pass |
| `position: sticky` | In flow, offset during scrolling | A constraint rect for the compositor, so sticky headers move without JavaScript |
| `overflow: auto \| scroll \| hidden \| clip` | Scroll container or clip | A scroll frame node in the scroll tree, with content size and `scrollbar-gutter` |

**Incremental layout.** Taffy caches each node’s result keyed by its available space. Dirty bits climb from a changed node toward the root and stop at a layout boundary, so typing in a fixed-size text field relays out the field and nothing else. After layout, squark-layout writes absolute border-box rects into the fragment tree, then runs the ResizeObserver and IntersectionObserver checks against the new rects.

**Replaced elements.** `<img>` takes its intrinsic size from the asset loader, and its `load` event fires once the size is known. Inline `<svg>` lays out as a replaced box; its contents are geometry for paint, not layout boxes. Form controls are inline boxes with intrinsic sizes from the UA stylesheet and squark-text.

## §8 Paint and GPU

Paint records display items. Every item points at nodes in four property trees: transform, clip, effect and scroll. Because scroll offsets, sticky offsets and compositor animations are values in those trees, the render thread can change them every frame without asking the document thread to repaint.

Render thread work happens in order for each frame, then one submission runs five GPU passes in dependency order. Vello runs before the tile passes because its coverage textures are inputs to them.

The shaders are in `crates/squark-gpu/shaders/`. One shader, `static.wgsl`, draws every item kind in the table below except effect groups, and picks the kind per instance. `effects.wgsl` runs effect groups. Both include `text_coverage.wgsl`, which matches Chromium’s text antialiasing. `hello.wgsl` serves only the `squark-hello` demo.

| Display item | From | GPU path |
| --- | --- | --- |
| Rect, rounded rect | `background-color`, `border-radius` | Instanced, coverage from a rounded-rect SDF with per-corner radii. Opaque rects go in the depth-tested opaque pass. |
| Border | `border-*` | SDF ring with per-side width and color. Dashed and dotted use arc length along the ring. |
| Box shadow | `box-shadow`, outset and inset | Closed-form approximation of a blurred rounded rect, so no blur pass. Spread folds into the rect. |
| Gradient | `linear-`, `radial-`, `conic-gradient()` | Stops baked into one row of the gradient ramp texture, interpolated in the color space CSS specifies. |
| Image | `<img>`, `background-image` | Atlas for images up to 512 px, own texture above that, mipmapped when drawn smaller. `object-fit` is resolved in paint. |
| Text run | text fragments | R8 mask atlas for regular glyphs, RGBA8 atlas for color glyphs. Glyph positions snap to device pixels along the baseline. |
| Path | inline SVG, `clip-path: path()`, Canvas 2D | Vello compute renders coverage into a texture, then the item shader samples it. Canvas 2D commands run in squark-canvas, also on Vello. GLES targets lack compute and use Vello’s hybrid CPU/GPU renderer. |
| Clip | `overflow`, rounded overflow, `clip-path` | Scissor for axis-aligned rects; an SDF test inside the item shader for one rounded rect; a mask texture otherwise. |
| Effect group | `opacity` over overlapping content, `filter`, `mix-blend-mode`, `backdrop-filter` | Offscreen surface, filter passes (separable Gaussian, color matrix), composited with the blend mode. Opacity on a single item folds into its color and allocates nothing. |
| Transform | `transform`, scroll and sticky offsets | A transform tree node. Instances carry a node index into a storage buffer of 4×4 matrices. |

**Batching.** Items are in stacking order. Adjacent items share a draw when they use the same pipeline and bind group. An item may join an earlier batch out of order if its bounds overlap nothing drawn in between, which keeps text and boxes from splitting into hundreds of draws. Opaque items skip that check: they draw front to back with a depth test, so hidden pixels cost a depth comparison instead of a fragment shader.

**Tile caching.** Layers are cut into 512 × 512 device-pixel tiles kept as textures. A frame re-renders only tiles that intersect damage. Scrolling a static list moves tiles and renders only the strip entering the viewport.

**Color and antialiasing.** Blending happens in sRGB-encoded space, with `Rgba8Unorm` and `Bgra8Unorm` targets rather than the `*Srgb` formats, because browsers blend that way and designs are tuned against it. Edges get analytic coverage from SDFs, so no MSAA is needed.

**Default budgets.** Glyph atlases start as one 2048² R8 page and one 2048² RGBA8 page and grow by pages. The image atlas is 4096² RGBA8. Tiles, atlases and images share a 256 MiB texture budget and are evicted least recently used.

## §9 Input and events

Input arrives on the main thread, gets a target from the hit test, and is dispatched by squark-dom on the document thread with real capture and bubble phases. React sees the event exactly as it would in Chrome: on the root container, with the target node it stored a fiber on.

Consider a click on a span inside a button, on a 2× display. Release produces pointerup, mouseup and click along the same path. The click target is the nearest common ancestor of the press and release targets.

- **Pointer and mouse.** One press produces `pointerdown` then `mousedown`. Release produces `pointerup`, `mouseup` and `click`, per UI Events. `pointermove` is coalesced to one dispatch per frame, and `getCoalescedEvents()` returns the rest. `setPointerCapture` pins the target in the hit tester.

- **Scrolling.** Paint marks the regions covered by non-passive `wheel` and `touchmove` listeners. Outside them, the main thread sends the gesture straight to the compositor, which scrolls at the next vsync without JavaScript. Inside them, the event goes to JavaScript first and scrolling waits until `preventDefault()` is decided.

- **Keyboard and focus.** Keys go to the focused element, or to `body`. Tab order follows `tabindex` and document order, computed in squark-dom. `:focus-visible` follows the browser heuristic: keyboard focus shows it, pointer focus on buttons does not.

- **Text input and IME.** A focused `<input>` or `<textarea>` has native editing state in squark-text. Typing produces `beforeinput`, the value change, then `input`. IME composition produces `compositionstart`, `compositionupdate` and `compositionend`, and the preedit text is drawn underlined. React’s `onChange` listens to `input`, and its value tracker sees the new value through the prototype `value` accessor. When a controlled input writes `value` back, SET_VALUE updates the editing state and keeps the caret if the text did not change.

- **Hover and cursor.** Each hit test updates the hover chain. The change becomes element state in the mirror and restyles `:hover` rules. The hit element’s computed `cursor` goes to winit.

- **Form controls.** Buttons, checkboxes, radios, ranges, text fields and selects are drawn by Strange Quark from the UA stylesheet, so `appearance: none` and Tailwind’s preflight reset behave as in browsers. An open `<select>` is a top-layer popup in the same window on desktop and the platform picker on mobile.

- **Assistive technology (planned).** A screen reader’s activate action arrives through AccessKit as a click on the node. Set value goes through the same path as typing.

## §10 Threads and memory

| Thread | Runs | Owns | Talks to others through |
| --- | --- | --- | --- |
| Main | winit event loop, input routing, hit testing, IME, clipboard, AccessKit adapter (planned) | Window handles, input state | Task queue to the document thread; scroll channel to the render thread |
| Document | JS engine, squark-dom, journal, mirror, style, layout, paint | JS heap, node arena, stylesheets, computed values, fragment tree | Frame mailbox to the render thread; rayon scopes and channels to the pools |
| Render | Compositor, GPU renderer, present | Layer tree, scroll offsets, compositor animations, every GPU resource | Hit-test data to the main thread; scroll offsets and animation-finished events to the document thread |
| Worker pool | Parallel restyle, image decode, glyph rasterization, CPU path fallback | Nothing between tasks | Results as resource updates |
| IO pool | fetch, WebSocket, EventSource, file reads (tokio, 2 threads) | Sockets, file handles | Task queue to the document thread |

**Frame handoff.** The mailbox holds one frame. A newer commit replaces one the render thread has not taken yet. Resource updates (new glyphs, decoded images, freed textures) travel in a separate ordered queue, so dropping a frame never drops an upload or frees a texture early.

**Why three threads.** macOS and iOS require the event loop on the main thread. Synchronous layout reads must be answered on the thread that asks, so style and layout live with JavaScript. Present and scrolling must not stall when JavaScript runs long, so they get their own thread.

## §11 Web APIs and libraries

squark-dom implements what React and popular libraries actually call, in tiers. Feature detection should be truthful: `CSS.supports()` answers from the real support table, and touch APIs exist only on touch devices.

| Area | TIER 1 · v1 | TIER 2 | NOT PLANNED |
| --- | --- | --- | --- |
| DOM | Node, Element, Text, Comment, DocumentFragment, Document, selectors API, `contains`, `compareDocumentPosition`, MutationObserver, `innerHTML` through html5ever | Shadow DOM, custom elements, `moveBefore`, Range | `document.write`, XPath, iframes |
| Events | EventTarget with capture, `passive`, `once`, `signal`; Pointer, Mouse, Keyboard, Focus, Input, Composition, Wheel, Touch, Clipboard, Custom events | Drag and drop |  |
| CSSOM | `element.style`, `CSSStyleSheet`, `document.styleSheets`, `getComputedStyle`, `CSS.supports`, `matchMedia` | `adoptedStyleSheets`, Typed OM |  |
| Geometry | Rects, offset, client and scroll metrics, `scrollTo`, `scrollIntoView`, ResizeObserver, IntersectionObserver, `elementFromPoint` |  |  |
| Timing | `setTimeout`, `setInterval`, `queueMicrotask`, `MessageChannel`, rAF, `performance.now` | Web Workers, Web Animations API, `requestIdleCallback`, `performance.mark` and `measure` |  |
| Network, storage | `fetch`, Headers, Request, Response, AbortController, WebSocket, EventSource, URL, TextEncoder, file-backed `localStorage`, IndexedDB, `crypto.getRandomValues`, `crypto.randomUUID`, `crypto.subtle` (SHA digests and AES-GCM) |  | Service workers, cookies, CORS |
| Graphics, media | `<img>`, inline SVG (shapes, paths, text, gradients, `clipPath`), Canvas 2D on Vello | `<video>` and `<audio>` through platform decoders, `navigator.gpu` over wgpu | WebGL |
| Forms, editing | Text, number, password, checkbox, radio and range inputs; `textarea`, `select`, `button`, `label`, form `submit` events | `contenteditable`, Selection |  |
| Navigation | History API (in memory), `location`, `<a href>` routed to the app or the system browser |  | Multiple documents, cross-origin navigation |

### Library compatibility

| Library | What it touches | Status |
| --- | --- | --- |
| Redux, Zustand, Jotai, TanStack Query, SWR | Plain JS, `fetch`, focus and visibility events | TIER 1 |
| React Router, TanStack Router | History API, `popstate`, `location` | TIER 1 Deep links arrive as a launch argument. |
| Tailwind CSS v4, CSS Modules | Compiled CSS | TIER 1 Container-query utilities need tier 2. |
| emotion, styled-components | `<style>` elements, `insertRule` | TIER 1 |
| Radix UI, Floating UI, shadcn/ui | Portals, focus scopes, `getBoundingClientRect`, ResizeObserver, pointer events on `document` | TIER 1 |
| React Hook Form, Formik | Form elements, `input` events, refs | TIER 1 |
| TanStack Virtual, react-window | `scrollTop`, `scroll` events, ResizeObserver | TIER 1 Scroll reads can lag the compositor by a frame. |
| Recharts, visx | SVG elements, `getBBox`, text measurement | TIER 1 |
| Motion (Framer Motion) | rAF and style writes; `element.animate()` for hardware-accelerated values | TIER 1 for rAF animations; WAAPI is tier 2. |
| Chart.js, Konva | Canvas 2D | TIER 1 Canvas 2D is built, as Excalidraw uses it; these libraries are not checked yet. |
| Lexical, Slate, ProseMirror, TipTap | `contenteditable`, Selection, Range, `beforeinput` | LATER The hardest gap. Needs a full editing host. |
| three.js, react-three-fiber | WebGL | NO Use `<GpuView>` (§13, planned), or the WebGPU renderer once `navigator.gpu` lands. |

## §12 Toolchain

`squark build` runs the app’s own build, compiles its `index.html` into `document.bin`, packs the output into a `.sqpak` archive and appends it to the prebuilt `squark-runtime` binary. On macOS it writes an app bundle instead. An app without native modules compiles no Rust. An app with native modules (§13) gets its own runtime: `squark build` generates a small binary crate that passes the app's modules to `squark_host::runtime_main`, builds it from a Strange Quark source checkout with the dist profile, and appends the pack to that. Debug builds of the runtime warn about every CSS property layout or paint would ignore (§16).

The `squark` CLI has five commands: `pack` packs a built directory directly, `inspect` dumps a pack, `dev` runs the live-reload loop, `build` builds and packs an app as above, and `screenshot` renders an HTML file, directory or pack headlessly to PNG.

### Dev loop: `squark dev`

- Starts Vite with `@strange-quark/vite-plugin`, which only `squark dev` uses, and the native runtime. The runtime loads modules from the dev server over HTTP, so edits need no rebuild of Rust.

- Vite’s HMR client connects over the WebSocket squark-dom provides. React Fast Refresh is the stock `react-refresh` runtime.

- Planned: React DevTools runs standalone and connects through `react-devtools-core`.

- Planned: the V8 inspector exposes the Chrome DevTools Protocol for breakpoints and CPU profiles.

- `squark inspect --dump-pack <file>` validates and dumps a `.sqpak`, a packed binary or a `document.bin`, with `--json` for JSON. Overlays for layout boxes, paint flashing, layer borders, overdraw and the atlases are planned (§16).

```tsx
// src/main.tsx: the same file Vite would run in a browser
import { createRoot } from 'react-dom/client';
import App from './App';
import './index.css';

createRoot(document.getElementById('root')!).render(<App />);
```

```tsx
// squark.config.ts: every key is optional
import { defineConfig } from 'strange-quark';

export default defineConfig({
  name: 'acme-console',                 // binary name; defaults to package.json's name
  window: { title: 'Acme Console', width: 1280, height: 800 },
  build: {
    command: 'npm run build',           // the app's own build; detected if omitted
    distDir: 'dist',                    // its output; dist/ or build/ if omitted
    entryHtml: 'index.html',
    env: { VITE_API_URL: 'https://api.acme.dev' },
  },
  network: { allow: ['api.acme.dev', 'wss://events.acme.dev'] },
  assets: [{ path: '/fonts/Inter.woff2', file: 'fonts/Inter.woff2' }],  // extra files at exact pack paths
  native: {                             // §13: the app's own Rust, linked into its runtime
    crate: 'native',                    // a library whose modules() returns its squark_native::Modules
    modules: ['acme'],                  // the modules JavaScript may call
    squarkSource: '../strange-quark',   // the checkout the app runtime builds from
  },
  preludes: [{ file: './boot.js', globals: { BOOT: { theme: 'dark' } } }],  // classic scripts run before the app's
});
```

## §13 Native escape hatches

Native modules are built (`squark-native`, below). `<GpuView>` and `@strange-quark/native` are planned.

**`<GpuView>`** reserves a box in layout like any element. Each frame it is visible, the render thread calls a registered Rust renderer to draw into a texture sized to the box in device pixels. The texture is composited as a layer in paint order, so DOM content can overlay it, clip it and round its corners, and pointer events over it still go through squark-dom first.

**Native modules** are Rust functions marked `#[squark_native::export]`, grouped in `squark_native::Module`s, and linked into the app's own runtime, so they cost no extra process or IPC. JavaScript calls them by name through the private `Symbol.for("strange-quark.native")` API (`invoke(module, command, args)`, `listen(event, callback)`), and module code pushes events with `app.emit(event, payload)`. Values cross as JSON. A module runs only if `squark.config.ts` lists it under `native.modules`. Sync commands run on a blocking thread and async ones on the runtime squark-net already owns. Generated TypeScript types are planned.

Strange Quark's own API knows no other shell. Compatibility with one is an optional module an app opts into. For Tauri 2 there are two halves: the `squark-tauri` crate gives a Tauri backend's commands the API they were written against (an app builds its backend with `squark_tauri` named `tauri`), and the `@strange-quark/tauri` package supplies `window.__TAURI_INTERNALS__` over native modules through squark.config's generic `preludes`, so the frontend runs without source changes. Apps that do not use them never install or link them.

Linking the app's Rust into its runtime was chosen over a sidecar process, a JavaScript backend on host APIs, and Wasm modules because it adds the least to the binary and to memory: the runtime's std, tokio, TLS and serde are shared, and there is no second process, isolate or serialization hop.

```tsx
// TSX: a GPU island inside ordinary layout
import { GpuView } from '@strange-quark/native';

export function Workbench() {
  return (
    <div className="grid h-full grid-cols-[280px_1fr]">
      <Sidebar />
      <GpuView renderer="scene3d" className="rounded-xl" />
    </div>
  );
}
```

```rust
// Rust: runs on the render thread, once per frame the view is visible
#[squark::gpu_view("scene3d")]
fn render(ctx: &mut GpuViewContext) {
    // ctx.target is a wgpu::TextureView sized to the box in device pixels
    let mut pass = ctx.begin_pass(wgpu::Color::BLACK);
    ctx.state::<Scene>().draw(&mut pass, ctx.size, ctx.frame_time);
}

#[squark_native::export]                  // JS: invoke("files", "read_text", { path })
async fn read_text(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(path).await.map_err(|error| error.to_string())
}

pub fn modules() -> Vec<squark_native::Module> {
    vec![squark_native::Module::new("files").commands(squark_native::handlers![read_text])]
}
```

## §14 Platform targets

| Target | Windowing | wgpu backend | JS engine | Accessibility | Notes |
| --- | --- | --- | --- | --- | --- |
| Windows 10+ x64, arm64 | winit (Win32) | D3D12, Vulkan fallback | V8 | UI Automation | System fonts through DirectWrite via fontique |
| macOS 12+ | winit (AppKit) | Metal | V8 | NSAccessibility | Event loop pinned to the main thread |
| Linux, Wayland and X11 | winit | Vulkan, GLES fallback | V8 | AT-SPI | IME through text-input-v3 or IBus |
| Android 10+ | winit (android-activity) | Vulkan, GLES fallback | V8 | AccessKit Android | Text input needs an InputConnection bridge (§18) |
| iOS 16+ | winit (UIKit) | Metal | V8 jitless or QuickJS-ng | UIAccessibility, adapter to build | No JIT for third-party code |
| Embedded Linux | squark-host DRM/KMS backend, no compositor | GLES or Vulkan | QuickJS-ng | None | Kiosks, appliances, in-vehicle displays |
| Web (wasm32) | Canvas element | WebGPU | The browser’s | None | For tests and demos. squark-dom runs in the page’s own JS. |

Only the three desktop targets are built, and each uses one wgpu backend: Direct3D 12 on Windows, Metal on macOS and Vulkan on Linux, without the fallbacks. No target has an accessibility adapter yet. Android, iOS, embedded Linux and the web target are planned.

## §15 Repository layout

```tsx
strange-quark/
├─ crates/
│  ├─ squark-host/         window, event loop, input routing, frame clock (winit); the squark-runtime binary
│  ├─ squark-js/           V8 embedding, module loader, event loop, __squark bindings
│  ├─ squark-journal/      record validation and encoding, string table, atoms
│  ├─ squark-mirror/       node arena, dirty bits, Stylo DOM traits
│  ├─ squark-style/        Stylo integration, UA stylesheet, CSS warnings
│  ├─ squark-layout/       Taffy integration, inline formatting, positioning
│  ├─ squark-text/         fonts, shaping, glyph raster, text editing
│  ├─ squark-paint/        display list, property trees, hit testing, damage
│  ├─ squark-compositor/   frame mailbox, scroll offsets, compositor animations
│  ├─ squark-gpu/          wgpu renderer, atlases, tiles, Vello paths, shaders/*.wgsl
│  ├─ squark-canvas/       Canvas 2D command playback on the GPU (Vello)
│  ├─ squark-assets/       asset pack (.sqpak) reader, initial document, image and SVG decode
│  ├─ squark-net/          fetch, WebSocket, EventSource, host allowlist (tokio)
│  ├─ squark-types/        types shared across crates, one module per seam
│  └─ squark-cli/          squark pack | inspect | dev | build | screenshot
├─ packages/
│  ├─ strange-quark/               npm entry: squark CLI shim, config types
│  ├─ @strange-quark/dom/          JS DOM, CSSOM, events, Web API facades, journal encoder
│  └─ @strange-quark/vite-plugin/  Vite dev-server plugin and launcher for squark dev
├─ npm/            per-platform runtime packages (cli-<os>-<cpu>)
├─ apps/           test apps: counter, todomvc, shadcn, virtual-list, excalidraw
├─ vendor/         Excalidraw (submodule) and patched Stylo
├─ scripts/        check.sh, check.mjs and other tooling
└─ tests/
   ├─ wpt/         imported Web Platform Tests subsets (DOM, Canvas 2D)
   ├─ reftests/    HTML/CSS cases diffed against the pinned Chromium
   ├─ react-dom/   react-dom's own test suite on squark-dom
   ├─ compat/      app interaction scripts, DOM and pixel diffs against Chromium
   └─ excalidraw/  Excalidraw end-to-end flow and scenes
```

Direct mode, accessibility, native modules and the devtools client (§5, §13, §16) have no crate or package yet.

## §16 Testing and observability

### Testing

- **react-dom’s own test suite** runs against squark-dom, adapted from its jsdom environment (`tests/react-dom`). It is pinned to React 19.0.0; the aim is to run it on every React release.

- **Web Platform Tests**: pinned subsets for DOM and Canvas 2D (`tests/wpt`). UI Events, Pointer Events, ResizeObserver, IntersectionObserver and CSS layout and painting are to follow.

- **Reftests** render each HTML/CSS case in Strange Quark and the pinned headless Chromium and compare pixels with a per-test tolerance for antialiasing.

- **Compat suite**: the apps in `apps/` (a counter, TodoMVC, shadcn/ui and a virtual list) run in both engines with the same interaction scripts, comparing DOM state and screenshots. Excalidraw has its own end-to-end flow in `tests/excalidraw`. Storybooks of Radix, Mantine, MUI and TanStack Table are planned.

- **GPU determinism**: CI (`.github/workflows/ci.yml`) renders on Mesa’s lavapipe software adapter for stable pixels. Its macOS and Windows jobs build and run the tests without a GPU. Runs on real Metal, Vulkan and Direct3D 12 hardware are made by hand and recorded in `tests/results/`; a nightly hardware job is planned.

### Observability

- **Frame timeline**: `SQUARK_FRAME_TIMELINE` records per-frame samples, and debug builds write a puffin trace when `SQUARK_PUFFIN_TRACE` is set. One span per step in §3, and Tracy in profiling builds, are planned.

- **Forced-layout detector** (planned): a read that forces layout during React’s render phase, or inside an interleaved write-read loop, logs both JS stack traces.

- **Overlays** (planned): layout boxes, paint flashing from damage rects, layer and tile borders, overdraw heatmap, atlas viewer.

- **Unsupported CSS warnings** in debug builds, with the property, value, stylesheet, line and column.

- **Journal tap** (planned): record the journal for a session and replay it in Rust without JavaScript, to reproduce rendering bugs.

## §17 Design decisions

| Decision | Chosen | Rejected | Why |
| --- | --- | --- | --- |
| Where the DOM lives | JavaScript, with a journal to Rust | A native DOM behind bindings; a custom reconciler only | Structural reads are property lookups. react-dom’s expandos and prototype accessors work unchanged. Writes batch for free. |
| JS engine | V8 by default, QuickJS-ng behind the same trait (planned) | JavaScriptCore, Hermes | V8 has startup snapshots, fast native calls and a mature inspector. QuickJS-ng is small and runs without a JIT. |
| Style | Stylo, with the mirror implementing its DOM traits | A hand-written cascade | Parsing, cascade, invalidation, style sharing and parallel traversal already exist, and Blitz has shown Stylo works outside Servo. |
| Layout | Taffy for block, flex and grid; Parley for inline | Yoga; a hand-written engine | Taffy covers the three main algorithms and has a measure hook. Parley handles rich text with inline boxes. |
| 2D rendering | Instanced primitive renderer, with Vello for paths | Vello for everything; Skia; WebRender | UI frames are mostly boxes and text, which are cheap as instanced quads. Vello excels at paths but needs compute shaders. Skia adds a C++ build and a GPU backend outside wgpu. |
| Threads | Main, document and render, plus pools | One thread; a separate layout thread | Meets the main-thread rule on Apple platforms, answers layout reads on the asking thread, and isolates scrolling from long JS tasks. |
| Blending | sRGB-encoded space | Linear-light blending | Matches browser output, so designs look as they do in Chrome. |
| Accessibility | AccessKit (planned) | Per-platform bridges | One tree format, existing adapters for Windows, macOS, Linux and Android. |
| Native app code | Rust native modules linked into an app runtime that `squark build` compiles (§13) | A sidecar process; a JavaScript backend on host APIs; Wasm modules; dynamically loaded libraries | Smallest binary and memory growth: no duplicated runtime libraries, second process or extra isolate. Dynamic loading needs `unsafe` and duplicates std per library. |

## §18 Risks and open questions

**Inline layout** (high)

CSS inline formatting (`vertical-align`, inline-block baselines, bidi around inline boxes, line-height rules) is the largest single piece of layout work, and Taffy does not cover it. Build on Parley’s inline boxes and reftest against Chromium from the first week.

**The CSS long tail** (high)

Stylo computes nearly everything, so an unimplemented property fails silently in layout or paint. Dev warnings with source locations, and a support table generated from the code, keep the gaps visible.

**Text editing parity** (high)

Inputs that feel native need selection handles, IME, autocorrect, dictation, spellcheck and accessibility on six platforms. winit’s IME support is uneven, especially on Android. Plan a per-platform text-input bridge behind one trait.

**Binary size** (medium)

V8 adds tens of megabytes. The planned QuickJS-ng backend keeps small targets small. ICU data that Parley already provides can be stripped from V8.

**react-dom drift** (medium)

New React releases can start using new DOM APIs. Run react-dom’s suite on every release and track experimental flags that use new APIs, such as `moveBefore`.

**Feature sniffing** (medium)

Libraries branch on `navigator.userAgent`, `CSS.supports` and `'ontouchstart' in window`. Ship a stable UA token, a truthful `CSS.supports`, and touch properties only on touch devices.

**Security model** (low)

App code is trusted and bundled, as in Electron without remote content, so no site isolation is needed. Network hosts are allowlisted in config (`network.allow`), and native modules in `native.modules` (§13); a native module is trusted app code with the user's full permissions, like a Tauri command.

### Open questions

- One JS realm per window, or one realm with a document per window?

- Answered: Canvas 2D moved into v1, ahead of `contenteditable`, because Excalidraw draws with it.

- Should `navigator.gpu` be exposed to JavaScript over wgpu, as Deno does, so three.js’s WebGPU renderer can run inside a box?

- Where is the size threshold for parallel restyle, given that rayon scheduling costs more than restyling a small subtree?

## §19 Prior art

| Project | What Strange Quark takes from it |
| --- | --- |
| Blitz (Dioxus Labs) | An HTML and CSS renderer built on Stylo, Taffy, Parley and Vello without a browser. Strange Quark’s native half uses the same crates. The difference is the JavaScript side and running react-dom unmodified. |
| React Native (Fabric, JSI) | A React renderer over a native shadow tree with a thin binding layer. Strange Quark keeps the DOM API instead of native primitives. |
| WebRender (Servo, Firefox) | Display lists, the spatial tree, picture caching, and front-to-back opaque batching. |
| Chromium’s compositor | Property trees, async scrolling, and the split between main-thread and compositor-thread work. |
| jsdom, happy-dom, linkedom | DOM implementations in JavaScript. squark-dom differs by streaming a journal to a real style and layout engine. |
| Deno | Embedding V8 from Rust with rusty_v8, and exposing WebGPU to JavaScript over wgpu. |

Strange Quark architecture map · design proposal rev 0.1 · 25 November 2026. Frame budgets and memory defaults are design targets, not measurements.

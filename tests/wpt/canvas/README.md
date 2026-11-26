<!-- SPDX-License-Identifier: MIT -->
# Canvas 2D Web Platform Tests (`tests/wpt/canvas/`)

This is the Canvas 2D subset for the Canvas 2D methods
Excalidraw uses, imported from upstream WPT at the same pinned
revision as the DOM subset:
[f4d3f2eba09697ebcb2f338b8f2d709a3ce85217](https://github.com/web-platform-tests/wpt/tree/f4d3f2eba09697ebcb2f338b8f2d709a3ce85217).

The files under `vendor/` are **unmodified upstream WPT files**. The
[BSD 3-Clause licence](../vendor/LICENSE.md) applies to them.

## What is imported

`cases.json` lists the 66 `.html` entry points, relative to
`vendor/html/canvas/`. They cover the Canvas 2D categories Excalidraw uses, with
the upstream file names at this revision:

| Category | Directory | What it exercises |
| --- | --- | --- |
| Paths | `element/path-objects` | `moveTo`, `lineTo`, `rect`, `roundRect` radii, `arc`, `ellipse`, quadratic/bezier curves, `closePath`, `fill`/`stroke`, winding, `clip` |
| Rectangles | `element/drawing-rectangles-to-the-canvas` | `clearRect`, `fillRect`, `strokeRect`, transforms and clips |
| Line styles | `element/line-styles` | `lineWidth` (including scaled), `lineCap`, `lineJoin`, defaults |
| Transforms | `element/transformations` | `rotate`, `scale`, `translate`, `setTransform`, `transform`, order |
| Paint styles | `element/fill-and-stroke-styles` | CSS colour parsing, defaults, invalid strings |
| Images | `element/drawing-images-to-the-canvas` | `drawImage` with 3, 5 and 9 arguments, canvas sources, alpha, transforms |
| Text | `element/text` | `font` parsing, `textAlign`, `textBaseline`, `fillText` `maxWidth`, `measureText` spaces |
| Pixels | `element/pixel-manipulation` | `createImageData`, `getImageData` order/range, `putImageData`, dirty rects |
| State | `element/the-canvas-state` | `save`/`restore` of stack, transform, fill style, clip and bitmap |

Upstream helpers are vendored too: `html/canvas/resources/canvas-tests.js` and
`.css`, `resources/testharnessreport.js`, `fonts/CanvasTest.ttf` (for the text
pixel tests) and `images/red.png` + `images/green.png` (for `drawImage`).
`resources/testharness.js` is shared with the DOM subset.

`expected-failures.json` is empty: every imported file passes in headless
Chromium with the harness flags below. Any future expected failure goes there
as `file` or `file::subtest` → missing feature, so a test report can name it.

## Running

```bash
# Chromium reference (also part of `npm test` in @strange-quark/wpt)
node tests/wpt/canvas/run.mjs

# Filter one file, emit JSON, or write a report elsewhere
node tests/wpt/canvas/run.mjs --filter 2d.path.rect --json
node tests/wpt/canvas/run.mjs --out tests/results/wpt-canvas/summary.json

# Native engine (reports `blocked` when squark and squark-runtime are not built)
node tests/wpt/canvas/run.mjs --engine native
```

The runner serves `/resources/`, `/html/canvas/`, `/fonts/` and `/images/` from
the vendor trees, launches headless Chromium (`SQUARK_CHROMIUM_PATH` or
`/usr/bin/chromium`) with the same deterministic flags as the reftests, and
collects results by appending a collector to `testharnessreport.js` — the first
script after `testharness.js`, so nothing completes before it is registered.

The browser is the pinned reference Chromium from `scripts/chromium-path.mjs`
(`SQUARK_CHROMIUM_PATH`, then Playwright's pinned Chrome for Testing, then the
system browser). CI installs and prints that exact build; older system browsers
fail `element/text/2d.text.font.parse.basic.html`, which the pinned WPT revision
expects to pass.

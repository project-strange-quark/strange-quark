<!-- SPDX-License-Identifier: MIT -->
# Reftest Suite R1 (Static HTML/CSS)

155 static HTML/CSS cases (zero JavaScript, zero external network URLs) rendered
against headless Chromium (`/usr/bin/chromium`) with bundled fonts under
`_assets/fonts/` and deterministic images under `_assets/images/`.

| Directory | Count | Coverage |
| --- | ---: | --- |
| `01-inline/` | 21 | Inline-layout cases (with `data-geom` paired geometry baselines in `tests/results/r1/geometry.json`: whitespace across spans, preserved line breaks, nested `inline-block` baselines, `vertical-align`, mixed bidi + inline image, fractional widths, loaded WOFF2 line-break change, flex/grid intrinsic inline sizing, clipping/ellipsis, missing-image fallback) plus font weights, line-height, text-align, spacing, word-break, decorations, transforms, shadows, `font-variation-settings` on a variable font |
| `02-block/` | 18 | Block stacking, `box-sizing`, sibling & parent-child margin collapsing, auto centering, negative margins, percentages, `min/max-*`, `display: none/contents`, `aspect-ratio`, `flow-root`, logical margin/padding, `calc/min/max/clamp`, viewport/`rem` units, lists, basic table, `contain` (size, inline-size, layout, paint, strict, content) |
| `03-flex/` | 16 | Row/column/reverse, `flex-grow`, `flex-shrink`/`flex-basis`, `flex-wrap`/`align-content`, `justify-content`, `align-items`/`align-self`, `gap`, `order`, auto margins, `inline-flex`, nested dashboard, `min-width: 0` truncation, `aspect-ratio`, baseline alignment |
| `04-grid/` | 14 | Fixed tracks, `fr`, `repeat()`/`minmax()`, `row-gap`/`column-gap`, line placement & spans, `grid-template-areas`, `grid-auto-flow` (`column`/`dense`), implicit `grid-auto-rows`, alignment, `place-self`/auto margins, `inline-grid`, `%`/`calc()`, z-index overlap, `auto-fill` |
| `05-position/` | 15 | `relative`, `absolute` containing blocks/`inset`/auto-center/`%`/shrink-to-fit, `fixed` viewport & transformed CB, `sticky`, `z-index`, `isolation: isolate`, stacking contexts (`opacity`, `transform`, negative `z-index`, `auto` vs `0`), absolute inside flex/grid |
| `06-backgrounds/` | 12 | Solid hex/rgb/hsl/alpha, `background-clip`, linear/radial/conic/repeating gradients, multi-layer backgrounds, `repeat`/`position`/`size`, `oklch()`/`color-mix()`, `currentColor` |
| `07-borders/` | 12 | Per-side borders & CSS triangles, `dashed`/`dotted`/`double`, `border-radius` (uniform, per-corner, elliptical, pill, thick borders, bg clip), `outline`/`outline-offset`, `box-shadow` (outset, spread rings, inset, layered elevation, rounded) |
| `08-effects/` | 15 | Group vs per-child `opacity`, `visibility`, `filter` (`invert` + `hue-rotate`, `blur`, `brightness`, `contrast`, `drop-shadow`, `grayscale`, `sepia`), `mix-blend-mode`, 2D/3D `transform` & `transform-origin`, `overflow: hidden` rect & rounded, `clip-path`, layered alpha compositing, `backdrop-filter` |
| `09-content/` | 16 | Inline SVG shapes/paths/dashes/`viewBox`, `data:image/png;base64` & `data:image/svg+xml` URLs, `<img>` intrinsic sizing & `object-fit`, `@font-face` WOFF2 + TTF, `@font-face` faces matched by weight, style and `local()` and split by `unicode-range`, `::before`/`::after`, `:nth-child`/`:not`/`:is`/`:where`/`:has`, `@media`, `<link rel="stylesheet">` cascade, `appearance: none` controls, root full-height layout |
| `10-excalidraw/` | 16 | Excalidraw v0.18.1 CSS patterns found by auditing its stylesheets: `:root`/`.theme--dark` CSS vars, dark-mode `invert(93%) hue-rotate(180deg)`, `.visually-hidden` legacy `clip: rect(1px,1px,1px,1px)`, `env(safe-area-inset-*)`, `.Island` floating toolbar, swatch grid, data-PNG checkerboard, data-SVG select chevron, modal backdrop, tooltip `::before` arrow, `[data-state]`/`[aria-selected]` selectors, `[dir="rtl"]`, SVG `stroke: currentColor`, Assistant WOFF2 UI typography, clipped sidebar, full workspace shell |

## Running

```bash
# Regenerate Chromium expected PNGs and inline geometry baselines
timeout 180 node tests/reftests/src/cli.mjs --suite r1 --mode chromium

# Compare squark screenshot against expected PNGs
timeout 300 node tests/reftests/src/cli.mjs --suite r1 --mode compare
```

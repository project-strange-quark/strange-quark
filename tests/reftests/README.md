<!-- SPDX-License-Identifier: MIT -->
# Reftests

Each case is an HTML page rendered by the pinned Chromium
(`scripts/chromium-path.mjs`) and by Strange Quark, and the two images are
compared. Chromium's renders are committed under `tests/results/<suite>/expected/`.
The suites, named by their directories under `cases/`:

| Suite | Cases | What it covers |
| --- | --- | --- |
| `smoke` | 1 | Proves the harness: one page rendered by Chromium and compared with its committed baseline. |
| `r1` | 155 | **R1**: static HTML and CSS, no JavaScript, with bundled fonts. See [cases/r1/README.md](cases/r1/README.md). |
| `r2-forms` | 5 | **R2**: form controls, text fields, buttons, selects and focus. |
| `r3-canvas` | 15 | **R3**: Canvas 2D as Excalidraw uses it: rough.js shapes, freehand strokes, text, dashes, transforms, clips, `drawImage` and the dark-mode filter. |
| `layout` | 1 | A layout regression case. |
| `motion` | 1 | A CSS animation case. |

`node tests/reftests/src/cli.mjs --suite <suite> --mode compare` renders a
suite with Strange Quark and compares it with the baselines; `--filter`,
`--squark-bin`, `--runtime` and `--gpu-adapter` narrow or point the run. The
default mode, `--mode chromium`, re-renders the baselines into
`tests/results/` instead, so run it only to update them, on Linux.

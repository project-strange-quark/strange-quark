<!-- SPDX-License-Identifier: MIT -->
# Bundled R1 Test Fonts

These fonts are checked into `tests/reftests/cases/r1/_assets/fonts/` so both
headless Chromium and `strange-quark` shape identical font bytes during R1 reftests.

| File | Family in `r1.css` | Source & Version | Licence |
| --- | --- | --- | --- |
| `LiberationSans-Regular.woff2` | `TestSans` (400 normal) | Liberation Fonts v2.1.5 | SIL OFL-1.1, [`OFL-Liberation.txt`](OFL-Liberation.txt) |
| `LiberationSans-Bold.woff2` | `TestSans` (700 normal) | Liberation Fonts v2.1.5 | SIL OFL-1.1, [`OFL-Liberation.txt`](OFL-Liberation.txt) |
| `LiberationSans-Italic.woff2` | `TestSans` (400 italic) | Liberation Fonts v2.1.5 | SIL OFL-1.1, [`OFL-Liberation.txt`](OFL-Liberation.txt) |
| `LiberationMono-Regular.woff2` | `TestMono` (400 normal) | Liberation Fonts v2.1.5 | SIL OFL-1.1, [`OFL-Liberation.txt`](OFL-Liberation.txt) |
| `LiberationMono-Regular.ttf` | `TestMonoTTF` (400 normal) | Liberation Fonts v2.1.5 | SIL OFL-1.1, [`OFL-Liberation.txt`](OFL-Liberation.txt) |
| `NotoNaskhArabic-Regular.woff2` | `TestArabic` (400 normal) | Noto Naskh Arabic 2.021 | SIL OFL-1.1, [`OFL-NotoNaskhArabic.txt`](OFL-NotoNaskhArabic.txt) |
| `Assistant-Regular.woff2` | `TestAssistant` (400 normal) | Assistant 3.000 (via Excalidraw v0.18.1) | SIL OFL-1.1, [`OFL-Assistant.txt`](OFL-Assistant.txt) |

The fonts are unmodified apart from lossless WOFF2 compression. Each
family's licence file holds its copyright notice and the full SIL Open Font
License 1.1, which must travel with the fonts.

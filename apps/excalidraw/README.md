<!-- SPDX-License-Identifier: MIT -->
# Excalidraw build config

`squark.config.ts` here is passed explicitly:

```bash
squark build vendor/excalidraw/excalidraw-app \
  --config apps/excalidraw/squark.config.ts
```

It declares the four upstream Assistant font files at the paths Excalidraw's
emitted stylesheet requests (`/assets/Assistant-<weight>.woff2`). The app's own
Vite build emits only `Assistant-Regular.woff2` at the site root, and its
`@font-face` prefers a CDN URL that a native pack cannot reach, so without this
the UI falls back to a system font and screenshots drift from Chromium.
`vendor/excalidraw` is never modified: the files come from its font sources.

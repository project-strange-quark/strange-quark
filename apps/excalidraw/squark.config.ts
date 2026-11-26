// SPDX-License-Identifier: MIT
// Build config for the vendored Excalidraw app.
//
// Excalidraw's stylesheet lists four Assistant weights. Its Vite build emits
// only the regular weight into dist/ (from public/), and its @font-face lists
// the CDN URL first and `./Assistant-<weight>.woff2` second. Chromium falls
// back to the CDN; a native pack cannot, so the UI font disappears and text
// metrics drift. Declare the upstream font files at the exact paths the
// stylesheet asks for (`/assets/Assistant-*.woff2`, relative to the emitted
// CSS). `assets[].file` resolves against this file's directory.
import { defineConfig } from "strange-quark/config";

const assistant =
  "../../vendor/excalidraw/packages/excalidraw/fonts/Assistant";

export default defineConfig({
  name: "excalidraw",
  assets: ["Bold", "Medium", "Regular", "SemiBold"].map((weight) => ({
    path: `/assets/Assistant-${weight}.woff2`,
    file: `${assistant}/Assistant-${weight}.woff2`,
  })),
});

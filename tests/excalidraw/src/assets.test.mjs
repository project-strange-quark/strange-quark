// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import test from "node:test";

import { cssRelativeAssets, missingCssAssets } from "./assets.mjs";

test("cssRelativeAssets resolves relative and root URLs against the sheet", () => {
  const css = `
    @font-face { src: url("https://cdn.example/x.woff2"), url(./Assistant-Bold.woff2); }
    .a { background: url('/images/bg.png'); }
    .b { background: url(data:image/png;base64,AAAA); }
    .c { filter: url(#f); }
  `;
  assert.deepEqual(cssRelativeAssets(css, "/assets/index.css").sort(), [
    "/assets/Assistant-Bold.woff2",
    "/images/bg.png",
  ]);
});

test("missingCssAssets reports only paths the pack cannot serve", () => {
  const css = `src: url(./Assistant-Bold.woff2), url(./Assistant-Regular.woff2)`;
  const packPaths = ["/assets/Assistant-Regular.woff2", "/index.html"];
  assert.deepEqual(missingCssAssets(css, "/assets/index.css", packPaths), [
    "/assets/Assistant-Bold.woff2",
  ]);
});

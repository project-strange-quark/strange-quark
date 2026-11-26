// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import test from "node:test";

import {
  createDevManifest,
  extractEntryModule,
  hasReactPlugin,
  hmrUrl,
  scriptAttributes,
} from "../dist/manifest.js";

test("extractEntryModule picks the app module, skipping vite and refresh", () => {
  const html = `<!DOCTYPE html><html><head>
    <script type="module">import RefreshRuntime from "/@react-refresh"</script>
    <script type="module" src="/@vite/client"></script>
  </head><body>
    <div id="root"></div>
    <script defer type="module" src="/src/main.tsx"></script>
  </body></html>`;
  assert.equal(extractEntryModule(html), "/src/main.tsx");
});

test("extractEntryModule ignores classic scripts and missing srcs", () => {
  const html = `<script src="/legacy.js"></script><script type="module"></script>`;
  assert.equal(extractEntryModule(html), null);
});

test("scriptAttributes reads quoted and bare attributes", () => {
  const [attrs] = scriptAttributes(`<script type='module' src=/src/app.tsx defer>`);
  assert.deepEqual(attrs, { type: "module", src: "/src/app.tsx", defer: "" });
});

test("hmrUrl mirrors the origin and honours base and explicit paths", () => {
  assert.equal(hmrUrl("http://127.0.0.1:5173", null), "ws://127.0.0.1:5173/");
  assert.equal(hmrUrl("https://dev.example.test", null, "/app/"), "wss://dev.example.test/app/");
  assert.equal(
    hmrUrl("http://127.0.0.1:5173", { path: "/custom-hmr" }),
    "ws://127.0.0.1:5173/custom-hmr",
  );
});

test("createDevManifest pins the hand-off shape squark dev copies into the pack", () => {
  const manifest = createDevManifest({
    appName: "counter",
    origin: "http://127.0.0.1:5173/",
    entry: "/src/main.tsx",
    root: "/repo/apps/counter",
    vite: "6.4.6",
  });
  assert.deepEqual(manifest, {
    version: 1,
    appName: "counter",
    origin: "http://127.0.0.1:5173",
    hmr: "ws://127.0.0.1:5173/",
    entry: "/src/main.tsx",
    client: "/@vite/client",
    refresh: "/@react-refresh",
    root: "/repo/apps/counter",
    vite: "6.4.6",
  });
});

test("hasReactPlugin detects nested and plugin-react style names", () => {
  assert.equal(hasReactPlugin([[{ name: "vite:react-babel" }], false]), true);
  assert.equal(hasReactPlugin([{ name: "strange-quark:vite-plugin" }]), false);
  assert.equal(hasReactPlugin(undefined), false);
});

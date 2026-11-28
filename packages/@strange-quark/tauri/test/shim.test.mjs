// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { NATIVE_API, fixtureTransport, installTauri, installTauriFromConfig, nativeTransport } from "@strange-quark/tauri";

// @tauri-apps/api reads `window.__TAURI_INTERNALS__` at call time, so one
// shared global that each test reinstalls is enough.
globalThis.window = globalThis;
const core = await import("@tauri-apps/api/core");
const event = await import("@tauri-apps/api/event");
const webview = await import("@tauri-apps/api/webview");

function fresh(transport, host = {}) {
  delete globalThis.__TAURI_INTERNALS__;
  delete globalThis.__TAURI_EVENT_PLUGIN_INTERNALS__;
  installTauri(globalThis, { transport, host });
}

test("invoke reaches the transport with camelCase args and keeps rejections as values", async () => {
  const calls = [];
  fresh(fixtureTransport({
    fs_read_file: args => {
      calls.push(args);
      return "contents";
    },
    fail: () => {
      throw "No such file";
    },
  }));
  assert.equal(await core.invoke("fs_read_file", { path: "/a.txt", sessionId: "s" }), "contents");
  assert.deepEqual(calls, [{ path: "/a.txt", sessionId: "s" }]);
  assert.equal(await core.invoke("unknown_command"), undefined);
  await assert.rejects(core.invoke("fail"), error => error === "No such file");
  assert.equal(core.isTauri(), true);
});

test("listen receives backend events in Tauri's event shape until unlisten", async () => {
  const transport = fixtureTransport();
  fresh(transport);
  const seen = [];
  const unlisten = await event.listen("pty-output", e => seen.push(e));
  transport.emit("pty-output", { session_id: "a", data: "$ " });
  transport.emit("other", 1);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].event, "pty-output");
  assert.equal(typeof seen[0].id, "number");
  assert.deepEqual(seen[0].payload, { session_id: "a", data: "$ " });
  await unlisten();
  transport.emit("pty-output", { session_id: "a", data: "more" });
  assert.equal(seen.length, 1);
});

test("once unregisters after the first event; emit stays in the page", async () => {
  const transport = fixtureTransport();
  fresh(transport);
  const seen = [];
  await event.once("ready", e => seen.push(e.payload));
  await event.emit("ready", 1);
  await event.emit("ready", 2);
  transport.emit("ready", 3);
  assert.deepEqual(seen, [1]);
});

test("plugin commands map to the platform or reject plainly", async () => {
  const written = [];
  globalThis.navigator ??= {};
  Object.defineProperty(globalThis.navigator, "clipboard", {
    configurable: true,
    value: { readText: async () => "from clipboard", writeText: async text => void written.push(text) },
  });
  const zooms = [];
  const opened = [];
  fresh(fixtureTransport(), { setZoom: value => zooms.push(value), openExternal: url => opened.push(url) });
  // An app polyfill may replace navigator.clipboard with plugin calls; the shim
  // must keep using the clipboard it captured, not recurse into the polyfill.
  globalThis.navigator.clipboard.writeText = text => core.invoke("plugin:clipboard-manager|write_text", { text });
  assert.equal(await core.invoke("plugin:clipboard-manager|read_text"), "from clipboard");
  await core.invoke("plugin:clipboard-manager|write_text", { text: "copied" });
  assert.deepEqual(written, ["copied"]);
  await core.invoke("plugin:opener|open_url", { url: "https://example.com" });
  assert.deepEqual(opened, ["https://example.com"]);
  await webview.getCurrentWebview().setZoom(1.25);
  assert.deepEqual(zooms, [1.25]);
  await assert.rejects(core.invoke("plugin:updater|check", {}), error => error === "plugin:updater|check is not available in Strange Quark");
  assert.equal(webview.getCurrentWebview().label, "main");
});

test("convertFileSrc follows Tauri's per-platform URL form", () => {
  const unix = { navigator: { platform: "Linux x86_64" } };
  installTauri(unix, { transport: fixtureTransport() });
  assert.equal(
    unix.__TAURI_INTERNALS__.convertFileSrc("/home/me/a b.png", "app-plugin"),
    "app-plugin://localhost/%2Fhome%2Fme%2Fa%20b.png",
  );
  assert.equal(unix.__TAURI_INTERNALS__.convertFileSrc("/x"), "asset://localhost/%2Fx");

  const windows = { navigator: { platform: "Win32" } };
  installTauri(windows, { transport: fixtureTransport() });
  assert.equal(
    windows.__TAURI_INTERNALS__.convertFileSrc("C:\\Users\\me\\a b.png", "app-plugin"),
    "http://app-plugin.localhost/C%3A%5CUsers%5Cme%5Ca%20b.png",
  );
  assert.equal(windows.__TAURI_INTERNALS__.convertFileSrc("C:\\x"), "http://asset.localhost/C%3A%5Cx");
});

test("nativeTransport routes through the private native API and fails clearly without it", async () => {
  const sent = [];
  const listeners = new Map();
  const host = {
    [NATIVE_API]: {
      modules: ["demo"],
      invoke: async (module, command, args) => {
        sent.push([module, command, args]);
        return 42;
      },
      listen: (name, callback) => {
        listeners.set(name, callback);
        return () => listeners.delete(name);
      },
      window: {},
    },
  };
  const transport = nativeTransport(host, "demo");
  assert.equal(await transport.invoke("git_status", { path: "/r" }), 42);
  assert.deepEqual(sent, [["demo", "git_status", { path: "/r" }]]);
  const off = transport.listen("fs-changed", () => {});
  assert.ok(listeners.has("fs-changed"));
  off();
  assert.ok(!listeners.has("fs-changed"));
  await assert.rejects(nativeTransport({}, "demo").invoke("x", {}), /no native modules/);
});

test("the classic script installs from the injected config", async () => {
  const script = fs.readFileSync(new URL("../dist/tauri.classic.js", import.meta.url), "utf8");
  const context = vm.createContext({
    __STRANGE_QUARK_TAURI__: { fixture: true },
    __STRANGE_QUARK_TAURI_FIXTURE__: { settings_load: { theme: "dark" } },
    Symbol,
    Promise,
    Uint32Array,
    Math,
  });
  vm.runInContext(script, context);
  assert.equal(context.isTauri, true);
  assert.deepEqual(await context.__TAURI_INTERNALS__.invoke("settings_load", {}), { theme: "dark" });
  assert.equal(typeof context.__STRANGE_QUARK_TAURI_EMIT__, "function");
  assert.equal(typeof installTauriFromConfig, "function");
});

test("classic.mjs strips exports and normalizes CRLF sources from Windows tsc output", async () => {
  const { toClassicScript } = await import("../scripts/classic.mjs");
  const built = fs.readFileSync(new URL("../dist/index.js", import.meta.url), "utf8");
  assert.equal(built.includes("\r"), false, "tsconfig newLine: lf keeps dist/index.js LF-only");
  const crlfSource = `${built.replace(/\r?\n/g, "\r\n")}export {};\r\n`;
  const script = toClassicScript(crlfSource);
  assert.equal(script.includes("\r"), false, "classic output should use LF line endings");
  assert.doesNotMatch(script, /^\s*export\b/m);
  const context = vm.createContext({
    __STRANGE_QUARK_TAURI__: { fixture: true },
    __STRANGE_QUARK_TAURI_FIXTURE__: { ping: "pong" },
    Symbol,
    Promise,
    Uint32Array,
    Math,
  });
  vm.runInContext(script, context);
  assert.equal(await context.__TAURI_INTERNALS__.invoke("ping", {}), "pong");
});

test("the config helper fills in squark.config's generic preludes", async () => {
  const { tauri, SHIM_FILE } = await import("@strange-quark/tauri/config");
  assert.deepEqual(tauri({ module: "demo" }), {
    preludes: [{ file: SHIM_FILE, globals: { __STRANGE_QUARK_TAURI__: { module: "demo" } } }],
  });
  assert.deepEqual(tauri({ fixture: "fixtures/replies.js" }).preludes, [
    { file: "./fixtures/replies.js" },
    { file: SHIM_FILE, globals: { __STRANGE_QUARK_TAURI__: { fixture: true } } },
  ]);
  for (const unchanged of [
    "./fixtures/replies.js",
    "../fixtures/replies.js",
    ".\\fixtures\\replies.js",
    "/abs/replies.js",
    "C:\\repo\\replies.js",
    "d:/repo/replies.js",
    "\\\\server\\share\\replies.js",
    "\\\\?\\C:\\repo\\replies.js",
  ]) {
    assert.equal(tauri({ fixture: unchanged }).preludes[0].file, unchanged);
  }
});

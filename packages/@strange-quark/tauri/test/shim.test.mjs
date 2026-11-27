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
  assert.equal(await core.invoke("fs_read_file", { path: "/a.jl", sessionId: "s" }), "contents");
  assert.deepEqual(calls, [{ path: "/a.jl", sessionId: "s" }]);
  assert.equal(await core.invoke("unknown_command"), undefined);
  await assert.rejects(core.invoke("fail"), error => error === "No such file");
  assert.equal(core.isTauri(), true);
});

test("listen receives backend events in Tauri's event shape until unlisten", async () => {
  const transport = fixtureTransport();
  fresh(transport);
  const seen = [];
  const unlisten = await event.listen("pty-output", e => seen.push(e));
  transport.emit("pty-output", { session_id: "a", data: "julia> " });
  transport.emit("other", 1);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].event, "pty-output");
  assert.equal(typeof seen[0].id, "number");
  assert.deepEqual(seen[0].payload, { session_id: "a", data: "julia> " });
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
  await core.invoke("plugin:opener|open_url", { url: "https://julialang.org" });
  assert.deepEqual(opened, ["https://julialang.org"]);
  await webview.getCurrentWebview().setZoom(1.25);
  assert.deepEqual(zooms, [1.25]);
  await assert.rejects(core.invoke("plugin:updater|check", {}), error => error === "plugin:updater|check is not available in Strange Quark");
  assert.equal(webview.getCurrentWebview().label, "main");
});

test("convertFileSrc follows Tauri's per-platform URL form", () => {
  fresh(fixtureTransport());
  assert.equal(core.convertFileSrc("/home/me/a b.png", "julide-plugin"), "julide-plugin://localhost/%2Fhome%2Fme%2Fa%20b.png");
  assert.equal(core.convertFileSrc("/x"), "asset://localhost/%2Fx");
});

test("nativeTransport routes through the private native API and fails clearly without it", async () => {
  const sent = [];
  const listeners = new Map();
  const host = {
    [NATIVE_API]: {
      modules: ["julide"],
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
  const transport = nativeTransport(host, "julide");
  assert.equal(await transport.invoke("git_status", { path: "/r" }), 42);
  assert.deepEqual(sent, [["julide", "git_status", { path: "/r" }]]);
  const off = transport.listen("fs-changed", () => {});
  assert.ok(listeners.has("fs-changed"));
  off();
  assert.ok(!listeners.has("fs-changed"));
  await assert.rejects(nativeTransport({}, "julide").invoke("x", {}), /no native modules/);
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

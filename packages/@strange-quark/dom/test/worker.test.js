// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import { installDOM, installTiming, installWorkers } from "@strange-quark/dom";
import { installWorkerGlobal } from "../dist/worker-global.js";

function parseURL(input, base) {
  let url;
  try { url = new URL(input, base); } catch { return undefined; }
  const { href, protocol, origin, username, password, hostname, host, port, pathname, search, hash } = url;
  return { href, protocol, origin, username, password, hostname, host, port, pathname, search, hash };
}

const timingHost = {
  now: () => 1,
  armTimer() {},
  queueMicrotask: callback => queueMicrotask(callback),
  postTask: callback => setImmediate(callback),
  cloneMessage: value => structuredClone(value),
};

test("Worker is undefined unless the host can start workers", () => {
  const global = {};
  const document = installDOM(global);
  assert.equal(installWorkers(global, document, { parseURL }), undefined);
  assert.equal("Worker" in global, false);
});

test("Worker validates its arguments and delivers worker packets", () => {
  const global = {};
  const document = installDOM(global, undefined, { url: "http://app.test/index.html" });
  installTiming(global, timingHost);
  const started = [];
  const posted = [];
  const terminated = [];
  const logged = [];
  const dispatch = installWorkers(global, document, {
    parseURL,
    workerStart: (...args) => started.push(args),
    workerPost: (...args) => posted.push(args),
    workerTerminate: id => terminated.push(id),
    logError: message => logged.push(message),
  });
  const { Worker, MessageChannel } = global;
  const worker = new Worker("/worker.js", { name: "named" });
  const module = new Worker(new URL("blob:http://app.test/1234"), { type: "module" });
  assert.deepEqual(started, [[1, "http://app.test/worker.js", false, "named"], [2, "blob:http://app.test/1234", true, ""]]);
  assert.throws(() => new Worker("https://other.test/x.js"), { name: "SecurityError" });
  assert.throws(() => new Worker("http://[bad"), { name: "SyntaxError" });
  assert.throws(() => new Worker("/x.js", { type: "shared" }), TypeError);
  assert.doesNotThrow(() => new Worker("data:text/javascript,"));

  const buffer = new ArrayBuffer(4);
  worker.postMessage({ buffer }, [buffer]);
  worker.postMessage(2, { transfer: [] });
  assert.deepEqual(posted, [[1, { buffer }, [buffer]], [1, 2, []]]);
  assert.throws(() => worker.postMessage(1, [new Uint8Array(1)]), { name: "DataCloneError" });
  assert.throws(() => worker.postMessage(1, [buffer, buffer]), { name: "DataCloneError", message: /duplicate/ });
  assert.throws(() => worker.postMessage(1, [new MessageChannel().port1]), { name: "DataCloneError", message: /MessagePort/ });
  assert.throws(() => worker.postMessage(), TypeError);

  const seen = [];
  worker.onmessage = event => seen.push(["message", event.data, event.isTrusted]);
  worker.addEventListener("messageerror", event => seen.push(["messageerror", event.data]));
  worker.onerror = event => seen.push(["error", event.constructor.name, event.message ?? null, event.lineno ?? null]);
  global.onerror = (message, filename, lineno, colno, error) => seen.push(["window", message, filename, lineno, colno, error]);
  dispatch([1, "message", { a: 1 }]);
  dispatch([1, "messageerror"]);
  dispatch([1, "error", ["Uncaught Error: x", "http://app.test/worker.js", 3, 4]]);
  dispatch([1, "failed"]);
  module.onerror = event => event.preventDefault();
  dispatch([2, "error", ["Uncaught Error: y", "blob:http://app.test/1234", 1, 1]]);
  assert.deepEqual(seen, [
    ["message", { a: 1 }, true],
    ["messageerror", null],
    ["error", "ErrorEvent", "Uncaught Error: x", 3],
    ["window", "Uncaught Error: x", "http://app.test/worker.js", 3, 4, null],
    ["error", "Event", null, null],
  ]);
  assert.deepEqual(logged, ["Uncaught Error: x"]);

  worker.terminate();
  worker.terminate();
  assert.deepEqual(terminated, [1]);
  dispatch([1, "message", "late"]);
  dispatch([2, "closed"]);
  module.terminate();
  assert.deepEqual(terminated, [1]);
  assert.equal(seen.length, 5);
});

function workerRealm(options) {
  const context = createContext({});
  const global = runInContext("globalThis", context);
  const calls = { posted: [], reported: [], loaded: [], ran: [], closed: 0 };
  const scripts = { "http://app.test/lib.js": "var lib = 1;" };
  const host = {
    ...timingHost,
    parseURL,
    updateURL: () => { throw new Error("unused"); },
    utf8Encode: text => new TextEncoder().encode(text),
    utf8Decode: bytes => new TextDecoder().decode(bytes),
    fetchStart() {}, fetchCredit() {}, fetchCancel() {}, socketOpen() {}, socketSend() {}, socketClose() {},
    blobRegister: () => "blob:http://app.test/1", blobRevoke() {},
    base64Encode: () => "", base64Decode: () => null, decodeText: () => "",
    randomBytes: size => new Uint8Array(size), randomUUID: () => "00000000-0000-4000-8000-000000000000",
    cryptoDigest() {}, cryptoGenerateKey() {}, cryptoImportKey() {}, cryptoExportKey() {}, cryptoForgetKey() {}, cryptoAesGcm() {},
    wasmPending() {},
    os: "linux", arch: "aarch64", hardwareConcurrency: 6,
    workerPost: (message, transfer) => calls.posted.push([message, transfer]),
    workerClose: () => calls.closed++,
    workerReport: error => calls.reported.push(error),
    loadScript: url => { calls.loaded.push(url); return url in scripts ? [url, scripts[url]] : undefined; },
    runClassicScript: (source, url) => calls.ran.push([source, url]),
  };
  const callbacks = installWorkerGlobal(global, host, options);
  return { context, global, calls, callbacks };
}

test("a worker global has the worker APIs and none of the document's", () => {
  const { context, global } = workerRealm({ url: "http://app.test/w.js?x#y", name: "pool", module: false });
  const surface = runInContext(`JSON.stringify([
    self === globalThis, Object.prototype.toString.call(self), name, String(location), location.search, location.origin,
    typeof document, typeof window, typeof Worker, typeof localStorage, typeof HTMLElement,
    typeof postMessage, typeof importScripts, typeof close, typeof fetch, typeof WebSocket, typeof setTimeout,
    typeof requestAnimationFrame, typeof structuredClone, typeof MessageChannel, typeof TextEncoder, typeof crypto,
    typeof URL, typeof Blob, typeof FileReader, typeof reportError, typeof EventTarget, typeof ErrorEvent,
    navigator.platform, navigator.hardwareConcurrency, navigator.onLine, navigator.languages.join(),
    Object.getPrototypeOf(Object.getPrototypeOf(self)) === WorkerGlobalScope.prototype,
    self instanceof EventTarget,
  ])`, context);
  assert.deepEqual(JSON.parse(surface), [
    true, "[object DedicatedWorkerGlobalScope]", "pool", "http://app.test/w.js?x#y", "?x", "http://app.test",
    "undefined", "undefined", "undefined", "undefined", "undefined",
    "function", "function", "function", "function", "function", "function",
    "function", "function", "function", "function", "object",
    "function", "function", "function", "function", "function", "function",
    "Linux aarch64", 6, true, "en-US", true, true,
  ]);
  assert.throws(() => new global.DedicatedWorkerGlobalScope(), TypeError);
  assert.throws(() => new global.WorkerLocation(), TypeError);
});

test("postMessage, importScripts and close go to the host", () => {
  const { context, calls } = workerRealm({ url: "http://app.test/w.js", name: "", module: false });
  // squark-dom's modules and the script share a realm in a worker; here the
  // buffer comes from the modules' realm.
  context.buffer = new ArrayBuffer(1);
  runInContext(`
    postMessage({ buffer }, [buffer]);
    postMessage(1);
    importScripts("lib.js");
    globalThis.errors = [];
    for (const run of [() => postMessage(1, [{}]), () => postMessage(), () => importScripts("http://[x"), () => importScripts("/missing.js")]) {
      try { run(); } catch (error) { errors.push([error.name, error.code]); }
    }
    close();
  `, context);
  assert.equal(calls.posted.length, 2);
  assert.deepEqual(calls.posted[1], [1, []]);
  assert.deepEqual(calls.ran, [["var lib = 1;", "http://app.test/lib.js"]]);
  assert.deepEqual(calls.loaded, ["http://app.test/lib.js", "http://app.test/missing.js"]);
  assert.deepEqual(JSON.parse(runInContext("JSON.stringify(errors)", context)), [["DataCloneError", 25], ["TypeError", null], ["SyntaxError", 12], ["NetworkError", 19]]);
  assert.equal(calls.closed, 1);

  const module = workerRealm({ url: "http://app.test/m.js", name: "", module: true });
  assert.throws(() => runInContext("importScripts('lib.js')", module.context), { name: "TypeError", message: /Module scripts/ });
  assert.deepEqual(module.calls.loaded, []);
});

test("worker globals dispatch messages and errors like a DedicatedWorkerGlobalScope", () => {
  const { context, calls, callbacks } = workerRealm({ url: "http://app.test/w.js", name: "", module: false });
  runInContext(`
    globalThis.seen = [];
    onmessage = event => seen.push(["message", event.data, event.isTrusted]);
    onmessageerror = event => seen.push(["messageerror", event.data]);
  `, context);
  callbacks.message({ x: 1 });
  callbacks.messageError();
  const error = new Error("boom");
  assert.equal(callbacks.report(error, "http://app.test/w.js", 1, 2, "Uncaught Error: boom"), true);
  runInContext(`
    onerror = (message, filename, lineno, colno, error) => { seen.push(["onerror", message, lineno, colno, error.message]); return true; };
  `, context);
  assert.equal(callbacks.report(error, "http://app.test/w.js", 1, 2, "Uncaught Error: boom"), false);
  callbacks.message(2);
  assert.deepEqual(JSON.parse(runInContext("JSON.stringify(seen)", context)), [
    ["message", { x: 1 }, true],
    ["messageerror", null],
    ["onerror", "Uncaught Error: boom", 1, 2, "boom"],
    ["message", 2, true],
  ]);
  // reportError hands the value to the host, which locates and reports it.
  runInContext(`reportError(new TypeError("reported"))`, context);
  assert.equal(calls.reported.length, 1);
  assert.equal(calls.reported[0].message, "reported");
  assert.throws(() => runInContext("reportError()", context), TypeError);
});

#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Probe the Web APIs and CSS features exposed by a Strange Quark runtime.

import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const opt = (name, fallback = null) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const squarkBinaryArg = opt("--squark-binary");
const squarkCliArg = opt("--squark");
const outArg = opt("--out");
const timeoutMs = Number(opt("--timeout", "30000"));

if (flag("--help")) {
  console.log("usage: node probe.mjs (--squark-binary <runtime-or-app> | --squark <squark-cli>) --out <file.json> [--timeout <ms>]");
  process.exit(0);
}
if (!outArg || Boolean(squarkBinaryArg) === Boolean(squarkCliArg) || !Number.isFinite(timeoutMs) || timeoutMs < 1) {
  console.error("usage: node probe.mjs (--squark-binary <runtime-or-app> | --squark <squark-cli>) --out <file.json> [--timeout <ms>]");
  process.exit(2);
}

const outPath = path.resolve(outArg);
const cliPath = squarkCliArg && path.resolve(squarkCliArg);
let runtimePath = squarkBinaryArg && path.resolve(squarkBinaryArg);

function run(command, args, { cwd, env, timeout = timeoutMs } = {}) {
  return new Promise((resolve, reject) => {
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeout);
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(new Error(`could not start ${command}: ${error.message}`));
    });
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      if (timedOut) {
        reject(new Error(`${command} timed out after ${timeout} ms\n${stderr.slice(-2000)}`));
      } else {
        resolve({ code, signal, stdout, stderr });
      }
    });
  });
}

function isExecutableFile(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

function findRuntime(cli) {
  const candidates = [
    process.env.SQUARK_RUNTIME_BIN,
    path.join(path.dirname(cli), "squark-runtime"),
    path.resolve(path.dirname(cli), "../dist/squark-runtime"),
  ].filter(Boolean).map((candidate) => path.resolve(candidate));
  return candidates.find(isExecutableFile) ?? null;
}

function findSquarkPacker() {
  const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
  const pathCandidates = (process.env.PATH ?? "").split(path.delimiter).map((dir) => path.join(dir, "squark"));
  const candidates = [
    ...pathCandidates,
    path.join(projectRoot, "target/debug/squark"),
    path.join(projectRoot, "target/dist/squark"),
  ];
  return candidates.find(isExecutableFile) ?? null;
}

const PAGE = `<!doctype html>
<html><head><meta charset="utf-8"><title>Strange Quark capability probe</title></head>
<body><main id="probe">Strange Quark capability probe</main>
<input id="file-probe" type="file"><canvas id="canvas-probe" width="2" height="2"></canvas>
</body></html>`;

const PROBE_SCRIPT = `(() => {
  const apis = {};
  const pending = [];
  const add = (name, get, smoke) => {
    let value;
    try {
      value = get();
      const present = value === null ? "object" : typeof value;
      const result = { present, works: value === undefined || value === null ? false : null };
      apis[name] = result;
      if (value !== undefined && value !== null && smoke) {
        try {
          const outcome = smoke(value);
          if (outcome && typeof outcome.then === "function") {
            pending.push(name);
            outcome.then((works) => { result.works = Boolean(works); }, (error) => {
              result.works = false;
              result.error = String(error?.message ?? error);
            }).finally(() => pending.splice(pending.indexOf(name), 1));
          } else {
            result.works = Boolean(outcome);
          }
        } catch (error) {
          result.works = false;
          result.error = String(error?.message ?? error);
        }
      }
    } catch (error) {
      apis[name] = { present: "throws", works: false, error: String(error?.message ?? error) };
    }
  };
  const own = (object, property) => object == null ? undefined : object[property];
  const canvas = () => document.createElement("canvas");

  add("WebGL", () => window.WebGLRenderingContext ?? window.WebGL2RenderingContext, (value) => !!canvas().getContext("webgl"));
  add("WebGL2", () => window.WebGL2RenderingContext, (value) => !!canvas().getContext("webgl2"));
  add("WebGPU", () => own(navigator, "gpu"), (value) => typeof value.requestAdapter === "function");
  add("contentEditable", () => own(document.createElement("div"), "contentEditable"), (value) => typeof value === "string");
  add("Selection", () => window.Selection, (Ctor) => {
    const selection = document.getSelection();
    if (!(selection instanceof Ctor) || typeof selection.addRange !== "function") return false;
    const range = document.createRange();
    range.selectNodeContents(document.body);
    selection.removeAllRanges();
    selection.addRange(range);
    const works = selection.rangeCount === 1 && selection.anchorNode === document.body;
    selection.removeAllRanges();
    return works;
  });
  add("Range", () => window.Range, (value) => {
    const range = document.createRange();
    range.selectNodeContents(document.body);
    return range instanceof value && typeof range.getClientRects === "function" && range.toString === value.prototype.toString;
  });
  add("document.caretRangeFromPoint", () => own(document, "caretRangeFromPoint"), (value) => typeof value === "function");
  add("document.caretPositionFromPoint", () => own(document, "caretPositionFromPoint"), (value) => typeof value === "function");
  add("document.implementation.createHTMLDocument", () => own(own(document, "implementation"), "createHTMLDocument"), () => {
    const doc = document.implementation.createHTMLDocument("probe");
    doc.body.innerHTML = "<p id=x>a</p>";
    return doc.getElementById("x")?.textContent === "a";
  });
  add("document.createNodeIterator", () => own(document, "createNodeIterator"), () => {
    const iterator = document.createNodeIterator(document.body, NodeFilter.SHOW_ELEMENT);
    return iterator.nextNode() === document.body;
  });
  add("requestIdleCallback", () => window.requestIdleCallback, (value) => new Promise((resolve) => {
    value((deadline) => resolve(typeof deadline.timeRemaining() === "number"), { timeout: 2000 });
  }));
  add("performance.mark", () => own(performance, "mark"), () => {
    performance.mark("squark-probe");
    const found = performance.getEntriesByName("squark-probe").length > 0;
    performance.clearMarks("squark-probe");
    return found;
  });
  add("document.hasFocus", () => own(document, "hasFocus"), (value) => typeof value.call(document) === "boolean");
  add("strange-quark native modules", () => own(window, Symbol.for("strange-quark.native")), (api) => Array.isArray(api.modules) && api.modules.length > 0);
  add("HTMLIFrameElement", () => window.HTMLIFrameElement, (Ctor) => new Promise((resolve) => {
    const frame = document.createElement("iframe");
    if (!(frame instanceof Ctor)) return resolve(false);
    frame.srcdoc = "<p>x</p>";
    const done = (works) => { frame.remove(); resolve(works); };
    frame.onload = () => done(frame.contentDocument?.body?.textContent === "x");
    setTimeout(() => done(false), 2000);
    document.body.appendChild(frame);
  }));
  add("Worker", () => window.Worker, (Ctor) => new Promise((resolve, reject) => {
    const url = URL.createObjectURL(new Blob(["onmessage = (e) => postMessage(e.data + 1);"], { type: "text/javascript" }));
    const worker = new Ctor(url);
    worker.onmessage = (event) => { worker.terminate(); URL.revokeObjectURL(url); resolve(event.data === 42); };
    worker.onerror = () => { worker.terminate(); reject(new Error("worker failed")); };
    worker.postMessage(41);
  }));
  add("SharedWorker", () => window.SharedWorker);
  add("navigator.serviceWorker", () => own(navigator, "serviceWorker"));
  add("Element.attachShadow", () => Element.prototype.attachShadow, () => {
    // A slot projects the host's child, and the shadow tree's style is scoped.
    const host = document.createElement("div");
    host.innerHTML = "<span>light</span>";
    document.body.appendChild(host);
    const root = host.attachShadow({ mode: "open" });
    root.innerHTML = "<style>:host { display: block; width: 40px } span { color: red }</style><slot></slot>";
    const slotted = root.querySelector("slot").assignedNodes()[0];
    const ok = host.shadowRoot === root && slotted === host.firstChild &&
      host.getBoundingClientRect().width === 40 && getComputedStyle(slotted).color !== "rgb(255, 0, 0)";
    host.remove();
    return ok;
  });
  add("customElements", () => window.customElements, (registry) => {
    const name = "squark-probe-" + Math.random().toString(36).slice(2);
    let connected = false;
    registry.define(name, class extends HTMLElement { connectedCallback() { connected = true; } });
    const element = document.createElement(name);
    document.body.appendChild(element);
    element.remove();
    return connected && element instanceof HTMLElement && registry.get(name) === element.constructor;
  });
  add("Element.animate", () => Element.prototype.animate, (value) => typeof value === "function");
  add("document.startViewTransition", () => own(document, "startViewTransition"));
  add("HTMLElement.draggable", () => own(document.createElement("div"), "draggable"), (value) => typeof value === "boolean");
  add("DragEvent", () => window.DragEvent, (Ctor) => new Ctor("dragstart") instanceof Ctor);
  add("HTMLMediaElement", () => window.HTMLMediaElement);
  add("HTMLVideoElement", () => window.HTMLVideoElement);
  add("HTMLAudioElement", () => window.HTMLAudioElement);
  add("Audio", () => window.Audio, (Ctor) => new Ctor() instanceof window.HTMLAudioElement);
  add("RTCPeerConnection", () => window.RTCPeerConnection);
  add("MediaRecorder", () => window.MediaRecorder);
  add("navigator.mediaDevices.getUserMedia", () => own(own(navigator, "mediaDevices"), "getUserMedia"));
  add("navigator.mediaDevices.getDisplayMedia", () => own(own(navigator, "mediaDevices"), "getDisplayMedia"));
  add("Notification", () => window.Notification);
  add("navigator.geolocation", () => own(navigator, "geolocation"));
  add("document.cookie", () => document.cookie, (value) => typeof value === "string");
  add("showOpenFilePicker", () => window.showOpenFilePicker);
  add("showSaveFilePicker", () => window.showSaveFilePicker);
  add("showDirectoryPicker", () => window.showDirectoryPicker);
  add("window.open", () => window.open);
  add("window.print", () => window.print);

  add("File", () => window.File, (Ctor) => new Ctor(["x"], "probe.txt").size === 1);
  add("FileReader", () => window.FileReader, (Ctor) => new Promise((resolve, reject) => {
    const reader = new Ctor();
    reader.onload = () => resolve(reader.result === "x");
    reader.onerror = () => reject(reader.error ?? new Error("FileReader failed"));
    reader.readAsText(new Blob(["x"]));
  }));
  add("Blob", () => window.Blob, (Ctor) => new Ctor(["x"]).size === 1);
  add("URL.createObjectURL", () => own(URL, "createObjectURL"), (create) => {
    const url = create.call(URL, new Blob(["x"]));
    const works = typeof url === "string" && url.startsWith("blob:");
    URL.revokeObjectURL(url);
    return works;
  });
  add("indexedDB", () => window.indexedDB, (db) => new Promise((resolve, reject) => {
    const name = "squark-probe-" + Date.now() + "-" + Math.random();
    let request;
    try {
      request = db.open(name, 1);
      request.onupgradeneeded = () => request.result.createObjectStore("probe");
      request.onerror = () => reject(request.error ?? new Error("IndexedDB open failed"));
      request.onsuccess = () => {
        const database = request.result;
        const transaction = database.transaction("probe", "readwrite");
        transaction.objectStore("probe").put("x", "key");
        transaction.onerror = () => reject(transaction.error ?? new Error("IndexedDB write failed"));
        transaction.oncomplete = () => {
          const read = database.transaction("probe").objectStore("probe").get("key");
          read.onerror = () => reject(read.error ?? new Error("IndexedDB read failed"));
          read.onsuccess = () => {
            const works = read.result === "x";
            database.close();
            db.deleteDatabase(name);
            resolve(works);
          };
        };
      };
    } catch (error) {
      reject(error);
    }
  }));
  add("crypto.subtle", () => own(window.crypto, "subtle"), (subtle) => subtle.digest("SHA-256", new Uint8Array([120])).then((digest) => digest.byteLength === 32));
  add("navigator.clipboard", () => own(navigator, "clipboard"));
  add("structuredClone", () => window.structuredClone, (clone) => clone({ a: 1 }).a === 1);
  add("FontFace", () => window.FontFace, (Ctor) => new Ctor("SquarkProbe", "local('sans-serif')").family === "SquarkProbe");
  add("document.fonts", () => document.fonts, (fonts) => typeof fonts.check === "function");
  add("ResizeObserver", () => window.ResizeObserver, (Ctor) => {
    const observer = new Ctor(() => {});
    observer.observe(document.documentElement);
    observer.disconnect();
    return true;
  });
  add("IntersectionObserver", () => window.IntersectionObserver, (Ctor) => {
    const observer = new Ctor(() => {});
    observer.observe(document.documentElement);
    observer.disconnect();
    return true;
  });
  add("MutationObserver", () => window.MutationObserver, (Ctor) => {
    const observer = new Ctor(() => {});
    observer.observe(document.documentElement, { childList: true });
    observer.disconnect();
    return true;
  });
  add("matchMedia", () => window.matchMedia, (match) => typeof match.call(window, "(min-width: 1px)").matches === "boolean");
  add("fetch", () => window.fetch, (fetcher) => fetcher.call(window, "data:text/plain,squark-probe").then((response) => response.text()).then((text) => text === "squark-probe"));
  add("WebSocket", () => window.WebSocket);
  add("localStorage", () => window.localStorage, (storage) => {
    const key = "__squark_probe__";
    storage.setItem(key, "x");
    const works = storage.getItem(key) === "x";
    storage.removeItem(key);
    return works;
  });
  add("sessionStorage", () => window.sessionStorage, (storage) => {
    const key = "__squark_probe__";
    storage.setItem(key, "x");
    const works = storage.getItem(key) === "x";
    storage.removeItem(key);
    return works;
  });
  add("createImageBitmap", () => window.createImageBitmap, (create) => create.call(window, canvas()).then((bitmap) => {
    const works = bitmap.width > 0 && bitmap.height > 0;
    bitmap.close?.();
    return works;
  }));
  add("Path2D", () => window.Path2D, (Ctor) => {
    const path = new Ctor();
    path.moveTo(0, 0);
    path.lineTo(1, 1);
    return true;
  });
  add("CanvasRenderingContext2D", () => window.CanvasRenderingContext2D ?? canvas().getContext("2d"), (value) => {
    const context = canvas().getContext("2d");
    if (!context) return false;
    context.fillRect(0, 0, 1, 1);
    return context.getImageData(0, 0, 1, 1).data.length === 4;
  });
  add("WebAssembly", () => window.WebAssembly, (wasm) => wasm.validate(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0])));
  add("OffscreenCanvas", () => window.OffscreenCanvas);
  add("DataTransfer", () => window.DataTransfer, (Ctor) => new Ctor().items != null);
  add("fileInput", () => document.createElement("input"), (input) => {
    input.type = "file";
    let changeFired = false;
    input.addEventListener("change", () => { changeFired = true; });
    const transfer = new DataTransfer();
    transfer.items.add(new File(["x"], "probe.txt"));
    input.files = transfer.files;
    input.dispatchEvent(new Event("change", { bubbles: true }));
    return input.files !== null && input.files?.length === 1 && input.files[0].name === "probe.txt" && changeFired;
  });

  const css = {};
  const cssTests = {
    "css-container-queries": ["container-type", "inline-size"],
    "css-float": ["float", "left"],
    "css-subgrid": ["grid-template-columns", "subgrid"],
    "css-mask-border-image": [["mask-image", "url(#probe)"], ["border-image-source", "url(#probe)"]],
    "css-anchor": ["anchor-name", "--probe"],
    "css-view-transitions": ["view-transition-name", "none"],
    "css-scroll-timeline": ["animation-timeline", "auto"],
    "css-multicol": ["columns", "2"],
    "css-vertical-writing": ["writing-mode", "vertical-rl"],
    "css-table-layout": ["display", "table"],
  };
  const supports = own(window.CSS, "supports");
  add("CSS.supports", () => supports, (test) => test.call(window.CSS, "color", "red"));
  for (const [name, test] of Object.entries(cssTests)) {
    if (typeof supports !== "function") css[name] = "unknown";
    else {
      try {
        const tests = Array.isArray(test[0]) ? test : [test];
        css[name] = tests.some(([property, value]) => supports.call(window.CSS, property, value));
      }
      catch (error) { css[name] = { works: false, error: String(error?.message ?? error) }; }
    }
  }
  if (typeof supports !== "function") css["css-print"] = "unknown";
  else {
    try { css["css-print"] = Boolean(window.matchMedia && window.matchMedia("print").media === "print"); }
    catch (error) { css["css-print"] = { works: false, error: String(error?.message ?? error) }; }
  }

  globalThis.__squarkProbeResult = { apis, css, pending };
  return globalThis.__squarkProbeResult;
})()`;

class SquarkEngine {
  constructor(binary, input, tempDir) {
    this.binary = binary;
    this.input = input;
    this.tempDir = tempDir;
    this.pending = new Map();
    this.nextId = 1;
    this.buffer = "";
    this.output = "";
  }

  async start() {
    this.socketPath = path.join(this.tempDir, `squark-probe-${process.pid}.sock`);
    this.child = spawn(this.binary, [this.input, "--test-control", this.socketPath, "--headless", "--width", "800", "--height", "600"], {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, XDG_DATA_HOME: path.join(this.tempDir, "data") },
    });
    this.child.stdout.on("data", (chunk) => { this.output += chunk; });
    this.child.stderr.on("data", (chunk) => { this.output += chunk; });
    this.child.once("error", (error) => { this.spawnError = error; });
    this.socket = await this.connect();
    this.socket.on("data", (chunk) => this.onData(chunk));
    this.socket.on("error", (error) => this.rejectPending(error));
    await this.request("frame.waitIdle", { frames: 2 });
    const result = await this.eval(PROBE_SCRIPT);
    if (!result?.apis || !result?.css) throw new Error("the runtime returned an invalid probe result");
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await this.request("frame.waitIdle", { frames: 2 });
      const latest = await this.eval("globalThis.__squarkProbeResult");
      if (!latest?.apis || !latest?.css) throw new Error("the runtime lost its probe result");
      if (latest.pending.length === 0) return latest;
    }
    const latest = await this.eval("globalThis.__squarkProbeResult");
    for (const name of latest.pending) {
      const api = latest.apis[name];
      if (api) {
        api.works = false;
        api.error = "smoke test timed out";
      }
    }
    return latest;
  }

  connect() {
    const deadline = Date.now() + timeoutMs;
    return new Promise((resolve, reject) => {
      const attempt = () => {
        if (this.spawnError) return reject(new Error(`could not start runtime: ${this.spawnError.message}`));
        if (this.child.exitCode !== null) {
          return reject(new Error(`runtime exited (${this.child.exitCode}) before opening test control:\n${this.output.slice(-2000)}`));
        }
        const socket = net.createConnection({ path: this.socketPath });
        socket.once("connect", () => resolve(socket));
        socket.once("error", (error) => {
          socket.destroy();
          if (Date.now() >= deadline) {
            reject(new Error(`runtime did not open test-control socket ${this.socketPath}: ${error.message}\n${this.output.slice(-2000)}`));
          } else {
            setTimeout(attempt, 25);
          }
        });
      };
      attempt();
    });
  }

  onData(chunk) {
    this.buffer += chunk;
    let newline;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (!line) continue;
      let message;
      try { message = JSON.parse(line); }
      catch (error) { this.rejectPending(new Error(`invalid JSON-RPC response: ${error.message}`)); continue; }
      const pending = this.pending.get(message.id);
      if (!pending) continue;
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      else pending.resolve(message.result);
    }
  }

  rejectPending(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  request(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, { method, resolve, reject, timer });
      this.socket.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  }

  async eval(script) {
    return (await this.request("dom.eval", { script })).value;
  }

  async stop() {
    try { await this.request("app.quit"); } catch { /* runtime may already have stopped */ }
    this.socket?.destroy();
    if (this.child && this.child.exitCode === null) {
      await Promise.race([
        new Promise((resolve) => this.child.once("close", resolve)),
        new Promise((resolve) => setTimeout(resolve, 1000)),
      ]);
      if (this.child.exitCode === null) this.child.kill("SIGKILL");
    }
    try { fs.unlinkSync(this.socketPath); } catch { /* already removed */ }
  }
}

async function getVersion(binary, fallback = null) {
  try {
    const result = await run(binary, ["--version"], { timeout: Math.min(timeoutMs, 5000) });
    if (result.code === 0) return (result.stdout || result.stderr).trim().split(/\r?\n/, 1)[0] || "unknown";
  } catch { /* version reporting is optional; the probe still tests the executable itself */ }
  if (fallback) {
    try {
      const result = await run(fallback, ["--version"], { timeout: Math.min(timeoutMs, 5000) });
      if (result.code === 0) return (result.stdout || result.stderr).trim().split(/\r?\n/, 1)[0] || "unknown";
    } catch { /* the CLI may not report a version either */ }
  }
  return "unknown";
}

async function main() {
  if (cliPath) {
    if (!isExecutableFile(cliPath)) throw new Error(`squark CLI is not an executable file: ${cliPath}`);
    runtimePath = findRuntime(cliPath);
    if (!runtimePath) {
      throw new Error(`could not find squark-runtime for ${cliPath}; set SQUARK_RUNTIME_BIN or use --squark-binary <runtime-or-app>`);
    }
  }
  if (!isExecutableFile(runtimePath)) throw new Error(`runtime is not an executable file: ${runtimePath}`);

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "strange-quark-probe-"));
  let engine;
  try {
    const pageDir = path.join(tempDir, "page");
    fs.mkdirSync(pageDir);
    fs.writeFileSync(path.join(pageDir, "index.html"), PAGE);
    const packPath = path.join(tempDir, "probe.sqpak");
    if (cliPath) {
      const packed = await run(cliPath, ["pack", pageDir, "--html", "index.html", "--out", packPath], { cwd: pageDir });
      if (packed.code !== 0) throw new Error(`squark pack failed (${packed.code}):\n${packed.stderr || packed.stdout}`);
    } else {
      const packer = findSquarkPacker();
      if (!packer) {
        throw new Error(`--squark-binary needs a probe-page packer; build squark with cargo build -p squark-cli --bin squark, or use --squark <squark-cli>`);
      }
      const packed = await run(packer, ["pack", pageDir, "--html", "index.html", "--out", packPath], { cwd: pageDir });
      if (packed.code !== 0) throw new Error(`squark pack failed (${packed.code}):\n${packed.stderr || packed.stdout}`);
    }
    if (!fs.statSync(packPath).isFile() || fs.statSync(packPath).size === 0) throw new Error("squark pack did not produce a probe .sqpak file");

    engine = new SquarkEngine(runtimePath, packPath, tempDir);
    const result = await engine.start();
    const report = {
      squarkVersion: await getVersion(runtimePath, cliPath),
      binary: runtimePath,
      probedAt: new Date().toISOString(),
      apis: result.apis,
      css: result.css,
    };
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
    printTable(report);
  } finally {
    await engine?.stop();
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

function printTable(report) {
  console.log(`strange-quark probe: ${report.squarkVersion}`);
  console.log(`Binary: ${report.binary}`);
  console.log("\nAPIs");
  for (const [name, result] of Object.entries(report.apis)) {
    const works = result.works === null ? "n/a" : String(result.works);
    console.log(`${name.padEnd(36)} ${result.present.padEnd(9)} ${works}${result.error ? ` — ${result.error}` : ""}`);
  }
  console.log("\nCSS");
  for (const [name, result] of Object.entries(report.css)) {
    console.log(`${name.padEnd(36)} ${typeof result === "object" ? JSON.stringify(result) : result}`);
  }
  console.log(`\nWrote ${outPath}`);
}

main().catch((error) => {
  console.error(`probe failed: ${error.message}`);
  process.exitCode = 1;
});
#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Run the same flows in headless Chromium and in a Strange Quark binary, then
// compare DOM state and screenshots at every checkpoint and record timings.
//
//   node measure.mjs --dist <static build dir> --flows flows.json --out results \
//     [--squark-binary dist/app] [--runs 3] [--skip-squark] [--gpu-adapter auto|software|hardware]
//     [--deps-dir ~/.cache/strange-quark-migrate] [--timeout 30000]
//
// Needs playwright-core and pngjs, installed outside the user's project:
//   npm install --prefix ~/.cache/strange-quark-migrate playwright-core@1 pngjs@7
// See references/measuring.md for the flows format and what is measured.

import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { appExecutable, controlEndpoint, removeControlEndpoint, rssKbOfTree } from "./platform.mjs";

// ---- arguments -------------------------------------------------------------------

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const opt = (name, fallback = null) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const distDir = opt("--dist") && path.resolve(opt("--dist"));
const flowsPath = opt("--flows") && path.resolve(opt("--flows"));
const outDir = path.resolve(opt("--out", "strange-quark-migration/results"));
const squarkInput = opt("--squark-binary");
const runs = Math.max(1, Number(opt("--runs", "3")));
const skipSquark = flag("--skip-squark") || !squarkInput;
const gpuAdapter = opt("--gpu-adapter", null);
const timeoutMs = Number(opt("--timeout", "30000"));
const depsDir = path.resolve(opt("--deps-dir", path.join(os.homedir(), ".cache", "strange-quark-migrate")));

if (!distDir || !flowsPath) {
  console.error("usage: measure.mjs --dist <dir> --flows <flows.json> --out <dir> [--squark-binary <path>] [--runs N] [--skip-squark]");
  process.exit(2);
}
const squarkBinary = skipSquark ? null : appExecutable(path.resolve(squarkInput));

const requireFromDeps = createRequire(path.join(depsDir, "package.json"));
let chromiumLib, PNG;
try {
  ({ chromium: chromiumLib } = requireFromDeps("playwright-core"));
  ({ PNG } = requireFromDeps("pngjs"));
} catch (err) {
  console.error(`Missing dependencies in ${depsDir}. Run:\n  npm install --prefix ${depsDir} playwright-core@1 pngjs@7\n(${err.message})`);
  process.exit(2);
}

const spec = JSON.parse(fs.readFileSync(flowsPath, "utf8"));
const viewport = spec.viewport ?? { width: 1280, height: 800 };
const root = spec.root ?? "#root";
fs.mkdirSync(outDir, { recursive: true });

// ---- shared page-side scripts (identical in both engines) ------------------------

// A normalized serialization of the tree under `root`: tag, id, sorted classes,
// collapsed text, form values and checked state. Equal strings mean equal state.
const SNAPSHOT_JS = `(() => {
  const root = document.querySelector(${JSON.stringify(root)});
  if (!root) return "<missing root>";
  const walk = (n) => {
    if (n.nodeType === 3) { const t = n.data.replace(/\\s+/g, " "); return t === " " ? "" : t; }
    if (n.nodeType !== 1) return "";
    const tag = n.localName;
    if (tag === "script" || tag === "style") return "";
    let s = "<" + tag;
    if (n.id) s += "#" + n.id;
    const cls = n.getAttribute("class");
    if (cls) s += "." + cls.trim().split(/\\s+/).sort().join(".");
    if (tag === "input" || tag === "textarea" || tag === "select") {
      s += "[value=" + JSON.stringify(n.value) + "]";
      if (n.type === "checkbox" || n.type === "radio") s += n.checked ? "[checked]" : "[unchecked]";
    }
    s += ">";
    for (const c of n.childNodes) s += walk(c);
    return s + "</" + tag + ">";
  };
  return walk(root);
})()`;
// In-page responsiveness: from the most recent input event's timestamp to the
// first animation frame after the DOM changed in response. Same code, same clock,
// in both engines. The clock starts at the latest input event (pointerup, click,
// keyup, input...), not the first, because apps usually react to click or input:
// a driver that delivers pointerdown and pointerup on different frames would
// otherwise add a frame that belongs to the driver, not the engine.
const LATENCY_START_JS = `(() => {
  globalThis.__sqmLat = []; let pending = null;
  for (const t of ["pointerdown", "pointerup", "click", "dblclick", "keydown", "keyup", "beforeinput", "input", "wheel"]) addEventListener(t, (e) => { pending = e.timeStamp; }, { capture: true });
  new MutationObserver(() => {
    if (pending === null) return;
    const start = pending; pending = null;
    requestAnimationFrame((ts) => globalThis.__sqmLat.push(ts - start));
  }).observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  return true;
})()`;
const LATENCY_READ_JS = `(() => globalThis.__sqmLat || [])()`;
const MOUNTED_JS = `(() => { const r = document.querySelector(${JSON.stringify(root)}); return !!(r && r.childElementCount > 0); })()`;

// ---- helpers ------------------------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const median = (xs) => {
  const s = xs.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  if (!s.length) return null;
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
const percentile = (xs, p) => {
  const s = xs.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))] : null;
};
const slug = (s) => String(s).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

function findChromium() {
  const system = {
    darwin: ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"],
    win32: [process.env.ProgramFiles, process.env["ProgramFiles(x86)"], process.env.LOCALAPPDATA]
      .filter(Boolean).map((dir) => path.join(dir, "Google", "Chrome", "Application", "chrome.exe")),
    linux: ["/usr/bin/chromium", "/usr/bin/chromium-browser"],
  };
  const names = process.platform === "win32"
    ? ["chrome.exe", "chromium.exe"]
    : ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"];
  const fromPath = (process.env.PATH ?? "").split(path.delimiter).filter(Boolean)
    .flatMap((dir) => names.map((name) => path.join(dir, name)));
  const candidates = [
    process.env.SQUARK_CHROMIUM_PATH, chromiumLib.executablePath(), ...(system[process.platform] ?? []), ...fromPath,
  ];
  const executable = candidates.find((candidate) => candidate && fs.statSync(candidate, { throwIfNoEntry: false })?.isFile());
  if (executable) return executable;
  throw new Error("No Chromium found; set SQUARK_CHROMIUM_PATH");
}

// Static server for the build output, with SPA fallback to index.html.
function serve(dir) {
  const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".json": "application/json", ".svg": "image/svg+xml", ".png": "image/png", ".jpg": "image/jpeg", ".webp": "image/webp", ".woff2": "font/woff2", ".woff": "font/woff", ".ttf": "font/ttf", ".wasm": "application/wasm", ".ico": "image/x-icon" };
  const server = http.createServer((req, res) => {
    const urlPath = decodeURIComponent(new URL(req.url, "http://x").pathname);
    let file = path.join(dir, urlPath);
    if (!file.startsWith(dir) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
      const asIndex = path.join(file, "index.html");
      file = fs.existsSync(asIndex) ? asIndex : path.join(dir, "index.html");
    }
    res.writeHead(200, { "content-type": types[path.extname(file)] ?? "application/octet-stream" });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

const KEY_CODES = { " ": "Space", Enter: "Enter", Escape: "Escape", Tab: "Tab", Backspace: "Backspace", Delete: "Delete", ArrowUp: "ArrowUp", ArrowDown: "ArrowDown", ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown" };
const keyToCode = (k) => KEY_CODES[k] ?? (/^[a-z]$/i.test(k) ? `Key${k.toUpperCase()}` : /^\d$/.test(k) ? `Digit${k}` : k);
function parseChord(chord) {
  const parts = chord.split("+");
  const key = parts.pop();
  const mods = { shift: false, ctrl: false, alt: false, meta: false };
  for (const p of parts) {
    const n = p.toLowerCase();
    if (n === "control" || n === "ctrl") mods.ctrl = true;
    else if (n === "shift") mods.shift = true;
    else if (n === "alt") mods.alt = true;
    else if (n === "meta" || n === "cmd") mods.meta = true;
  }
  return { key, mods };
}

// ---- engines --------------------------------------------------------------------------

class ChromiumEngine {
  name = "chromium";
  async start(url) {
    const t0 = performance.now();
    this.server = await chromiumLib.launchServer({ executablePath: findChromium(), headless: true, args: ["--force-color-profile=srgb", "--font-render-hinting=none", "--disable-lcd-text", "--hide-scrollbars"] });
    this.browser = await chromiumLib.connect(this.server.wsEndpoint());
    this.context = await this.browser.newContext({ viewport, deviceScaleFactor: 1 }); // fresh profile per run
    this.page = await this.context.newPage();
    await this.page.goto(url, { waitUntil: "load", timeout: timeoutMs });
    await this.page.waitForFunction(MOUNTED_JS, null, { timeout: timeoutMs });
    await this.page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    return performance.now() - t0;
  }
  eval(js) { return this.page.evaluate(js); }
  idle() { return this.page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))); }
  async center(sel) {
    const r = await this.page.evaluate((s) => { const e = document.querySelector(s); if (!e) return null; const b = e.getBoundingClientRect(); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; }, sel);
    if (!r) throw new Error(`no element matches ${sel}`);
    return r;
  }
  async clickAt(pt) { await this.page.mouse.click(pt.x, pt.y); }
  async dblclickAt(pt) { await this.page.mouse.dblclick(pt.x, pt.y); }
  async type(text) { await this.page.keyboard.type(text); }
  async press(chord) { await this.page.keyboard.press(chord); }
  async wheelAt(pt, dy) { await this.page.mouse.move(pt.x, pt.y); await this.page.mouse.wheel(0, dy); }
  async screenshot(file) { await this.page.screenshot({ path: file }); }
  rssKb() { const pid = this.server?.process()?.pid; return pid ? rssKbOfTree(pid) : null; }
  async stop() { await this.browser?.close().catch(() => {}); await this.server?.close(); }
}

class SquarkEngine {
  name = "strange-quark";
  pending = new Map();
  nextId = 1;
  buffer = "";
  async start() {
    this.dataHome = fs.mkdtempSync(path.join(os.tmpdir(), "sqm-data-")); // fresh storage per run
    this.sock = controlEndpoint();
    const args = ["--test-control", this.sock, "--headless", "--width", String(viewport.width), "--height", String(viewport.height), "--dpr", "1"];
    if (gpuAdapter) args.push("--gpu-adapter", gpuAdapter);
    const t0 = performance.now();
    this.child = spawn(squarkBinary, args, { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, XDG_DATA_HOME: this.dataHome } });
    this.stderr = "";
    this.child.once("error", (error) => { this.spawnError = error; });
    this.child.stderr.on("data", (c) => { this.stderr += c; });
    this.child.stdout.on("data", (c) => { this.stderr += c; });
    this.socket = await this.connect();
    this.socket.setEncoding("utf8").on("data", (chunk) => this.onData(chunk));
    this.socket.on("error", (error) => this.rejectPending(error));
    const deadline = Date.now() + timeoutMs;
    while (true) {
      await this.request("frame.waitIdle", { frames: 1 });
      if ((await this.eval(MOUNTED_JS)) === true) break;
      if (Date.now() > deadline) throw new Error(`root ${root} never mounted. Runtime output:\n${this.stderr.slice(-2000)}`);
    }
    return performance.now() - t0;
  }
  connect() {
    const deadline = Date.now() + timeoutMs;
    return new Promise((resolve, reject) => {
      const attempt = () => {
        if (this.spawnError) return reject(this.spawnError);
        const status = this.child.exitCode ?? this.child.signalCode;
        if (status !== null) return reject(new Error(`strange-quark exited (${status}) before opening test control:\n${this.stderr.slice(-2000)}`));
        const s = net.createConnection({ path: this.sock });
        s.once("connect", () => resolve(s));
        s.once("error", () => {
          s.destroy();
          if (Date.now() > deadline) reject(new Error(`timed out waiting for ${this.sock}`));
          else setTimeout(attempt, 25);
        });
      };
      attempt();
    });
  }
  onData(chunk) {
    this.buffer += chunk;
    let nl;
    while ((nl = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, nl).trim();
      this.buffer = this.buffer.slice(nl + 1);
      if (!line) continue;
      const msg = JSON.parse(line);
      const p = this.pending.get(msg.id);
      if (!p) continue;
      this.pending.delete(msg.id);
      clearTimeout(p.timer);
      msg.error ? p.reject(new Error(`${p.method}: ${msg.error.message}`)) : p.resolve(msg.result);
    }
  }
  request(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${method} timed out`)); }, timeoutMs);
      this.pending.set(id, { method, resolve, reject, timer });
      this.socket.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }
  rejectPending(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }
  async eval(js) { return (await this.request("dom.eval", { script: js })).value; }
  idle() { return this.request("frame.waitIdle", { frames: 2 }); }
  async center(sel) {
    const { rect } = await this.request("dom.queryRect", { selector: sel });
    if (!rect) throw new Error(`no element matches ${sel}`);
    return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
  }
  async pointer(kind, pt, buttons) { await this.request("input.pointer", { kind, x: pt.x, y: pt.y, button: 0, buttons, modifiers: 0, pointerId: 1 }); }
  async clickAt(pt) { await this.pointer("move", pt, 0); await this.pointer("down", pt, 1); await this.pointer("up", pt, 0); }
  async dblclickAt(pt) { for (let i = 0; i < 2; i++) { await this.pointer("down", pt, 1); await this.pointer("up", pt, 0); } }
  async key(key, mods, text) {
    const bits = (mods.shift ? 1 : 0) | (mods.ctrl ? 2 : 0) | (mods.alt ? 4 : 0) | (mods.meta ? 8 : 0);
    const code = keyToCode(key);
    await this.request("input.key", { kind: "down", key, code, ...(text ? { text } : {}), modifiers: bits });
    await this.request("input.key", { kind: "up", key, code, modifiers: bits });
  }
  async type(text) { for (const ch of text) await this.key(ch, {}, ch); }
  async press(chord) { const { key, mods } = parseChord(chord); const printable = key.length === 1 && !mods.ctrl && !mods.alt && !mods.meta; await this.key(key, mods, printable ? key : null); }
  async wheelAt(pt, dy) { await this.request("input.wheel", { x: pt.x, y: pt.y, dx: 0, dy, modifiers: 0 }); }
  async screenshot(file) {
    const res = await this.request("frame.screenshot", { path: file });
    if (res.pngBase64 && !fs.existsSync(file)) fs.writeFileSync(file, Buffer.from(res.pngBase64, "base64"));
  }
  rssKb() { return this.child?.pid ? rssKbOfTree(this.child.pid) : null; }
  async stop() {
    if (this.socket && !this.socket.destroyed) {
      try { await this.request("app.quit"); } catch { /* already gone */ }
      this.socket.destroy();
    }
    if (this.child?.pid && this.child.exitCode === null && this.child.signalCode === null) {
      await sleep(200);
      if (this.child.exitCode === null && this.child.signalCode === null) this.child.kill("SIGKILL");
    }
    removeControlEndpoint(this.sock);
    if (this.dataHome) fs.rmSync(this.dataHome, { recursive: true, force: true });
  }
}

// ---- running flows ----------------------------------------------------------------------

async function runOnce(engine, url, run) {
  const result = { run, startupMs: null, rssKb: null, stepLatencyMs: [], inputToFrameMs: [], checkpoints: {}, errors: [] };
  try {
    result.startupMs = await engine.start(url);
    await engine.eval(LATENCY_START_JS);
    for (const flow of spec.flows) {
      for (const step of flow.steps) {
        try {
          // Resolve targets before timing, so the step time is input plus idle only.
          const target = step.click ?? step.dblclick ?? step.wheel?.selector;
          const pt = target ? await engine.center(target) : null;
          const t0 = performance.now();
          if (step.click) await engine.clickAt(pt);
          else if (step.dblclick) await engine.dblclickAt(pt);
          else if (step.type !== undefined) await engine.type(step.type);
          else if (step.press) await engine.press(step.press);
          else if (step.wheel) await engine.wheelAt(pt, step.wheel.dy ?? 0);
          else if (step.wait) await sleep(step.wait);
          else if (step.checkpoint) {
            const id = `${slug(flow.name)}--${slug(step.checkpoint)}`;
            const dir = path.join(outDir, engine.name, `run-${run}`);
            fs.mkdirSync(dir, { recursive: true });
            const png = path.join(dir, `${id}.png`);
            await engine.idle();
            await engine.screenshot(png);
            result.checkpoints[id] = { flow: flow.name, dom: await engine.eval(SNAPSHOT_JS), screenshot: png };
            continue;
          }
          await engine.idle();
          if (!step.wait) result.stepLatencyMs.push(performance.now() - t0);
        } catch (err) {
          result.errors.push({ flow: flow.name, step, error: String(err.message ?? err) });
          break; // the rest of this flow depends on this step
        }
      }
    }
    result.inputToFrameMs = (await engine.eval(LATENCY_READ_JS)) ?? [];
    result.rssKb = engine.rssKb();
  } catch (err) {
    result.errors.push({ flow: null, step: "start", error: String(err.message ?? err) });
  } finally {
    await engine.stop().catch(() => {});
  }
  return result;
}

function comparePng(a, b, diffPath) {
  const A = PNG.sync.read(fs.readFileSync(a));
  const B = PNG.sync.read(fs.readFileSync(b));
  if (A.width !== B.width || A.height !== B.height) return { pass: false, reason: `size ${A.width}x${A.height} vs ${B.width}x${B.height}` };
  const out = new PNG({ width: A.width, height: A.height });
  let diff = 0;
  let maxDelta = 0;
  for (let i = 0; i < A.data.length; i += 4) {
    const d = Math.max(Math.abs(A.data[i] - B.data[i]), Math.abs(A.data[i + 1] - B.data[i + 1]), Math.abs(A.data[i + 2] - B.data[i + 2]), Math.abs(A.data[i + 3] - B.data[i + 3]));
    maxDelta = Math.max(maxDelta, d);
    const g = (A.data[i] * 0.3 + A.data[i + 1] * 0.59 + A.data[i + 2] * 0.11) * 0.3 + 178;
    if (d > 8) {
      diff++;
      out.data.set([255, 0, 255, 255], i);
    } else out.data.set([g, g, g, 255], i);
  }
  fs.mkdirSync(path.dirname(diffPath), { recursive: true });
  fs.writeFileSync(diffPath, PNG.sync.write(out));
  const ratio = diff / (A.width * A.height);
  return { pass: ratio <= 0.005, diffRatio: ratio, maxDelta, diffImage: diffPath };
}

function summarize(results) {
  return {
    runs: results.length,
    startupMsMedian: median(results.map((r) => r.startupMs)),
    rssMbMedian: median(results.map((r) => (r.rssKb ? r.rssKb / 1024 : null))),
    inputToFrameP50Ms: percentile(results.flatMap((r) => r.inputToFrameMs), 50),
    inputToFrameP95Ms: percentile(results.flatMap((r) => r.inputToFrameMs), 95),
    inputToFrameSamples: results.reduce((n, r) => n + r.inputToFrameMs.length, 0),
    stepLatencyP50Ms: percentile(results.flatMap((r) => r.stepLatencyMs), 50),
    stepLatencyP95Ms: percentile(results.flatMap((r) => r.stepLatencyMs), 95),
    stepLatencyMaxMs: percentile(results.flatMap((r) => r.stepLatencyMs), 100),
    errors: results.flatMap((r) => r.errors.map((e) => ({ run: r.run, ...e }))),
  };
}

// ---- main ----------------------------------------------------------------------------------

const server = await serve(distDir);
const url = `http://127.0.0.1:${server.address().port}/`;
const chromiumRuns = [];
const squarkRuns = [];
for (let i = 1; i <= runs; i++) {
  chromiumRuns.push(await runOnce(new ChromiumEngine(), url, i));
  if (!skipSquark) squarkRuns.push(await runOnce(new SquarkEngine(), url, i));
}
server.close();

const checkpoints = [];
const base = chromiumRuns[0];
const native = squarkRuns[0];
for (const [id, c] of Object.entries(base.checkpoints)) {
  const flow = spec.flows.find((f) => f.name === c.flow) ?? {};
  const entry = { id, flow: c.flow, core: flow.core !== false, stub: flow.stub === true };
  const j = native?.checkpoints[id];
  if (skipSquark) entry.squark = "not run";
  else if (!j) entry.squark = "missing (the flow failed before this checkpoint)";
  else {
    entry.domEqual = c.dom === j.dom;
    if (!entry.domEqual) {
      const dir = path.join(outDir, "dom");
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, `${id}.chromium.txt`), c.dom);
      fs.writeFileSync(path.join(dir, `${id}.strange-quark.txt`), j.dom);
    }
    entry.screenshot = comparePng(c.screenshot, j.screenshot, path.join(outDir, "diff", `${id}.png`));
  }
  checkpoints.push(entry);
}
const stable = (rs) => rs.length < 2 || Object.keys(rs[0].checkpoints).every((id) => rs.every((r) => r.checkpoints[id]?.dom === rs[0].checkpoints[id]?.dom));

const report = {
  measuredAt: new Date().toISOString(),
  host: { platform: `${os.type()} ${os.release()}`, cpus: os.cpus().length, gpuAdapter: gpuAdapter ?? "auto" },
  viewport,
  root,
  runsPerEngine: runs,
  chromium: { ...summarize(chromiumRuns), domStableAcrossRuns: stable(chromiumRuns) },
  strangeQuark: skipSquark ? "not measured" : { ...summarize(squarkRuns), domStableAcrossRuns: stable(squarkRuns), binaryBytes: fs.statSync(squarkBinary).size },
  tolerance: { maxChannelDelta: 8, maxDiffRatio: 0.005, dom: "exact" },
  checkpoints,
};
fs.writeFileSync(path.join(outDir, "measure.json"), JSON.stringify(report, null, 2) + "\n");

const fmt = (x, unit) => (x === null || x === undefined ? "not measured" : `${Math.round(x * 10) / 10}${unit}`);
console.log(`measured ${spec.flows.length} flows, ${checkpoints.length} checkpoints, ${runs} run(s) per engine -> ${path.join(outDir, "measure.json")}`);
for (const c of checkpoints) {
  const s = c.screenshot;
  const j = typeof c.squark === "string" ? c.squark : `dom ${c.domEqual ? "equal" : "DIFFERENT"}, screenshot ${s.pass ? "pass" : "FAIL"} (${s.diffRatio !== undefined ? (s.diffRatio * 100).toFixed(3) + "%" : s.reason})`;
  console.log(`  ${c.id}${c.stub ? " [stub]" : ""}: ${j}`);
}
for (const [name, r] of [["chromium", report.chromium], ["strange-quark", report.strangeQuark]]) {
  if (typeof r === "string") { console.log(`  ${name}: ${r}`); continue; }
  console.log(`  ${name}: first frame ${fmt(r.startupMsMedian, " ms")}, RSS ${fmt(r.rssMbMedian, " MiB")}, input-to-frame p50/p95 ${fmt(r.inputToFrameP50Ms, "")}/${fmt(r.inputToFrameP95Ms, " ms")} (${r.inputToFrameSamples} samples), harness step p50 ${fmt(r.stepLatencyP50Ms, " ms")}, errors ${r.errors.length}`);
  for (const e of r.errors.slice(0, 5)) console.log(`    error in ${e.flow ?? "startup"} (run ${e.run}): ${e.error.split("\n")[0]}`);
}

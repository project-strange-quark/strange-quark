// SPDX-License-Identifier: MIT
// Excalidraw v0.18.1 on the native runtime.
//
// Builds (optionally), packs and runs vendor/excalidraw headless with test
// control, then checks three things: the app mounts, the toolbar is present,
// and drawing a rectangle changes the canvas. Every failure is reported with
// the component most likely at fault and the diagnostics behind it: startup
// exceptions, console errors, missing APIs, and the DOM at each stage.
//
// Two runs are made:
//   1. the unmodified pack, for the three checks;
//   2. a diagnostic copy of `build/` with an error/console probe injected
//      after `<head>`, so exceptions thrown by the app's own scripts during
//      startup are captured. `vendor/excalidraw` is never modified.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";

import { SquarkDriver } from "../../compat/src/squark-driver.mjs";
import { auditAssets } from "./assets.mjs";
import { platformSuffix, repoRelative } from "./platform.mjs";
import {
  assertSubmoduleClean,
  buildDir,
  buildWithYarn,
  gpuAdapter,
  inspectPack,
  squarkBin,
  outDir,
  packExcalidraw as buildExcalidraw,
  repoRoot,
  resolveRuntime,
} from "./build.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const excalidrawDir = path.join(repoRoot, "vendor/excalidraw");
const appDir = path.join(excalidrawDir, "excalidraw-app");
const resultsDir = path.join(repoRoot, "tests/results/platform");
const suffix = platformSuffix();
// Screenshots stay under a directory named "actual", which .gitignore covers;
// other hosts add a subdirectory instead of a sibling.
const shotsDir = path.join(resultsDir, "actual", suffix ? suffix.slice(1) : "");
const defaultReportPath = path.join(resultsDir, `excalidraw-native${suffix}.json`);

const runtime = resolveRuntime();

/** Peak resident set of a live process, in bytes, where the host reports it. */
function peakRssBytes(pid) {
  if (!pid) return null;
  if (process.platform === "linux") {
    try {
      const status = fs.readFileSync(`/proc/${pid}/status`, "utf8");
      const match = status.match(/^VmHWM:\s+(\d+) kB$/m);
      return match ? Number(match[1]) * 1024 : null;
    } catch {
      return null;
    }
  }
  if (process.platform === "darwin") {
    try {
      // `ps` reports resident set in KiB; it is current RSS, not peak, but it
      // is the best figure macOS exposes to an unprivileged process.
      const kib = Number.parseInt(
        execFileSync("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }).trim(),
        10,
      );
      return Number.isFinite(kib) ? kib * 1024 : null;
    } catch {
      return null;
    }
  }
  return null;
}

/** Installed before any app script so startup exceptions are observable. */
const PROBE = `<script>
window.__squarkErrorLog = [];
window.__squarkConsole = [];
window.addEventListener("error", function (e) {
  window.__squarkErrorLog.push("error: " + ((e.error && (e.error.stack || e.error.message)) || e.message));
});
window.addEventListener("unhandledrejection", function (e) {
  var r = e.reason;
  window.__squarkErrorLog.push("rejection: " + ((r && (r.stack || r.message)) || String(r)));
});
["error", "warn"].forEach(function (level) {
  var original = console[level];
  console[level] = function () {
    try {
      window.__squarkConsole.push(level + ": " + Array.prototype.join.call(arguments, " ").slice(0, 500));
    } catch (e) {}
    return original.apply(console, arguments);
  };
});
</script>
`;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Copies `build/` with the probe script injected, and packs the copy. */
function packDiagnosticCopy() {
  const dir = path.join(outDir, "diagnostic");
  fs.rmSync(dir, { recursive: true, force: true });
  fs.cpSync(buildDir, dir, { recursive: true });
  const htmlPath = path.join(dir, "index.html");
  const html = fs.readFileSync(htmlPath, "utf8");
  assert.ok(html.includes("<head>"), "Excalidraw's index.html lost its <head>");
  fs.writeFileSync(htmlPath, html.replace("<head>", `<head>${PROBE}`));
  const sqpak = path.join(outDir, "diagnostic.sqpak");
  fs.rmSync(sqpak, { force: true });
  execFileSync(squarkBin, ["pack", dir, "--out", sqpak], {
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 32 * 1024 * 1024,
  });
  return sqpak;
}

/** What the pack contains, for the report. */
function launch(binaryPath, args = [], { adapter = gpuAdapter, env = null } = {}) {
  return SquarkDriver.launch({
    binaryPath,
    args,
    headless: true,
    width: 1280,
    height: 800,
    gpuAdapter: adapter === "auto" ? null : adapter,
    env: env ?? {},
    timeoutMs: 120_000,
  });
}

async function evalSafe(driver, expression) {
  try {
    return { ok: true, value: await driver.eval(expression) };
  } catch (error) {
    return { ok: false, error: error.message };
  }
}

/** The DOM facts the smoke test checks, read defensively. */
async function domSnapshot(driver) {
  const [title, rootChildren, excalidraw, canvases, svg, toolbarButtons, bodyLength, scripts] =
    await Promise.all([
      evalSafe(driver, "document.title"),
      evalSafe(driver, "document.getElementById('root')?.children.length ?? null"),
      evalSafe(driver, "!!document.querySelector('.excalidraw')"),
      evalSafe(driver, "document.querySelectorAll('canvas').length"),
      evalSafe(driver, "document.querySelectorAll('svg').length"),
      evalSafe(driver, "document.querySelectorAll('[data-testid^=\"toolbar-\"]').length"),
      evalSafe(driver, "document.body?.innerHTML.length ?? 0"),
      evalSafe(driver, "document.querySelectorAll('script').length"),
    ]);
  const value = (entry) => (entry.ok ? entry.value : { error: entry.error });
  return {
    title: value(title),
    rootChildren: value(rootChildren),
    excalidrawRoot: value(excalidraw),
    canvases: value(canvases),
    svgElements: value(svg),
    toolbarButtons: value(toolbarButtons),
    bodyHtmlLength: value(bodyLength),
    scripts: value(scripts),
  };
}

/** Probes the Web APIs Excalidraw uses and the DOM members its startup reads. */
async function apiProbe(driver) {
  const result = await evalSafe(
    driver,
    `(() => {
      const type = (v) => typeof v;
      const innerText = (() => {
        try { return type(document.body.innerText); } catch (e) { return "throws: " + e.message; }
      })();
      const cookie = (() => {
        try { return JSON.stringify(document.cookie); } catch (e) { return "throws: " + e.message; }
      })();
      return {
        "document.cookie": type(document.cookie) + " " + cookie,
        "document.body.innerText": innerText,
        "document.scripts": type(document.scripts),
        "document.fonts": type(document.fonts),
        "document.fonts.check": type(document.fonts && document.fonts.check),
        "FontFace": type(window.FontFace),
        "ResizeObserver": type(window.ResizeObserver),
        "IntersectionObserver": type(window.IntersectionObserver),
        "MutationObserver": type(window.MutationObserver),
        "matchMedia": type(window.matchMedia),
        "indexedDB": type(window.indexedDB),
        "crypto.subtle": type(window.crypto && window.crypto.subtle),
        "ClipboardItem": type(window.ClipboardItem),
        "navigator.clipboard": type(navigator.clipboard),
        "FileReader": type(window.FileReader),
        "Blob": type(window.Blob),
        "File": type(window.File),
        "WebAssembly": type(window.WebAssembly),
        "Worker": type(window.Worker),
        "OffscreenCanvas": type(window.OffscreenCanvas),
        "createImageBitmap": type(window.createImageBitmap),
        "Path2D": type(window.Path2D),
        "CanvasRenderingContext2D": type(window.CanvasRenderingContext2D),
        "HTMLCanvasElement": type(window.HTMLCanvasElement),
        "getSelection": type(window.getSelection),
        "structuredClone": type(window.structuredClone),
        "WebSocket": type(window.WebSocket),
        "EventSource": type(window.EventSource),
        "fetch": type(window.fetch),
        "localStorage": type(window.localStorage),
        "history.pushState": type(window.history && window.history.pushState),
      };
    })()`,
  );
  return result.ok ? result.value : { error: result.error };
}

/** The component an exception message points at, using the API probe when it helps. */
export function componentForError(message = "", api = {}) {
  const cookie = String(api?.["document.cookie"] ?? "");
  if (
    message.includes("document.cookie") ||
    (message.includes("reading 'includes'") && cookie.startsWith("undefined"))
  ) {
    return "squark-dom (document.cookie, which reads as cookies disabled)";
  }
  if (message.includes("innerText")) return "squark-dom (HTMLElement.innerText)";
  if (message.includes("document.scripts")) return "squark-dom (Document.scripts)";
  if (message.includes("Canvas 2D backend") || message.includes("squark-canvas")) {
    return "squark-canvas (canvas execution)";
  }
  if (message.includes("canvas")) return "squark-dom canvas recorder / squark-canvas";
  return "squark-dom (document JavaScript)";
}

/**
 * Runs the injected-probe copy and returns the startup exceptions, console
 * output and API probe. The app scripts run exactly as in the real pack.
 */
async function runDiagnostics(sqpak, launchOpts) {
  let driver = null;
  try {
    driver = await launch(runtime.path, [sqpak], launchOpts);
    // Let the app's modules settle; a mounted app would finish well before this.
    await sleep(3_000);
    const errors = await evalSafe(driver, "window.__squarkErrorLog ?? null");
    const consoleLog = await evalSafe(driver, "window.__squarkConsole ?? null");
    const api = await apiProbe(driver);
    const dom = await domSnapshot(driver);
    const stderr = driver.stderrText();
    await driver.nextPresented().catch(() => undefined);
    await driver.screenshot(path.join(shotsDir, "excalidraw-diagnostic.png")).catch(() => undefined);
    return {
      errors: errors.ok ? errors.value : { error: errors.error },
      console: consoleLog.ok ? consoleLog.value : { error: consoleLog.error },
      api,
      dom,
      stderrTail: stderr.slice(-4000),
    };
  } catch (error) {
    return { harnessError: error.message };
  } finally {
    await driver?.close().catch(() => undefined);
  }
}

/**
 * A three-line pack proving whether an uncaught module exception is reported
 * on the runtime's stderr at all.
 */
async function probeModuleErrorReporting(launchOpts) {
  const dir = path.join(outDir, "module-probe");
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(
    path.join(dir, "index.html"),
    '<!doctype html><html><body><script>window.__inlineRan = "yes";</script>' +
      '<script type="module">window.__moduleRan = "yes"; throw new Error("squark-module-probe");</script>' +
      "</body></html>",
  );
  const sqpak = path.join(outDir, "module-probe.sqpak");
  execFileSync(squarkBin, ["pack", dir, "--out", sqpak], { encoding: "utf8", timeout: 60_000 });
  let driver = null;
  try {
    driver = await launch(runtime.path, [sqpak], launchOpts);
    await sleep(1_000);
    const ran = await evalSafe(driver, "window.__moduleRan ?? null");
    const stderr = driver.stderrText();
    return {
      moduleRan: ran.ok ? ran.value : { error: ran.error },
      reported: /JavaScript task failed|TypeError|Error:/.test(stderr),
      stderrTail: stderr.slice(-1500),
    };
  } finally {
    await driver?.close().catch(() => undefined);
  }
}

async function waitFor(driver, expression, { label, timeoutMs = 20_000, intervalMs = 50 } = {}) {
  const deadline = performance.now() + timeoutMs;
  let last;
  while (performance.now() < deadline) {
    const result = await evalSafe(driver, expression);
    last = result.ok ? result.value : `eval error: ${result.error}`;
    if (last === true) return true;
    await sleep(intervalMs);
  }
  throw new Error(`timed out waiting for ${label ?? expression} (last=${JSON.stringify(last)})`);
}

function readPng(file) {
  return PNG.sync.read(fs.readFileSync(file));
}

/** Counts pixels that differ by more than `delta` inside `rect` (CSS pixels). */
function diffRegion(beforePath, afterPath, rect, delta = 8) {
  const before = readPng(beforePath);
  const after = readPng(afterPath);
  assert.equal(before.width, after.width, "screenshot width changed");
  assert.equal(before.height, after.height, "screenshot height changed");
  const x0 = Math.max(0, Math.floor(rect.x));
  const y0 = Math.max(0, Math.floor(rect.y));
  const x1 = Math.min(before.width, Math.ceil(rect.x + rect.width));
  const y1 = Math.min(before.height, Math.ceil(rect.y + rect.height));
  let differing = 0;
  let maxDelta = 0;
  for (let y = y0; y < y1; y += 1) {
    for (let x = x0; x < x1; x += 1) {
      const at = (y * before.width + x) * 4;
      const d = Math.max(
        Math.abs(before.data[at] - after.data[at]),
        Math.abs(before.data[at + 1] - after.data[at + 1]),
        Math.abs(before.data[at + 2] - after.data[at + 2]),
        Math.abs(before.data[at + 3] - after.data[at + 3]),
      );
      if (d > maxDelta) maxDelta = d;
      if (d > delta) differing += 1;
    }
  }
  return { differingPixels: differing, maxDelta, region: { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } };
}

async function runCheck(checks, name, component, fn) {
  try {
    const detail = await fn();
    checks.push({ name, component, pass: true, detail: detail ?? null });
    console.log(`PASS ${name}${detail ? `: ${JSON.stringify(detail).slice(0, 300)}` : ""}`);
    return true;
  } catch (error) {
    checks.push({ name, component, pass: false, error: error.message });
    console.log(`FAIL ${name} [component: ${component}]: ${error.message}`);
    return false;
  }
}

/**
 * Runs the headless Excalidraw smoke test and returns the report.
 */
export async function runExcalidrawNative({
  build = false,
  adapter = gpuAdapter,
  env = null,
  outPath = defaultReportPath,
} = {}) {
  const launchOpts = { adapter, env };
  const effectiveVkIcd =
    env && Object.hasOwn(env, "VK_ICD_FILENAMES")
      ? env.VK_ICD_FILENAMES
      : process.env.VK_ICD_FILENAMES ?? null;
  fs.mkdirSync(shotsDir, { recursive: true });
  if (build) buildWithYarn();
  assert.ok(fs.existsSync(path.join(buildDir, "index.html")), `missing ${buildDir}/index.html`);
  assertSubmoduleClean();
  const packed = buildExcalidraw();
  const binary = packed.binary;
  const pack = inspectPack(binary);

  // The pack must serve every asset the built app asks for. This catches fonts
  // and images the app's Vite build did not emit, which Chromium silently loads
  // from a CDN and a native pack cannot.
  let assetAudit = null;
  try {
    assetAudit = await auditAssets({ buildDir, packPaths: pack.paths });
  } catch (error) {
    assetAudit = { status: "not-run", error: error.message };
  }

  const diagnostics = await runDiagnostics(packDiagnosticCopy(), launchOpts);
  const startupErrors = Array.isArray(diagnostics.errors) ? diagnostics.errors : [];
  // Web APIs Excalidraw uses that this runtime does not define. Worker and
  // OffscreenCanvas are deliberately undefined, so they are not
  // counted as missing.
  const missingApis = Object.entries(diagnostics.api ?? {})
    .filter(([name, value]) => String(value).startsWith("undefined") && !["Worker", "OffscreenCanvas"].includes(name))
    .map(([name, value]) => ({ name, value }));
  const moduleErrorReporting = await probeModuleErrorReporting(launchOpts);

  const checks = [];
  const dom = {};
  await runCheck(checks, "the pack serves every asset the app requests", "squark-cli (packing)", async () => {
    if (!assetAudit || assetAudit.status === "not-run") {
      throw new Error(`the asset audit did not run: ${assetAudit?.error ?? "unknown error"}`);
    }
    if (!assetAudit.passed) {
      throw new Error(
        `missing from the pack: css ${JSON.stringify(assetAudit.cssRelativeMissing)}, ` +
          `requests ${JSON.stringify(assetAudit.unserved)}`,
      );
    }
    return {
      requests: assetAudit.requests,
      packEntries: assetAudit.packEntries,
      external: assetAudit.external.length,
    };
  });
  const startup = { launchMs: null, firstFrameMs: null };
  let peakRss = null;
  const started = performance.now();
  let driver = null;
  let stderr = "";
  try {
    try {
      const launchStart = performance.now();
      driver = await launch(binary, [], launchOpts);
      startup.launchMs = Number((performance.now() - launchStart).toFixed(2));
    } catch (error) {
      for (const [name, component] of [
        ["app mounts", componentForError(error.message)],
        ["toolbar is present", componentForError(error.message)],
        ["drawing a rectangle changes the canvas", componentForError(error.message)],
      ]) {
        checks.push({ name, component, pass: false, error: error.message });
        console.log(`FAIL ${name} [component: ${component}]: ${error.message}`);
      }
      throw new Error(error.message);
    }

    dom.afterLaunch = await domSnapshot(driver);
    // First presented frame once the document has finished loading; the
    // timestamp is on the runtime's own monotonic clock (process start).
    await evalSafe(driver, "document.readyState");
    await sleep(1_000);
    const firstFrame = await driver.nextPresented().catch(() => null);
    startup.firstFrameMs = firstFrame?.presentedAtMs ?? null;
    peakRss = peakRssBytes(driver._child?.pid);
    const mounted = await runCheck(
      checks,
      "app mounts",
      componentForError(startupErrors[0] ?? "", diagnostics.api),
      async () => {
        try {
          await waitFor(driver, `!!document.querySelector('.excalidraw')`, {
            label: "the Excalidraw root",
            timeoutMs: 20_000,
          });
        } catch (error) {
          const cause = startupErrors[0] ?? error.message;
          throw new Error(
            `the app did not render: ${String(cause).split("\n")[0]} ` +
              `(#root children=${JSON.stringify(dom.afterLaunch.rootChildren)}, ` +
              `body html=${dom.afterLaunch.bodyHtmlLength})`,
          );
        }
        const canvases = await driver.eval(`document.querySelectorAll('canvas').length`);
        if (canvases === 0) throw new Error("the Excalidraw root exists but has no canvas element");
        return { canvases };
      },
    );

    await driver.nextPresented().catch(() => undefined);
    await driver.screenshot(path.join(shotsDir, "excalidraw-mounted.png")).catch(() => undefined);

    await runCheck(checks, "toolbar is present", "squark-layout / squark-paint", async () => {
      dom.afterToolbar = await domSnapshot(driver);
      const buttons = dom.afterToolbar.toolbarButtons;
      if (typeof buttons !== "number" || buttons < 5) {
        throw new Error(
          `expected the tool buttons, found ${JSON.stringify(buttons)}; ` +
            `DOM: #root children=${JSON.stringify(dom.afterToolbar.rootChildren)}, ` +
            `canvases=${JSON.stringify(dom.afterToolbar.canvases)}`,
        );
      }
      const rectangle = await evalSafe(
        driver,
        `!!document.querySelector('[data-testid="toolbar-rectangle"]')`,
      );
      if (!rectangle.ok || !rectangle.value) throw new Error("missing [data-testid=toolbar-rectangle]");
      return { buttons };
    });

    await runCheck(
      checks,
      "drawing a rectangle changes the canvas",
      "squark-canvas / squark-dom canvas recorder",
      async () => {
        if (!mounted) {
          // Still exercise the pointer path so the DOM evidence exists.
          await driver
            .drag({ x: 400, y: 300 }, { x: 640, y: 440 }, 8)
            .catch(() => undefined);
          dom.afterDrag = await domSnapshot(driver);
          throw new Error(
            "skipped: the app did not render, so there is no canvas to draw on " +
              `(DOM after the drag: #root children=${JSON.stringify(dom.afterDrag.rootChildren)}, ` +
              `canvases=${JSON.stringify(dom.afterDrag.canvases)})`,
          );
        }
        const canvasRect = await driver.queryRect("canvas");
        if (!canvasRect || canvasRect.width < 100 || canvasRect.height < 100) {
          throw new Error(`canvas rect is not laid out: ${JSON.stringify(canvasRect)}`);
        }
        const before = path.join(shotsDir, "excalidraw-before-draw.png");
        const after = path.join(shotsDir, "excalidraw-after-draw.png");
        await driver.screenshot(before);
        await driver.click('[data-testid="toolbar-rectangle"]');
        const x = canvasRect.x + canvasRect.width * 0.3;
        const y = canvasRect.y + canvasRect.height * 0.3;
        const to = { x: x + 240, y: y + 140 };
        await driver.drag({ x, y }, to, 12);
        await driver.nextPresented().catch(() => undefined);
        await driver.nextPresented().catch(() => undefined);
        await driver.screenshot(after);
        dom.afterDrag = await domSnapshot(driver);
        // Only the rectangle's own area: the toolbar and properties panel change
        // on tool selection too, and those are not canvas pixels.
        const diff = diffRegion(before, after, {
          x: x - 20,
          y: y - 20,
          width: to.x - x + 40,
          height: to.y - y + 40,
        });
        if (diff.differingPixels < 200) {
          throw new Error(`the canvas did not change after drawing: ${JSON.stringify(diff)}`);
        }
        return diff;
      },
    );
  } catch (error) {
    if (checks.length === 0) {
      checks.push({ name: "harness", component: "test harness", pass: false, error: error.message });
    }
  } finally {
    stderr = driver?.stderrText?.() ?? "";
    await driver?.close().catch(() => undefined);
  }

  const report = {
    step: "excalidraw on the native runtime",
    app: "vendor/excalidraw v0.18.1",
    binary: repoRelative(repoRoot, binary),
    pack,
    headless: true,
    viewport: { width: 1280, height: 800 },
    runtime: { ...runtime, path: repoRelative(repoRoot, runtime.path) },
    gpuAdapter: adapter,
    vkIcd: effectiveVkIcd,
    durationMs: Number((performance.now() - started).toFixed(2)),
    startup,
    memory: { peakRssBytes: peakRss },
    passed: checks.filter((check) => check.pass).length,
    failed: checks.filter((check) => !check.pass).length,
    checks,
    assets: assetAudit,
    diagnostics: {
      startupErrors,
      consoleErrors: diagnostics.console ?? [],
      apiProbe: diagnostics.api ?? null,
      missingApis,
      dom: diagnostics.dom ?? null,
      moduleErrorsReported: moduleErrorReporting.reported,
      moduleErrorProbe: moduleErrorReporting,
      stderrTail: diagnostics.stderrTail ?? null,
    },
    dom,
    stderrTail: stderr.slice(-4000),
  };
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

function parseCliArgs(argv) {
  const opts = { build: false, adapter: gpuAdapter, env: null, outPath: defaultReportPath };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--build") opts.build = true;
    else if (arg === "--gpu-adapter" && argv[i + 1]) opts.adapter = argv[++i];
    else if (arg === "--out" && argv[i + 1]) opts.outPath = path.resolve(argv[++i]);
    else if (arg === "--unset-vk-icd") opts.env = { ...(opts.env ?? {}), VK_ICD_FILENAMES: null };
  }
  return opts;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const opts = parseCliArgs(process.argv.slice(2));
  const report = await runExcalidrawNative(opts);
  console.log(
    `\n${report.passed}/${report.passed + report.failed} checks passed ` +
      `(${report.durationMs} ms, adapter ${report.gpuAdapter}, ICD ${report.vkIcd ?? "system"}, ` +
      `runtime ${(report.runtime.bytes / 1024 / 1024).toFixed(1)} MiB ${report.runtime.profile}, ` +
      `binary ${(report.pack.binaryBytes / 1024 / 1024).toFixed(1)} MiB, ` +
      `first frame ${report.startup.firstFrameMs ?? "?"} ms, peak RSS ` +
      `${report.memory.peakRssBytes ? (report.memory.peakRssBytes / 1024 / 1024).toFixed(0) + " MiB" : "?"}); ` +
      `module errors reported: ${report.diagnostics.moduleErrorsReported}; ` +
      `missing APIs: ${report.diagnostics.missingApis.map((api) => api.name).join(", ") || "none"}`,
  );
  if (report.diagnostics.startupErrors.length > 0) {
    console.log(`startup exceptions:\n${report.diagnostics.startupErrors.join("\n")}`);
  }
  console.log(`report: ${path.relative(repoRoot, opts.outPath)}`);
  if (report.failed > 0) process.exit(1);
}

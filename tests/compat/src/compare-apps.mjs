#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Cross-engine app compatibility runner:
// Builds an app via `squark build`, runs its shared interaction script on both
// ChromiumDriver and SquarkDriver (headless native binary via --test-control),
// verifies exact DOM state equality at every checkpoint, and compares every
// screenshot at the app screenshot tolerance (maxChannelDelta: 8, maxDiffRatio: 0.005).

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { comparePngBuffers } from "../../reftests/src/compare.mjs";
import { launchable } from "../../excalidraw/src/build.mjs";
import { ChromiumDriver } from "./chromium-driver.mjs";
import { SquarkDriver } from "./squark-driver.mjs";
import { runCounterScript } from "./scripts/counter.mjs";
import { runShadcnScript } from "./scripts/shadcn.mjs";
import { runTodoMvcScript } from "./scripts/todomvc.mjs";
import { runVirtualListScript } from "./scripts/virtual-list.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

/**
 * App interaction screenshot tolerance: at most 0.5% of pixels may differ by
 * more than 8 levels in a channel. DOM state must match exactly.
 */
export const APP_SCREENSHOT_TOLERANCE = Object.freeze({
  maxChannelDelta: 8,
  maxDiffRatio: 0.005,
});

export const REACT_APPS = {
  counter: { dir: "apps/counter", script: runCounterScript },
  todomvc: { dir: "apps/todomvc", script: runTodoMvcScript },
};

/**
 * Interactive apps. They report into tests/results/interactive-apps and run in CI
 * in report-only mode.
 */
export const INTERACTIVE_APPS = {
  shadcn: { dir: "apps/shadcn", script: runShadcnScript },
  "virtual-list": { dir: "apps/virtual-list", script: runVirtualListScript },
};

export const APPS = { ...REACT_APPS, ...INTERACTIVE_APPS };

function defaultSquarkBin() {
  return process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark");
}

function defaultRuntimeBin() {
  return process.env.SQUARK_RUNTIME_BIN || path.join(repoRoot, "target/debug/squark-runtime");
}

export async function compareAppAcrossEngines({
  appName,
  scriptFn = null,
  squarkBin = defaultSquarkBin(),
  runtimeBin = defaultRuntimeBin(),
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto",
  headless = true,
  width = 800,
  height = 600,
  dpr = 1,
  outDir = path.join(repoRoot, "tests/results", appName in INTERACTIVE_APPS ? "interactive-apps" : "react-apps", appName),
  timeoutMs = 30_000,
}) {
  const entry = APPS[appName];
  const runScript = scriptFn ?? entry?.script;
  if (!runScript) {
    throw new Error(`Unknown app ${appName}; provide scriptFn or use one of: ${Object.keys(APPS).join(", ")}`);
  }
  const appDir = path.join(repoRoot, entry ? entry.dir : path.join("apps", appName));
  const buildOutDir = path.join(repoRoot, "target/react-apps", appName);
  fs.mkdirSync(buildOutDir, { recursive: true });
  fs.mkdirSync(outDir, { recursive: true });

  const nativeBinary = path.join(buildOutDir, `${appName}-native`);
  if (fs.existsSync(nativeBinary)) fs.rmSync(nativeBinary);

  // 1. Build the unmodified React app + pack + append to real squark-runtime via `squark build`.
  execFileSync(
    squarkBin,
    ["build", appDir, "--runtime", runtimeBin, "--out", nativeBinary],
    { encoding: "utf8", timeout: 120_000, maxBuffer: 32 * 1024 * 1024 },
  );
  const distDir = path.join(appDir, "dist");
  assert.ok(fs.existsSync(path.join(distDir, "index.html")), `missing ${distDir}/index.html`);

  const modeTag = headless ? "headless" : "windowed";
  const expectedDir = path.join(buildOutDir, `expected-${modeTag}`);
  const actualDir = path.join(outDir, "actual", modeTag);
  const diffDir = path.join(outDir, "diff", modeTag);
  fs.mkdirSync(expectedDir, { recursive: true });
  fs.mkdirSync(actualDir, { recursive: true });

  // 2. Run the interaction script in the built native binary via SquarkDriver first so
  // windowed runs can report the compositor's actual viewport/DPR to Chromium.
  const squarkDriver = await SquarkDriver.launch({
    binaryPath: launchable(nativeBinary).executable,
    width,
    height,
    dpr: headless ? dpr : null,
    gpuAdapter,
    headless,
    timeoutMs,
  });
  let squarkResult = null;
  let squarkError = null;
  let effectiveWidth = width;
  let effectiveHeight = height;
  let effectiveDpr = dpr;
  try {
    if (!headless) {
      await squarkDriver.waitForIdle(2);
      const dims = await squarkDriver.eval(
        `({ width: window.innerWidth, height: window.innerHeight, dpr: window.devicePixelRatio })`,
      );
      if (dims && dims.width > 0 && dims.height > 0 && dims.dpr > 0) {
        effectiveWidth = Math.round(dims.width);
        effectiveHeight = Math.round(dims.height);
        effectiveDpr = dims.dpr;
      }
    }
    squarkResult = await runScript(squarkDriver, { screenshotDir: actualDir });
  } catch (err) {
    squarkError = err?.message || String(err);
  } finally {
    await squarkDriver.close();
  }

  // 3. Run the exact same interaction script in headless Chromium at matching viewport/DPR.
  const chromiumDriver = await ChromiumDriver.launch({
    distDir,
    width: effectiveWidth,
    height: effectiveHeight,
    dpr: effectiveDpr,
    timeoutMs,
  });
  let chromiumResult;
  try {
    chromiumResult = await runScript(chromiumDriver, { screenshotDir: expectedDir });
    assert.deepEqual(chromiumDriver.pageErrors, [], `Chromium page errors in ${appName}`);
    assert.deepEqual(chromiumDriver.consoleErrors, [], `Chromium console errors in ${appName}`);
  } finally {
    await chromiumDriver.close();
  }

  if (squarkError) {
    return {
      app: appName,
      mode: modeTag,
      viewport: { width: effectiveWidth, height: effectiveHeight, dpr: effectiveDpr },
      pass: false,
      error: squarkError,
      checkpointsTotal: chromiumResult.checkpoints.length,
      checkpointsPassed: 0,
      checkpointsExactMatch: false,
      screenshotsTotal: Object.keys(chromiumResult.screenshots).length,
      screenshotsPassed: 0,
      screenshots: [],
    };
  }

  // 4. Verify exact DOM state equality at every checkpoint.
  const checkpointResults = [];
  let checkpointsPassed = 0;
  assert.equal(
    squarkResult.checkpoints.length,
    chromiumResult.checkpoints.length,
    `checkpoint count mismatch in ${appName}`,
  );
  for (let i = 0; i < chromiumResult.checkpoints.length; i++) {
    const exp = chromiumResult.checkpoints[i];
    const act = squarkResult.checkpoints[i];
    let exact = false;
    let reason = null;
    try {
      assert.equal(act.name, exp.name);
      assert.deepEqual(act.state, exp.state);
      exact = true;
      checkpointsPassed++;
    } catch (err) {
      reason = err.message;
    }
    checkpointResults.push({ name: exp.name, pass: exact, ...(reason ? { reason } : {}) });
  }
  const checkpointsExactMatch = checkpointsPassed === chromiumResult.checkpoints.length;

  // 5. Compare every screenshot at the app screenshot tolerance (maxChannelDelta: 8, maxDiffRatio: 0.005).
  const screenshotResults = [];
  let screenshotsPassed = 0;
  for (const [name, expectedPath] of Object.entries(chromiumResult.screenshots)) {
    const actualPath = squarkResult.screenshots[name];
    assert.ok(actualPath && fs.existsSync(actualPath), `missing Squark screenshot for ${name}`);
    const cmp = comparePngBuffers(
      fs.readFileSync(expectedPath),
      fs.readFileSync(actualPath),
      APP_SCREENSHOT_TOLERANCE,
    );
    if (cmp.pass) {
      screenshotsPassed++;
    } else if (cmp.diffPng) {
      fs.mkdirSync(diffDir, { recursive: true });
      fs.writeFileSync(path.join(diffDir, `${name}.png`), cmp.diffPng);
    }
    screenshotResults.push({
      name,
      pass: cmp.pass,
      diffRatio: cmp.diffRatio,
      maxDelta: cmp.maxObservedDelta,
      diffPixels: cmp.diffPixels,
      meanChannelError: cmp.meanChannelError,
      reason: cmp.reason,
    });
  }

  const allScreenshotsPass = screenshotsPassed === screenshotResults.length;
  return {
    app: appName,
    mode: modeTag,
    viewport: { width: effectiveWidth, height: effectiveHeight, dpr: effectiveDpr },
    pass: checkpointsExactMatch && allScreenshotsPass,
    error: null,
    checkpointsTotal: checkpointResults.length,
    checkpointsPassed,
    checkpointsExactMatch,
    checkpoints: checkpointResults,
    screenshotsTotal: screenshotResults.length,
    screenshotsPassed,
    screenshots: screenshotResults,
  };
}

function parseArgs(argv) {
  const opts = {
    apps: ["counter", "todomvc"],
    squarkBin: defaultSquarkBin(),
    runtimeBin: defaultRuntimeBin(),
    gpuAdapter: process.env.SQUARK_GPU_ADAPTER || "auto",
    headless: true,
    strict: true,
    summary: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--apps" && argv[i + 1]) opts.apps = argv[++i].split(",").filter(Boolean);
    else if (a === "--squark-bin" && argv[i + 1]) opts.squarkBin = argv[++i];
    else if (a === "--runtime" && argv[i + 1]) opts.runtimeBin = argv[++i];
    else if (a === "--gpu-adapter" && argv[i + 1]) opts.gpuAdapter = argv[++i];
    else if (a === "--windowed") opts.headless = false;
    else if (a === "--report-only") opts.strict = false;
    else if (a === "--summary" && argv[i + 1]) opts.summary = argv[i + 1];
  }
  return opts;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const opts = parseArgs(process.argv.slice(2));
  const results = [];
  let allPass = true;
  for (const appName of opts.apps) {
    let res;
    try {
      res = await compareAppAcrossEngines({
        appName,
        squarkBin: opts.squarkBin,
        runtimeBin: opts.runtimeBin,
        gpuAdapter: opts.gpuAdapter,
        headless: opts.headless,
      });
    } catch (error) {
      // Report-only runs keep going: a broken app is a result, not an abort.
      res = {
        app: appName,
        mode: opts.headless ? "headless" : "windowed",
        viewport: { width: 0, height: 0, dpr: 0 },
        pass: false,
        error: error?.message || String(error),
        checkpointsTotal: 0,
        checkpointsPassed: 0,
        screenshotsTotal: 0,
        screenshotsPassed: 0,
        screenshots: [],
      };
    }
    results.push(res);
    if (!res.pass) allPass = false;
    const shots = res.screenshots
      .map((s) => `${s.name}:${s.pass ? "PASS" : "FAIL"}(diff=${(s.diffRatio * 100).toFixed(3)}%,maxDelta=${s.maxDelta})`)
      .join(", ");
    console.log(
      `compat/${appName} (${res.mode}, ${opts.gpuAdapter}, ${res.viewport.width}x${res.viewport.height}@${res.viewport.dpr}x): ${res.pass ? "PASS" : "FAIL"} | checkpoints ${res.checkpointsPassed}/${res.checkpointsTotal} | screenshots ${res.screenshotsPassed}/${res.screenshotsTotal} [${shots}]${res.error ? ` | error: ${res.error}` : ""}`,
    );
  }
  if (opts.summary) {
    fs.mkdirSync(path.dirname(opts.summary), { recursive: true });
    fs.writeFileSync(
      opts.summary,
      `${JSON.stringify(
        {
          apps: opts.apps,
          reportOnly: !opts.strict,
          gpuAdapter: opts.gpuAdapter,
          headless: opts.headless,
          tolerance: APP_SCREENSHOT_TOLERANCE,
          generatedAt: new Date().toISOString(),
          results,
        },
        null,
        2,
      )}\n`,
    );
  }
  if (opts.strict && !allPass) {
    process.exit(1);
  }
}

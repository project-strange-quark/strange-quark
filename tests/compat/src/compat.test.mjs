// SPDX-License-Identifier: MIT
// Builds apps/counter and apps/todomvc (unmodified Vite + React 19) and runs
// their interaction scripts in headless Chromium through ChromiumDriver.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";
import { ChromiumDriver } from "./chromium-driver.mjs";
import { APP_SCREENSHOT_TOLERANCE } from "./compare-apps.mjs";
import { runCounterScript } from "./scripts/counter.mjs";
import { runShadcnScript } from "./scripts/shadcn.mjs";
import { runTodoMvcScript } from "./scripts/todomvc.mjs";
import { runVirtualListScript } from "./scripts/virtual-list.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const screenshotDir = path.join(repoRoot, "tests/results/compat/actual");

function buildApp(appName) {
  const appDir = path.join(repoRoot, "apps", appName);
  // Run vite's script with this Node: `.bin/vite` is a `.cmd` shim on Windows,
  // which execFile cannot start.
  const viteBin = path.join(repoRoot, "node_modules/vite/bin/vite.js");
  execFileSync(process.execPath, [viteBin, "build"], {
    cwd: appDir,
    stdio: "pipe",
    timeout: 60_000,
  });
  const distDir = path.join(appDir, "dist");
  assert.equal(fs.existsSync(path.join(distDir, "index.html")), true);
  return distDir;
}

function assertValidPng(filePath, expectedWidth, expectedHeight) {
  assert.equal(fs.existsSync(filePath), true, `missing screenshot ${filePath}`);
  const decoded = PNG.sync.read(fs.readFileSync(filePath));
  assert.equal(decoded.width, expectedWidth);
  assert.equal(decoded.height, expectedHeight);
}

test("apps/counter builds with Vite + React 19 and passes ChromiumDriver script", async () => {
  const distDir = buildApp("counter");
  const driver = await ChromiumDriver.launch({
    distDir,
    width: 800,
    height: 600,
    timeoutMs: 30_000,
  });
  try {
    const result = await runCounterScript(driver, { screenshotDir });
    assert.equal(result.finalCount, "0");
    assert.deepEqual(driver.pageErrors, []);
    assert.deepEqual(driver.consoleErrors, []);
    assertValidPng(result.screenshots["counter-initial"], 800, 600);
    assertValidPng(result.screenshots["counter-after-ops"], 800, 600);
    // Persistent contexts must grant clipboard and fullscreen so Chromium baselines match a normal browser.
    const clip = await driver.eval(`(async () => {
      const readState = (await navigator.permissions.query({ name: "clipboard-read" })).state;
      const writeState = (await navigator.permissions.query({ name: "clipboard-write" })).state;
      await navigator.clipboard.writeText("squark-clip-ok");
      const text = await navigator.clipboard.readText();
      return { readState, writeState, text };
    })()`);
    assert.deepEqual(clip, { readState: "granted", writeState: "granted", text: "squark-clip-ok" });
    await driver.eval(`(() => {
      const btn = document.createElement("button");
      btn.id = "__squark_fs_test";
      btn.style.cssText = "position:fixed;left:0;top:0;width:24px;height:24px;z-index:99999";
      btn.onclick = () => {
        window.__squarkFsPromise = document.documentElement.requestFullscreen().then(async () => {
          const entered = document.fullscreenElement === document.documentElement;
          await document.exitFullscreen();
          return { entered, exited: document.fullscreenElement === null };
        });
      };
      document.body.appendChild(btn);
    })()`);
    await driver.click("#__squark_fs_test");
    const fsRes = await driver.eval("window.__squarkFsPromise");
    assert.deepEqual(fsRes, { entered: true, exited: true });
  } finally {
    await driver.close();
  }
});

test("apps/todomvc builds with Vite + React 19 and passes ChromiumDriver script", async () => {
  const distDir = buildApp("todomvc");
  const driver = await ChromiumDriver.launch({
    distDir,
    width: 800,
    height: 600,
    timeoutMs: 30_000,
  });
  try {
    const result = await runTodoMvcScript(driver, { screenshotDir });
    assert.equal(result.remainingItems, 1);
    assert.deepEqual(driver.pageErrors, []);
    assert.deepEqual(driver.consoleErrors, []);
    assertValidPng(result.screenshots["todomvc-added"], 800, 600);
    assertValidPng(result.screenshots["todomvc-completed"], 800, 600);
    assertValidPng(result.screenshots["todomvc-final"], 800, 600);
  } finally {
    await driver.close();
  }
});

test("apps/shadcn builds with Tailwind v4 + Radix + React Hook Form and passes ChromiumDriver script", async () => {
  const distDir = buildApp("shadcn");
  const driver = await ChromiumDriver.launch({
    distDir,
    width: 800,
    height: 600,
    timeoutMs: 30_000,
  });
  try {
    const result = await runShadcnScript(driver, { screenshotDir });
    assert.equal(result.checkpoints.length, 9);
    assert.deepEqual(driver.pageErrors, []);
    assert.deepEqual(driver.consoleErrors, []);
    assertValidPng(result.screenshots["shadcn-initial"], 800, 600);
    assertValidPng(result.screenshots["shadcn-dialog"], 800, 600);
    assertValidPng(result.screenshots["shadcn-submitted"], 800, 600);
  } finally {
    await driver.close();
  }
});

test("apps/virtual-list builds with TanStack Virtual (10k rows) and passes ChromiumDriver fling script", async () => {
  const distDir = buildApp("virtual-list");
  const driver = await ChromiumDriver.launch({
    distDir,
    width: 800,
    height: 600,
    timeoutMs: 30_000,
  });
  try {
    const result = await runVirtualListScript(driver, { screenshotDir });
    assert.equal(result.checkpoints.length, 2);
    assert.ok(result.frameTimeline.frameCount > 0);
    assert.deepEqual(driver.pageErrors, []);
    assert.deepEqual(driver.consoleErrors, []);
    assertValidPng(result.screenshots["virtual-list-top"], 800, 600);
    assertValidPng(result.screenshots["virtual-list-scrolled"], 800, 600);
  } finally {
    await driver.close();
  }
});

test("the app screenshot tolerance matches specification", () => {
  assert.deepEqual(APP_SCREENSHOT_TOLERANCE, { maxChannelDelta: 8, maxDiffRatio: 0.005 });
});

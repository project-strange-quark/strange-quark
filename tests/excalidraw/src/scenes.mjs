// SPDX-License-Identifier: MIT
// Renders the 10 canonical .excalidraw scenes through the app's own open path
// in Chromium (for baselines/verification) and in the packed native binary
// (headless, through test control), comparing native screenshots against the
// committed Chromium baselines at the app screenshot tolerance.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { comparePngBuffers } from "../../reftests/src/compare.mjs";
import { buildDir, gpuAdapter as defaultGpuAdapter, outDir, packExcalidraw, repoRoot } from "./build.mjs";
import { ChromiumExcalidraw, NativeExcalidraw, sleep } from "./flow/drivers.mjs";
import { elementTypes, waitForScene } from "./flow/state.mjs";
import { canonicalScenes, fixturesDir } from "./fixtures.mjs";
import { platformSuffix, scratchTempDir } from "./platform.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const suffix = platformSuffix();
// The committed Chromium baselines are the Linux reference. Other hosts write
// their renders and summaries beside it, never over it.
export const scenesDir = path.join(repoRoot, "tests/results/excalidraw/scenes");
export const outputDir = path.join(repoRoot, `tests/results/excalidraw/scenes${suffix}`);
export const expectedDir = path.join(scenesDir, "expected");
export const baselineDir = path.join(outputDir, "expected");
export const actualDir = path.join(outputDir, "actual");
export const nativeDir = path.join(outputDir, "native");

// At most 0.5% of pixels may differ by more than 8 levels in a channel.
export const SCREENSHOT_TOLERANCE = Object.freeze({ maxChannelDelta: 8, maxDiffRatio: 0.005 });

let cachedScenes = null;
function ensureFixtures() {
  if (!cachedScenes) {
    cachedScenes = canonicalScenes();
    fs.mkdirSync(fixturesDir, { recursive: true });
    for (const scene of cachedScenes) {
      const file = path.join(fixturesDir, `${scene.id}.excalidraw`);
      if (!fs.existsSync(file)) {
        fs.writeFileSync(file, `${JSON.stringify(scene.data, null, 2)}\n`);
      }
    }
  }
  return cachedScenes;
}

/**
 * Renders every scene in Chromium. With `check: true` it renders to actual/
 * and compares with the committed baselines; otherwise it writes the baselines.
 */
export async function renderScenes({ check = false, timeoutMs = 120_000 } = {}) {
  const scenes = ensureFixtures();
  if (!fs.existsSync(path.join(buildDir, "index.html"))) {
    return {
      status: "not-run",
      reason: `missing ${buildDir}/index.html; run the Excalidraw build`,
      scenes: [],
    };
  }
  if (check) {
    if (!fs.existsSync(path.join(expectedDir, `${scenes[0].id}.png`))) {
      return { status: "not-run", reason: "no committed scene baselines yet", scenes: [] };
    }
  } else {
    fs.mkdirSync(baselineDir, { recursive: true });
  }

  const results = [];
  for (const scene of scenes) {
    const profile = scratchTempDir(repoRoot, "squark-scene-profile-");
    let driver = null;
    try {
      driver = await ChromiumExcalidraw.launch({ buildDir, userDataDir: profile });
      await driver.openFile(path.join(fixturesDir, `${scene.id}.excalidraw`));
      await waitForScene(driver, (state) => elementTypes(state).length === scene.data.elements.length, {
        label: `${scene.id} (${scene.data.elements.length} elements)`,
        timeoutMs,
      });
      // Let fonts, images and the app's own post-load fit settle.
      await sleep(600);
      const actualPath = path.join(actualDir, `${scene.id}.png`);
      const expectedPath = path.join(check ? expectedDir : baselineDir, `${scene.id}.png`);
      await driver.screenshot(check ? actualPath : expectedPath);
      if (check) {
        const cmp = comparePngBuffers(
          fs.readFileSync(expectedPath),
          fs.readFileSync(actualPath),
          SCREENSHOT_TOLERANCE,
        );
        results.push({
          id: scene.id,
          note: scene.note,
          pass: cmp.pass,
          diffRatio: cmp.diffRatio,
          maxDelta: cmp.maxObservedDelta,
          reason: cmp.reason,
        });
      } else {
        results.push({ id: scene.id, note: scene.note, written: path.relative(repoRoot, expectedPath) });
      }
    } catch (error) {
      results.push({ id: scene.id, note: scene.note, pass: false, error: error.message });
    } finally {
      await driver?.close().catch(() => undefined);
      fs.rmSync(profile, { recursive: true, force: true });
    }
  }

  if (!check) {
    return { status: "baselines", total: results.length, scenes: results };
  }
  const passed = results.filter((scene) => scene.pass).length;
  // The committed baselines are Linux renders. Off Linux, Chromium's own font
  // stack can differ, so its comparison is recorded but only the native leg
  // gates the run.
  const informational = process.platform !== "linux";
  return {
    status: informational
      ? "informational"
      : passed === results.length
        ? "passing"
        : "failing",
    ...(informational ? { informational: true } : {}),
    total: results.length,
    passed,
    failed: results.length - passed,
    tolerance: { ...SCREENSHOT_TOLERANCE },
    scenes: results,
  };
}

/**
 * Renders every canonical scene in the packed native Excalidraw binary
 * (headless, through test control's fileDialog.queue open path), screenshots
 * each at 1280×800, and compares against the committed Chromium baselines
 * at the app screenshot tolerance.
 */
async function waitForFontsSettled(driver, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    await driver.driver.nextPresented();
    const status = await driver.eval("document.fonts?.status ?? 'loaded'").catch(() => "loaded");
    if (status !== "loading") {
      return;
    }
    await sleep(50);
  }
}

export async function renderNativeScenes({
  binaryPath = null,
  gpuAdapter = defaultGpuAdapter,
  outputDir = nativeDir,
  skipBuild = false,
  timeoutMs = 120_000,
} = {}) {
  const scenes = ensureFixtures();
  if (!fs.existsSync(path.join(expectedDir, `${scenes[0].id}.png`))) {
    return { status: "not-run", reason: "no committed scene baselines yet", scenes: [] };
  }

  let packedBinary = binaryPath;
  if (!packedBinary) {
    const defaultBinary = path.join(outDir, "excalidraw-native");
    if (skipBuild && fs.existsSync(defaultBinary)) {
      packedBinary = defaultBinary;
    } else {
      if (!fs.existsSync(path.join(buildDir, "index.html"))) {
        return {
          status: "not-run",
          reason: `missing ${buildDir}/index.html; run the Excalidraw build`,
          scenes: [],
        };
      }
      packedBinary = packExcalidraw().binary;
    }
  }

  fs.mkdirSync(outputDir, { recursive: true });
  const results = [];
  for (const scene of scenes) {
    const xdgDataHome = scratchTempDir(repoRoot, "squark-scene-xdg-");
    const actualPath = path.join(outputDir, `${scene.id}.png`);
    const diffPath = path.join(outputDir, `${scene.id}.diff.png`);
    const metaPath = path.join(outputDir, `${scene.id}.json`);
    const expectedPath = path.join(expectedDir, `${scene.id}.png`);
    let driver = null;
    try {
      driver = await NativeExcalidraw.launch({
        binaryPath: packedBinary,
        workDir: outputDir,
        xdgDataHome,
        headless: true,
        gpuAdapter,
        timeoutMs,
      });
      await driver.driver.waitForSelector(".excalidraw", 30_000);
      await waitForFontsSettled(driver);
      await sleep(300);

      await driver.openFile(path.join(fixturesDir, `${scene.id}.excalidraw`));
      await waitForScene(driver, (state) => elementTypes(state).length === scene.data.elements.length, {
        label: `${scene.id} (${scene.data.elements.length} elements)`,
        timeoutMs,
      });
      // Let fonts triggered by canvas text, decoded images and post-load layout/canvas
      // work settle, then advance until fresh frames have been committed and read back.
      await waitForFontsSettled(driver);
      await sleep(400);
      await waitForFontsSettled(driver);
      await driver.driver.nextPresented();
      await driver.driver.nextPresented();

      await driver.screenshot(actualPath);
      const cmp = comparePngBuffers(
        fs.readFileSync(expectedPath),
        fs.readFileSync(actualPath),
        { ...SCREENSHOT_TOLERANCE, generateDiff: true },
      );

      if (!cmp.pass && cmp.diffPng) {
        fs.writeFileSync(diffPath, cmp.diffPng);
      } else {
        fs.rmSync(diffPath, { force: true });
      }

      const entry = {
        id: scene.id,
        note: scene.note,
        pass: cmp.pass,
        diffPixels: cmp.diffPixels,
        totalPixels: cmp.totalPixels,
        diffRatio: cmp.diffRatio,
        maxDelta: cmp.maxObservedDelta,
        meanChannelError: Number(cmp.meanChannelError.toFixed(4)),
        diffBounds: cmp.diffBounds,
        reason: cmp.reason,
        actual: path.relative(repoRoot, actualPath),
        expected: path.relative(repoRoot, expectedPath),
        diff: !cmp.pass && cmp.diffPng ? path.relative(repoRoot, diffPath) : null,
      };
      fs.writeFileSync(metaPath, `${JSON.stringify(entry, null, 2)}\n`);
      results.push(entry);
    } catch (error) {
      const entry = {
        id: scene.id,
        note: scene.note,
        pass: false,
        error: error.message,
      };
      fs.writeFileSync(metaPath, `${JSON.stringify(entry, null, 2)}\n`);
      results.push(entry);
    } finally {
      await driver?.close().catch(() => undefined);
      fs.rmSync(xdgDataHome, { recursive: true, force: true });
    }
  }

  const passed = results.filter((scene) => scene.pass).length;
  return {
    status: passed === results.length ? "passing" : "failing",
    gpuAdapter,
    vkIcd: process.env.VK_ICD_FILENAMES ?? null,
    total: results.length,
    passed,
    failed: results.length - passed,
    tolerance: { ...SCREENSHOT_TOLERANCE },
    scenes: results,
  };
}

function argValue(flag) {
  const index = process.argv.indexOf(flag);
  return index !== -1 && index + 1 < process.argv.length ? process.argv[index + 1] : null;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const check = process.argv.includes("--check");
  const nativeOnly = process.argv.includes("--native");
  const chromiumOnly = process.argv.includes("--chromium");
  const skipBuild = process.argv.includes("--skip-build");
  const adapter = argValue("--gpu-adapter") ?? defaultGpuAdapter;
  const customOutDir = argValue("--out-dir");
  const customSummaryOut = argValue("--summary-out");

  const summaryPath = customSummaryOut
    ? path.resolve(customSummaryOut)
    : path.join(outputDir, "summary.json");
  fs.mkdirSync(path.dirname(summaryPath), { recursive: true });
  const existing = fs.existsSync(summaryPath) ? JSON.parse(fs.readFileSync(summaryPath, "utf8")) : {};

  let chromiumResult = existing;
  if (!nativeOnly) {
    chromiumResult = await renderScenes({ check });
  }

  let nativeResult = existing.native ?? null;
  if (nativeOnly || (check && !chromiumOnly)) {
    nativeResult = await renderNativeScenes({
      gpuAdapter: adapter,
      outputDir: customOutDir ? path.resolve(customOutDir) : nativeDir,
      skipBuild,
    });
  }

  const summary = nativeOnly
    ? { ...existing, native: nativeResult }
    : { check, ...chromiumResult, native: nativeResult };
  fs.writeFileSync(summaryPath, `${JSON.stringify(summary, null, 2)}\n`);

  console.log(
    JSON.stringify({
      check: check || nativeOnly,
      status: chromiumResult.status ?? null,
      passed: chromiumResult.passed ?? null,
      failed: chromiumResult.failed ?? null,
      total: chromiumResult.total ?? null,
      native: nativeResult
        ? {
            status: nativeResult.status,
            gpuAdapter: nativeResult.gpuAdapter,
            passed: nativeResult.passed,
            failed: nativeResult.failed,
            total: nativeResult.total,
          }
        : null,
    }),
  );

  if (nativeOnly) {
    if (nativeResult.status !== "passing") process.exit(1);
  } else if (check) {
    if (process.platform === "linux") {
      if (chromiumResult.status !== "passing") process.exit(1);
    } else if (!nativeOnly) {
      console.log(
        `note: fresh Chromium vs the committed Linux baseline is informational on ` +
          `${process.platform} (${chromiumResult.passed ?? 0}/${chromiumResult.total ?? 0}); ` +
          "the native leg decides the result",
      );
    }
    if (!chromiumOnly && nativeResult?.status !== "passing") process.exit(1);
  }
}

// SPDX-License-Identifier: MIT
// The end-to-end harness. It packs the app once, runs the scripted
// flow in Chromium (the reference) and in the native binary (gating), compares
// the DOM and scene checkpoint state against Chromium at every step, and writes
// tests/results/excalidraw/e2e.json (or e2e-windowed.json).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  buildDir,
  gpuAdapter,
  inspectPack,
  packExcalidraw,
  repoRoot,
  resolveRuntime,
} from "./build.mjs";
import { ChromiumExcalidraw, NativeExcalidraw } from "./flow/drivers.mjs";
import { runFlow } from "./flow/steps.mjs";
import { compareCheckpoints } from "./flow/state.mjs";
import { tinyPngBase64 } from "./fixtures.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const resultsDir = path.join(repoRoot, "tests/results/excalidraw");
export const artifactsDir = path.join(repoRoot, "target/excalidraw-e2e");

/**
 * The report file for a run. Linux keeps the release names; other platforms
 * add their name, so a macOS or Windows run never replaces Linux evidence.
 */
/** Recorded results name repository paths relative to the checkout. */
function repoRelative(file) {
  const relative = path.relative(repoRoot, file);
  return relative.startsWith("..") || path.isAbsolute(relative) ? file : relative.split(path.sep).join("/");
}

export function reportName(windowed, platform = process.platform) {
  const base = windowed ? "e2e-windowed" : "e2e";
  const suffix = { linux: "", darwin: "-macos", win32: "-windows" }[platform] ?? `-${platform}`;
  return `${base}${suffix}.json`;
}

function context(paths) {
  return {
    fs,
    savedFile: paths.savedFile,
    exportedPng: paths.exportedPng,
    exportedSvg: paths.exportedSvg,
    clipboardPng: paths.clipboardPng,
  };
}

function prepareArtifacts() {
  fs.rmSync(artifactsDir, { recursive: true, force: true });
  fs.mkdirSync(artifactsDir, { recursive: true });
  const clipboardPng = path.join(artifactsDir, "clipboard.png");
  fs.writeFileSync(clipboardPng, Buffer.from(tinyPngBase64(), "base64"));
  return {
    clipboardPng,
    // Recreated per engine so the Chromium run's file never satisfies the
    // native run's assertion.
    savedFile: path.join(artifactsDir, "scene.excalidraw"),
    exportedPng: path.join(artifactsDir, "export.png"),
    exportedSvg: path.join(artifactsDir, "export.svg"),
  };
}

function resetRunPaths(paths, engine) {
  for (const key of ["savedFile", "exportedPng", "exportedSvg"]) {
    fs.rmSync(paths[key], { force: true });
    paths[key] = path.join(artifactsDir, `${engine}-${path.basename(paths[key])}`);
  }
}

export async function runE2E({ build = false, skipBuild = false, windowed = process.env.SQUARK_WINDOW_TESTS === "1" } = {}) {
  fs.mkdirSync(resultsDir, { recursive: true });
  if (build) {
    const { buildWithYarn } = await import("./build.mjs");
    buildWithYarn();
  }
  const paths = prepareArtifacts();
  let packaged = null;
  if (!skipBuild) {
    packaged = packExcalidraw();
  }

  const report = {
    step: "end-to-end harness",
    app: "vendor/excalidraw v0.18.1",
    viewport: { width: 1280, height: 800 },
    windowed,
    gpuAdapter,
    vkIcd: process.env.VK_ICD_FILENAMES ?? null,
    engines: {},
  };
  if (packaged) {
    report.pack = inspectPack(packaged.binary);
    report.runtime = { path: repoRelative(packaged.runtime.path), profile: packaged.runtime.profile, bytes: packaged.runtime.bytes };
  } else {
    const runtime = resolveRuntime();
    report.runtime = { ...runtime, path: repoRelative(runtime.path) };
  }

  // 1. Chromium: the authoring target. Every step must pass here.
  resetRunPaths(paths, "chromium");
  let chromiumDriver = null;
  const chromiumStarted = performance.now();
  try {
    chromiumDriver = await ChromiumExcalidraw.launch({ buildDir });
    report.engines.chromium = await runFlow(chromiumDriver, {
      engine: "chromium",
      ctx: context(paths),
    });
    report.engines.chromium.consoleLog = chromiumDriver.consoleLog.slice(-20);
  } catch (error) {
    report.engines.chromium = {
      engine: "chromium",
      launchError: error.message,
      passed: 0,
      total: 0,
      steps: [],
    };
  } finally {
    await chromiumDriver?.close().catch(() => undefined);
  }
  report.engines.chromium.durationMs = Number((performance.now() - chromiumStarted).toFixed(2));

  // 2. Native: the same flow through test control (gating).
  resetRunPaths(paths, "native");
  let nativeDriver = null;
  const nativeStarted = performance.now();
  try {
    nativeDriver = await NativeExcalidraw.launch({
      binaryPath: packaged?.binary ?? path.join(artifactsDir, "missing-binary"),
      workDir: artifactsDir,
      headless: !windowed,
      gpuAdapter,
    });
    report.engines.native = await runFlow(nativeDriver, {
      engine: "native",
      ctx: context(paths),
    });
    // Evidence for test-control click counts: the WYSIWYG double click succeeds through
    // clickCount 1 then 2 even when the presses are far apart in wall time.
    report.engines.native.doubleClick = nativeDriver.lastDoubleClick ?? null;
    report.engines.native.clipboard = nativeDriver.lastClipboard ?? null;
    report.engines.native.ime = nativeDriver.lastIme ?? null;
  } catch (error) {
    report.engines.native = {
      engine: "native",
      launchError: error.message,
      passed: 0,
      failed: 0,
      blocked: 0,
      total: 0,
      steps: [],
    };
  } finally {
    await nativeDriver?.close().catch(() => undefined);
  }
  report.engines.native.durationMs = Number((performance.now() - nativeStarted).toFixed(2));

  // The flow is identical in both engines: same ids, same order, and matching
  // DOM + persisted scene checkpoint state at every step.
  const chromiumIds = report.engines.chromium.steps.map((step) => step.id);
  const nativeIds = report.engines.native.steps.map((step) => step.id);
  report.sameFlow = JSON.stringify(chromiumIds) === JSON.stringify(nativeIds);
  report.domComparison = compareCheckpoints(
    report.engines.chromium.steps,
    report.engines.native.steps,
    { checkCanvasSize: !windowed },
  );
  report.chromiumStatus =
    report.engines.chromium.failed === 0 &&
    report.engines.chromium.blocked === 0 &&
    report.engines.chromium.total > 0 &&
    !report.engines.chromium.launchError
      ? "passing"
      : "failing";
  report.nativeMode = "gating";
  report.nativeStatus =
    report.engines.native.failed === 0 &&
    report.engines.native.blocked === 0 &&
    report.engines.native.total > 0 &&
    !report.engines.native.launchError &&
    report.sameFlow &&
    report.domComparison.passed
      ? "passing"
      : "failing";

  fs.writeFileSync(
    path.join(resultsDir, reportName(windowed)),
    `${JSON.stringify(report, null, 2)}\n`,
  );
  return report;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = await runE2E({ build: process.argv.includes("--build"), skipBuild: process.argv.includes("--skip-build") });
  const c = report.engines.chromium;
  const n = report.engines.native;
  const d = report.domComparison;
  console.log(
    `Excalidraw e2e (${report.windowed ? "windowed" : "headless"}): Chromium ${c.passed}/${c.total} steps passed (${report.chromiumStatus}` +
      `${c.launchError ? `, launch error: ${c.launchError}` : ""}); ` +
      `native ${n.passed}/${n.total} steps passed (${report.nativeStatus}` +
      `${n.launchError ? `, launch error: ${n.launchError.split("\n")[0]}` : ""}); ` +
      `DOM checkpoints ${d.matched}/${d.total} matched`,
  );
  for (const step of c.steps.filter((entry) => entry.status !== "pass")) {
    console.log(`  chromium ${step.status} ${step.id}: ${step.error ?? step.reason ?? ""}`);
  }
  for (const step of n.steps.filter((entry) => entry.status !== "pass")) {
    console.log(`  native ${step.status} ${step.id}: ${step.error ?? step.reason ?? ""}`);
  }
  for (const mismatch of d.mismatches) {
    console.log(`  dom mismatch ${mismatch.id}: ${JSON.stringify(mismatch.diffs ?? mismatch)}`);
  }
  console.log(`report: tests/results/excalidraw/${reportName(report.windowed)}`);
  if (report.chromiumStatus !== "passing" || report.nativeStatus !== "passing") process.exit(1);
}

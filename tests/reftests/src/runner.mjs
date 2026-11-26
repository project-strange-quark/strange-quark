// SPDX-License-Identifier: MIT
// Orchestrates reftest runs across Chromium reference rendering, squark screenshot
// rendering, and pixel comparison under tests/results/<suite>/.

import { execFile } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { renderCasesInChromium } from "./chromium.mjs";
import { comparePngBuffers } from "./compare.mjs";
import { discoverCases } from "./manifest.mjs";

const execFileAsync = promisify(execFile);
const require = createRequire(import.meta.url);

function resolveLibFile(lib) {
  if (lib.resolve === "import") {
    return fileURLToPath(import.meta.resolve(lib.package));
  }
  const packageJson = require.resolve(`${lib.package}/package.json`);
  return path.join(path.dirname(packageJson), lib.file);
}

/**
 * Copies a suite that needs npm-sourced browser libraries (R3's rough.js and
 * perfect-freehand) under `target/reftests/staging`, so the third-party code
 * stays out of git and both Chromium and `squark screenshot` see a self-contained
 * tree with the `_assets/lib` files the cases import.
 *
 * Suites without a `_libs.json` are used in place.
 */
function stageSuite(casesRoot, suite, resultsRoot) {
  const suiteDir = path.join(casesRoot, suite);
  const libsPath = path.join(suiteDir, "_libs.json");
  if (!fs.existsSync(libsPath)) return casesRoot;

  const { libs = [] } = JSON.parse(fs.readFileSync(libsPath, "utf8"));
  const stagingRoot = path.resolve(resultsRoot, "../../target/reftests/staging/cases");
  const stagedSuite = path.join(stagingRoot, suite);
  fs.rmSync(stagedSuite, { recursive: true, force: true });
  fs.mkdirSync(path.dirname(stagedSuite), { recursive: true });
  fs.cpSync(suiteDir, stagedSuite, { recursive: true });
  for (const lib of libs) {
    const source = resolveLibFile(lib);
    const target = path.join(stagedSuite, lib.target);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.copyFileSync(source, target);
  }
  return stagingRoot;
}

async function renderCaseInSquark(testCase, outPath, { squarkBin, runtime, gpuAdapter, timeoutMs, env = null }) {
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  const args = [
    "screenshot",
    testCase.filePath,
    "--out",
    outPath,
    "--width",
    String(testCase.meta.width),
    "--height",
    String(testCase.meta.height),
    "--dpr",
    String(testCase.meta.dpr),
    "--timeout",
    String(Math.max(1, Math.ceil(timeoutMs / 1000))),
  ];
  if (gpuAdapter) {
    args.push("--gpu-adapter", gpuAdapter);
  }
  if (runtime) {
    args.push("--runtime", runtime);
  }
  const xdgDir = fs.mkdtempSync(path.join(os.tmpdir(), "squark-reftest-xdg-"));
  try {
    const childEnv = { ...process.env, XDG_DATA_HOME: xdgDir };
    for (const [key, value] of Object.entries(env ?? {})) {
      if (value === null) delete childEnv[key];
      else childEnv[key] = value;
    }
    // A .mjs squarkBin (a test double) runs with this Node: Windows cannot
    // start scripts directly.
    const [command, commandArgs] = squarkBin.endsWith(".mjs")
      ? [process.execPath, [squarkBin, ...args]]
      : [squarkBin, args];
    await execFileAsync(command, commandArgs, {
      timeout: timeoutMs + 5_000,
      env: childEnv,
    });
  } finally {
    fs.rmSync(xdgDir, { recursive: true, force: true });
  }
}

/**
 * Runs a reftest suite in one of three modes:
 * - "chromium": renders expected PNGs into <resultsRoot>/<suite>/expected/
 * - "squark": renders actual PNGs via `squark screenshot` into .../actual/
 * - "compare": renders actual (and expected if missing) and compares them
 */
export async function runReftests({
  casesRoot,
  resultsRoot,
  suite,
  mode = "chromium",
  filter = null,
  squarkBin = "squark",
  runtime = null,
  gpuAdapter = null,
  env = null,
  timeoutMs = 30_000,
}) {
  // R3 stages npm-sourced libraries into target/; every other suite runs in place.
  const effectiveCasesRoot = stageSuite(casesRoot, suite, resultsRoot);
  const cases = discoverCases(effectiveCasesRoot, suite, filter);
  const suiteResultsDir = path.join(resultsRoot, suite);
  const expectedDir = path.join(suiteResultsDir, "expected");
  const actualDir = path.join(suiteResultsDir, "actual");
  const diffDir = path.join(suiteResultsDir, "diff");

  fs.mkdirSync(suiteResultsDir, { recursive: true });

  if (mode === "chromium") {
    const rawRendered = await renderCasesInChromium(cases, effectiveCasesRoot, expectedDir, {
      timeoutMs,
    });
    const geometryMap = {};
    const rendered = rawRendered.map((entry) => {
      if (entry.geometry) {
        geometryMap[entry.id] = {
          width: entry.width,
          height: entry.height,
          dpr: entry.dpr,
          elements: entry.geometry,
        };
      }
      return {
        id: entry.id,
        outPath: path.relative(resultsRoot, entry.outPath).replace(/\\/g, "/"),
        width: entry.width,
        height: entry.height,
        dpr: entry.dpr,
      };
    });
    if (Object.keys(geometryMap).length > 0) {
      fs.writeFileSync(
        path.join(suiteResultsDir, "geometry.json"),
        JSON.stringify({ suite, cases: geometryMap }, null, 2) + "\n",
      );
    }
    const summary = {
      suite,
      mode,
      total: rendered.length,
      rendered,
    };
    fs.writeFileSync(
      path.join(suiteResultsDir, "summary.json"),
      JSON.stringify(summary, null, 2) + "\n",
    );
    return summary;
  }

  const squarkOpts = { squarkBin, runtime, gpuAdapter, timeoutMs, env };

  if (mode === "squark") {
    const rendered = [];
    for (const c of cases) {
      const outPath = path.join(actualDir, `${c.id}.png`);
      await renderCaseInSquark(c, outPath, squarkOpts);
      rendered.push({ id: c.id, outPath });
    }
    return { suite, mode, total: rendered.length, rendered };
  }

  if (mode === "compare") {
    const missingExpected = cases.filter(
      (c) => !fs.existsSync(path.join(expectedDir, `${c.id}.png`)),
    );
    if (missingExpected.length > 0) {
      await renderCasesInChromium(missingExpected, effectiveCasesRoot, expectedDir, { timeoutMs });
    }

    const entries = [];
    let passed = 0;
    let failed = 0;
    let renderErrors = 0;

    for (const c of cases) {
      const expectedPath = path.join(expectedDir, `${c.id}.png`);
      const actualPath = path.join(actualDir, `${c.id}.png`);
      try {
        await renderCaseInSquark(c, actualPath, squarkOpts);
      } catch (err) {
        failed++;
        renderErrors++;
        const detail = err?.stderr?.trim() || err?.message || String(err);
        entries.push({
          id: c.id,
          pass: false,
          diffRatio: null,
          maxDelta: null,
          diffPixels: null,
          meanChannelError: null,
          diffBounds: null,
          reason: `render error: ${detail}`,
        });
        continue;
      }

      const cmp = comparePngBuffers(
        fs.readFileSync(expectedPath),
        fs.readFileSync(actualPath),
        c.meta,
      );

      if (cmp.pass) {
        passed++;
      } else {
        failed++;
        if (cmp.diffPng) {
          const diffPath = path.join(diffDir, `${c.id}.png`);
          fs.mkdirSync(path.dirname(diffPath), { recursive: true });
          fs.writeFileSync(diffPath, cmp.diffPng);
        }
      }

      entries.push({
        id: c.id,
        pass: cmp.pass,
        diffRatio: cmp.diffRatio,
        maxDelta: cmp.maxObservedDelta,
        diffPixels: cmp.diffPixels,
        meanChannelError: cmp.meanChannelError,
        diffBounds: cmp.diffBounds,
        reason: cmp.reason,
      });
    }

    const summary = {
      suite,
      mode,
      gpuAdapter: gpuAdapter || "auto",
      total: cases.length,
      passed,
      failed,
      renderErrors,
      passRate: cases.length === 0 ? 1 : passed / cases.length,
      cases: entries,
    };
    fs.writeFileSync(
      path.join(suiteResultsDir, "summary.json"),
      JSON.stringify(summary, null, 2) + "\n",
    );
    return summary;
  }

  throw new Error(`Unknown reftest mode: ${mode}`);
}

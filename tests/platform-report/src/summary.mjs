// SPDX-License-Identifier: MIT
// Writes the platform report to tests/results/platform/summary.json.
//
// Collects the platform numbers: the Canvas 2D WPT subset against Chromium
// and natively, R3 in compare mode, and the Excalidraw native checks. A native
// leg that cannot run is recorded as blocked rather than skipped.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { findChromiumPath, chromiumVersion, pinnedChromium } from "../../../scripts/chromium-path.mjs";
import { runReftests } from "../../reftests/src/runner.mjs";
import { WPT_REVISION, runCanvasWpt } from "../../wpt/canvas/run.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(__dirname, "../../..");

const suites = {
  cases: path.join(repoRoot, "tests/reftests/cases"),
  results: path.join(repoRoot, "tests/results"),
};
const defaultSquark = path.join(repoRoot, "target/debug/squark");
const defaultRuntime = path.join(repoRoot, "target/debug/squark-runtime");

function submoduleCommit() {
  try {
    return execFileSync("git", ["-C", path.join(repoRoot, "vendor/excalidraw"), "rev-parse", "HEAD"], {
      encoding: "utf8",
      timeout: 30_000,
    }).trim();
  } catch {
    return null;
  }
}

function findLavapipeIcd() {
  for (const dir of ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"]) {
    if (!fs.existsSync(dir)) continue;
    const match = fs
      .readdirSync(dir)
      .sort()
      .find((entry) => entry.includes("lvp") && entry.endsWith(".json"));
    if (match) return path.join(dir, match);
  }
  return null;
}

/** Reads an Excalidraw native smoke report, when the harness has run. */
export function excalidrawStatus(reportRelPath = "platform/excalidraw-native.json") {
  const reportPath = path.join(suites.results, reportRelPath);
  if (!fs.existsSync(reportPath)) {
    return { status: "not-run", reason: `tests/results/${reportRelPath} is missing` };
  }
  const report = JSON.parse(fs.readFileSync(reportPath, "utf8"));
  const blocked = report.startupError?.includes("Canvas 2D backend");
  return {
    status: report.failed === 0 ? "passing" : blocked ? "blocked" : "failing",
    gpuAdapter: report.gpuAdapter ?? null,
    vkIcd: report.vkIcd ?? null,
    passed: report.passed,
    failed: report.failed,
    checks: report.checks.map((check) => ({
      name: check.name,
      pass: check.pass,
      component: check.component,
      ...(check.error ? { error: check.error } : {}),
    })),
    startupError: report.startupError ?? null,
    runtime: report.runtime ?? null,
    startup: report.startup ?? null,
    memory: report.memory ?? null,
    pack: report.pack ?? null,
    submodule: submoduleCommit(),
  };
}

/**
 * Runs the R3 canvas suite against the Chromium baselines in compare mode.
 * Report-only: the numbers land in the summary whether or not they pass.
 */
export async function r3Status({
  squarkBin = process.env.SQUARK_BIN || defaultSquark,
  runtimeBin = process.env.SQUARK_RUNTIME_BIN || defaultRuntime,
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto",
  env = null,
} = {}) {
  if (!fs.existsSync(squarkBin) || !fs.existsSync(runtimeBin)) {
    return {
      status: "not-run",
      reason: `missing ${fs.existsSync(squarkBin) ? "runtime" : "squark"} binary; build the workspace first`,
    };
  }
  try {
    const summary = await runReftests({
      casesRoot: suites.cases,
      // Keep the committed tests/results/r3-canvas/summary.json as the
      // Chromium baseline record; compare artefacts go under target/, one
      // directory per adapter so both runs can coexist.
      resultsRoot: path.join(repoRoot, "target/platform-report", `r3-${gpuAdapter}`),
      suite: "r3-canvas",
      mode: "compare",
      squarkBin,
      runtimeBin,
      gpuAdapter,
      env,
    });
    const failed = summary.failed ?? 0;
    const renderErrors = summary.renderErrors ?? 0;
    return {
      status: renderErrors > 0 ? "blocked" : failed > 0 ? "failing" : "passing",
      total: summary.total,
      passed: summary.passed,
      failed,
      renderErrors,
      passRate: summary.passRate,
      gpuAdapter: summary.gpuAdapter,
      vkIcd:
        env && Object.hasOwn(env, "VK_ICD_FILENAMES")
          ? env.VK_ICD_FILENAMES
          : process.env.VK_ICD_FILENAMES ?? null,
      cases: (summary.cases ?? []).map((entry) => ({
        id: entry.id,
        pass: entry.pass,
        diffRatio: entry.diffRatio,
        maxDelta: entry.maxDelta,
        ...(entry.reason ? { reason: entry.reason } : {}),
      })),
    };
  } catch (error) {
    return { status: "error", error: error.message };
  }
}

function summarizeNativeWpt(result) {
  return {
    status: result.status === "blocked" ? "blocked" : result.failed === 0 ? "passing" : "failing",
    ...(result.status === "blocked"
      ? { reason: result.reason }
      : {
          total: result.total,
          passed: result.passed,
          failed: result.failed,
          passRate: result.passRate,
        }),
  };
}

function verdict(report) {
  const native = report.canvasWpt.native;
  if (native.status !== "passing") return "blocked";
  if (report.canvasWpt.nativeByAdapter?.auto?.status !== "passing") return "blocked";
  if (report.r3.status !== "passing") return "blocked";
  if (report.r3ByAdapter?.auto?.status !== "passing") return "blocked";
  if (report.excalidraw.status !== "passing") return "blocked";
  if (
    report.excalidrawByAdapter?.auto &&
    report.excalidrawByAdapter.auto.status !== "not-run" &&
    report.excalidrawByAdapter.auto.status !== "passing"
  ) {
    return "blocked";
  }
  return "passing";
}

/**
 * Generates the report. `canvasChromium` may be supplied by tests to avoid a
 * second browser run.
 */
export async function generatePlatformSummary({
  outPath = path.join(suites.results, "platform/summary.json"),
  canvasChromium = null,
  squarkBin,
  runtimeBin,
  gpuAdapter,
} = {}) {
  const chromium = canvasChromium ?? (await runCanvasWpt({ engine: "chromium" }));
  // Both adapters: a hardware driver (auto) and lavapipe (software) can be
  // installed side by side. Pin the ICD per launch so a lavapipe shell cannot
  // silently turn the hardware leg into a second software run.
  const lvpIcd = findLavapipeIcd();
  const softwareEnv = lvpIcd ? { VK_ICD_FILENAMES: lvpIcd } : null;
  const hardwareEnv = { VK_ICD_FILENAMES: null };
  const [nativeSoftware, nativeHardware] = await Promise.all([
    runCanvasWpt({ engine: "native", squarkBin, runtimeBin, gpuAdapter: "software", env: softwareEnv }),
    runCanvasWpt({ engine: "native", squarkBin, runtimeBin, gpuAdapter: "auto", env: hardwareEnv }),
  ]);
  const native = nativeSoftware;
  // R3 stages its cases in one shared directory, so the two adapters run
  // sequentially rather than racing on stageSuite's copy.
  const r3Software = await r3Status({ squarkBin, runtimeBin, gpuAdapter: "software", env: softwareEnv });
  const r3Hardware = await r3Status({ squarkBin, runtimeBin, gpuAdapter: "auto", env: hardwareEnv });
  const excalidrawSoftware = excalidrawStatus("platform/excalidraw-native.json");
  const excalidrawHardware = excalidrawStatus("platform/excalidraw-native-auto.json");
  const report = {
    goal: "Excalidraw's platform: Canvas 2D on Vello, fonts, storage, crypto, clipboard, files, wasm",
    gpu: {
      adapters: { software: "lavapipe", auto: "hardware" },
      software: { adapter: "software", vkIcd: softwareEnv?.VK_ICD_FILENAMES ?? null },
      auto: { adapter: "auto", vkIcd: null },
    },
    referenceChromium: {
      path: findChromiumPath(),
      version: chromiumVersion(),
      playwright: pinnedChromium(),
    },
    canvasWpt: {
      revision: WPT_REVISION,
      chromium: {
        status: chromium.failed === 0 ? "passing" : "failing",
        total: chromium.total,
        passed: chromium.passed,
        failed: chromium.failed,
        passRate: chromium.passRate,
        subtestsTotal: chromium.subtestsTotal,
        subtestsPassed: chromium.subtestsPassed,
      },
      native: summarizeNativeWpt(native),
      nativeByAdapter: {
        software: summarizeNativeWpt(nativeSoftware),
        auto: summarizeNativeWpt(nativeHardware),
      },
    },
    r3: r3Software,
    r3ByAdapter: { software: r3Software, auto: r3Hardware },
    excalidraw: excalidrawSoftware,
    excalidrawByAdapter: {
      software: excalidrawSoftware,
      auto: excalidrawHardware,
    },
    checks: {
      checkSh: process.env.SQUARK_CHECK_SH_RESULT || "not-recorded",
      lavapipe: process.env.SQUARK_LAVAPIPE_RESULT || "not-recorded",
    },
    generatedBy: "tests/platform-report/src/summary.mjs",
  };
  report.verdict = verdict(report);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = await generatePlatformSummary();
  const c = report.canvasWpt;
  console.log(
    `Platform summary: verdict ${report.verdict}; canvas WPT ${c.chromium.passed}/${c.chromium.total} ` +
      `(Chromium), native ${c.native.status}; R3 ${report.r3.status}` +
      `${report.r3.total ? ` (${report.r3.passed}/${report.r3.total}, ${report.r3.renderErrors} render errors)` : ""}; ` +
      `Excalidraw ${report.excalidraw.status}`,
  );
  console.log("report: tests/results/platform/summary.json");
}

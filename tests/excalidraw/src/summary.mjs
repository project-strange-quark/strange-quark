// SPDX-License-Identifier: MIT
// Writes the Excalidraw release report to tests/results/excalidraw/summary.json.
//
// Folds in every Excalidraw check from tests/results/excalidraw/:
//   - packaging (packaging.mjs -> packaging.json): no patches, single
//     stripped x86-64 binary, system libraries only, isolated startup smoke;
//   - startup (startup.json): process start to first presented frame of the
//     empty scene, median of 5 windowed runs on the dist runtime (< 1,500 ms);
//   - e2e (e2e.json headless lavapipe, e2e-windowed.json windowed RADV): 20/20
//     steps matching Chromium;
//   - scenes (scenes/summary.json): 10 canonical scenes matching Chromium at the app screenshot tolerance;
//   - pan (pan.json): 1,000-element pan on the host frame timeline (>= 95% fresh
//     commits meeting the commit deadline);
//   - checks: scripts/check.sh and scripts/check-lavapipe.sh.
//
// The verdict is "passing" only when every native leg and both checks pass.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { findChromiumPath, chromiumVersion, pinnedChromium } from "../../../scripts/chromium-path.mjs";
import { repoRoot } from "./build.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const resultsDir = path.join(repoRoot, "tests/results/excalidraw");

function readJsonIfExists(filePath) {
  if (!fs.existsSync(filePath)) return null;
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

// The release verdict reads only the Linux release names. macOS and Windows
// write packaging-macos.json / packaging-windows.json and scenes-*/ so their
// host-specific renders never replace or enter the Linux evidence.
function summarizePackaging(raw) {
  if (!raw) {
    return { status: "not-run", reason: "tests/results/excalidraw/packaging.json is missing" };
  }
  const noPatches = Boolean(raw.submodule?.clean && raw.submodule?.matchesTag);
  const oneBinary = Boolean(
    raw.binary?.regularFile &&
      raw.binary?.executable &&
      raw.binary?.class === "ELF64" &&
      raw.binary?.stripped &&
      Array.isArray(raw.binary?.isolatedDirectoryContents) &&
      raw.binary.isolatedDirectoryContents.length === 1,
  );
  const systemLibsOnly = Boolean(raw.libraries?.systemOnly && raw.libraries?.commonPackagesOnly);
  const isolatedSmoke = Boolean(raw.smoke?.ran && raw.smoke?.copiedBinaryOnly && raw.smoke?.appMounted);
  const passed = Boolean(raw.pass) && noPatches && oneBinary && systemLibsOnly && isolatedSmoke;
  return {
    status: passed ? "passing" : "failing",
    passed,
    source: "tests/results/excalidraw/packaging.json",
    expectedTag: raw.expectedTag ?? null,
    submoduleCommit: raw.submodule?.commit ?? null,
    noPatches,
    oneBinary,
    stripped: Boolean(raw.binary?.stripped),
    sizeBytes: raw.binary?.sizeBytes ?? null,
    sizeMiB: raw.binary?.sizeMiB ?? null,
    sizeLimitBytes: raw.binary?.sizeLimitBytes ?? null,
    xz9Bytes: raw.binary?.xz9Bytes ?? null,
    zstd19Bytes: raw.binary?.zstd19Bytes ?? null,
    systemLibsOnly,
    libraryCount: Array.isArray(raw.libraries?.resolved) ? raw.libraries.resolved.length : null,
    isolatedSmoke,
  };
}

function summarizeStartup(raw) {
  if (!raw) {
    return { status: "not-run", reason: "tests/results/excalidraw/startup.json is missing" };
  }
  if (raw.status !== "measured") {
    return { status: raw.status ?? "not-run", reason: raw.reason ?? null };
  }
  const passed = Boolean(raw.passed) && typeof raw.medianMs === "number" && raw.medianMs < (raw.limitMs ?? 1500);
  return {
    status: passed ? "passing" : "failing",
    passed,
    source: "tests/results/excalidraw/startup.json",
    adapter: raw.adapter ?? null,
    runs: raw.runs ?? 0,
    medianMs: raw.medianMs ?? null,
    samplesMs: Array.isArray(raw.samples) ? raw.samples.map((s) => s.ms) : [],
    limitMs: raw.limitMs ?? 1500,
    cpuWeight: raw.cpuWeight ?? null,
    loadStart: raw.loadStart ?? null,
    loadEnd: raw.loadEnd ?? null,
    runtime: raw.runtime ?? null,
    binary: raw.binary ?? null,
    v8CodeCache: raw.v8CodeCache ?? null,
    phaseMediansMs: raw.phaseMediansMs ?? null,
  };
}

function summarizeE2eEngine(engineReport) {
  if (!engineReport) {
    return { status: "not-run", total: 0, passed: 0, failed: 0, blocked: 0 };
  }
  const total = engineReport.total ?? 0;
  const passed = engineReport.passed ?? 0;
  const failed = engineReport.failed ?? 0;
  const blocked = engineReport.blocked ?? 0;
  const ok = total > 0 && passed === total && failed === 0 && blocked === 0 && !engineReport.launchError;
  return {
    status: ok ? "passing" : engineReport.launchError ? "blocked" : "failing",
    total,
    passed,
    failed,
    blocked,
    ...(engineReport.launchError ? { launchError: engineReport.launchError } : {}),
  };
}

function summarizeE2eFile(raw, relPath) {
  if (!raw) {
    return { status: "not-run", source: relPath, reason: `${relPath} is missing` };
  }
  const chromium = summarizeE2eEngine(raw.engines?.chromium);
  const native = summarizeE2eEngine(raw.engines?.native);
  const nativeMode = raw.nativeMode ?? (raw.nativeStatus === "report-only" ? "report-only" : "gating");
  const nativeStatus = raw.nativeStatus ?? "not-run";
  const sameFlow = Boolean(raw.sameFlow);
  const domComparison = raw.domComparison
    ? {
        status: raw.domComparison.passed ? "passing" : "failing",
        matched: raw.domComparison.matched ?? 0,
        total: raw.domComparison.total ?? 0,
        mismatched: raw.domComparison.mismatched ?? 0,
      }
    : { status: "not-run", matched: 0, total: 0, mismatched: 0 };
  const ok =
    chromium.status === "passing" &&
    native.status === "passing" &&
    nativeMode === "gating" &&
    nativeStatus === "passing" &&
    sameFlow &&
    domComparison.status === "passing";
  return {
    source: relPath,
    windowed: Boolean(raw.windowed),
    gpuAdapter: raw.gpuAdapter ?? null,
    vkIcd: raw.vkIcd ?? null,
    nativeMode,
    nativeStatus,
    sameFlow,
    domComparison,
    chromium,
    native,
    status: ok ? "passing" : "failing",
  };
}

function summarizeNativeSceneRun(run, source) {
  if (!run) {
    return { status: "not-run", source, reason: `${source} is missing` };
  }
  const scenes = Array.isArray(run.scenes) ? run.scenes : [];
  const diffRatios = scenes
    .map((scene) => scene.diffRatio)
    .filter((value) => typeof value === "number" && Number.isFinite(value));
  const maxDiffRatio = diffRatios.length > 0 ? Math.max(...diffRatios) : null;
  const minDiffRatio = diffRatios.length > 0 ? Math.min(...diffRatios) : null;
  return {
    status: run.status ?? "not-run",
    source,
    gpuAdapter: run.gpuAdapter ?? null,
    vkIcd: run.vkIcd ?? null,
    total: run.total ?? scenes.length,
    passed: run.passed ?? 0,
    failed: run.failed ?? 0,
    tolerance: run.tolerance ?? null,
    minDiffRatio,
    maxDiffRatio,
    ...(run.reason ? { reason: run.reason } : {}),
    scenes: scenes.map((scene) => ({
      id: scene.id,
      pass: Boolean(scene.pass),
      diffPixels: scene.diffPixels ?? null,
      diffRatio: scene.diffRatio ?? null,
      maxDelta: scene.maxDelta ?? null,
      ...(scene.error ? { error: scene.error } : {}),
    })),
  };
}

function combineStatuses(statuses) {
  if (statuses.every((status) => status === "passing")) return "passing";
  if (statuses.includes("failing")) return "failing";
  if (statuses.includes("blocked")) return "blocked";
  return "not-run";
}

function summarizeScenes(raw, rawAuto) {
  if (!raw) {
    return { status: "not-run", reason: "tests/results/excalidraw/scenes/summary.json is missing" };
  }
  const chromium = {
    status: raw.status ?? "not-run",
    total: raw.total ?? 0,
    passed: raw.passed ?? 0,
    failed: raw.failed ?? 0,
    tolerance: raw.tolerance ?? null,
  };
  const softwareRun = raw.native ?? null;
  const autoRun = rawAuto?.native ?? rawAuto ?? null;
  const software = summarizeNativeSceneRun(softwareRun, "tests/results/excalidraw/scenes/summary.json");
  const auto = summarizeNativeSceneRun(autoRun, "tests/results/excalidraw/scenes/summary-auto.json");
  const nativeStatus = combineStatuses([software.status, auto.status]);
  const native = {
    status: nativeStatus,
    adapters: {
      software: software.status,
      auto: auto.status,
    },
    ...(software.reason && software.status !== "passing" ? { softwareReason: software.reason } : {}),
    ...(auto.reason && auto.status !== "passing" ? { autoReason: auto.reason } : {}),
  };
  return {
    source: "tests/results/excalidraw/scenes/summary.json",
    chromium,
    native,
    nativeByAdapter: {
      software,
      auto,
    },
    status: combineStatuses([chromium.status, software.status, auto.status]),
  };
}

function summarizePan(raw) {
  if (!raw) {
    return { status: "not-run", reason: "tests/results/excalidraw/pan.json is missing" };
  }
  if (raw.status !== "measured") {
    return { status: raw.status ?? "not-run", reason: raw.reason ?? null };
  }
  const limitRatio = raw.limitRatio ?? 0.95;
  const runs = Array.isArray(raw.runs) ? raw.runs : null;
  const valid =
    raw.valid === true && (!runs || (runs.length > 0 && runs.every((run) => run.valid === true)));
  const allRunsMeetLimit =
    !runs ||
    (runs.length > 0 &&
      runs.every((run) => run.valid === true && run.passed === true && Number(run.deadlineRatio) >= limitRatio));
  const passed =
    valid &&
    Boolean(raw.passed) &&
    typeof raw.deadlineRatio === "number" &&
    raw.deadlineRatio >= limitRatio &&
    allRunsMeetLimit;
  return {
    status: passed ? "passing" : valid ? "failing" : "invalid",
    passed,
    valid,
    invalidReasons: Array.isArray(raw.invalidReasons) ? raw.invalidReasons : [],
    reportOnly: Boolean(raw.reportOnly),
    source: "tests/results/excalidraw/pan.json",
    driver: raw.driver ?? null,
    runner: raw.runner ?? null,
    adapter: raw.adapter ?? null,
    cpuWeight: raw.cpuWeight ?? null,
    loadStart: raw.loadStart ?? null,
    loadEnd: raw.loadEnd ?? null,
    elements: raw.elements ?? null,
    scrollStart: raw.scrollStart ?? null,
    scrollEnd: raw.scrollEnd ?? null,
    expectedScroll: raw.expectedScroll ?? null,
    slots: raw.slots ?? null,
    wheelEvents: raw.wheelEvents ?? null,
    pageWheelEvents: raw.pageWheelEvents ?? null,
    canvasWheelEvents: raw.canvasWheelEvents ?? null,
    viewport: raw.viewport ?? null,
    submitted: raw.submitted ?? null,
    fresh: raw.fresh ?? null,
    onTimeSlots: raw.onTimeSlots ?? null,
    deadlineRatio: raw.deadlineRatio ?? null,
    minDeadlineRatio: raw.minDeadlineRatio ?? raw.deadlineRatio ?? null,
    medianDeadlineRatio: raw.medianDeadlineRatio ?? raw.deadlineRatio ?? null,
    limitRatio,
    commitLatenessP95Ms: raw.commitLatenessP95Ms ?? null,
    tickToCommitP95Ms: raw.tickToCommitP95Ms ?? null,
    latchAndEncodeP95Ms: raw.latchAndEncodeP95Ms ?? null,
    submitIntervalP95Ms: raw.submitIntervalP95Ms ?? null,
    ...(runs ? { runCount: runs.length, runs } : {}),
  };
}

/**
 * Verdict passes only when every native leg and both checks pass.
 * Returns `{ verdict: "passing" | "blocked", blockers: string[] }`.
 */
export function computeVerdict(report) {
  const blockers = [];
  if (report.packaging?.status !== "passing") {
    blockers.push(`packaging: ${report.packaging?.status ?? "missing"}`);
  }
  if (report.startup?.status !== "passing") {
    blockers.push(`startup: ${report.startup?.status ?? "missing"}`);
  }
  for (const mode of ["headless", "windowed"]) {
    const leg = report.e2e?.[mode];
    if (leg?.chromium?.status !== "passing") {
      blockers.push(`e2e.${mode}.chromium: ${leg?.chromium?.status ?? "missing"}`);
    }
    if (leg?.native?.status !== "passing") {
      blockers.push(`e2e.${mode}.native: ${leg?.native?.status ?? "missing"}`);
    }
    if (leg?.nativeMode !== "gating") {
      blockers.push(`e2e.${mode}.nativeMode: ${leg?.nativeMode ?? "missing"}`);
    }
    if (leg?.nativeStatus !== "passing") {
      blockers.push(`e2e.${mode}.nativeStatus: ${leg?.nativeStatus ?? "missing"}`);
    }
    if (leg?.sameFlow !== true) {
      blockers.push(`e2e.${mode}.sameFlow: ${String(leg?.sameFlow ?? "missing")}`);
    }
    if (leg?.domComparison?.status !== "passing") {
      blockers.push(`e2e.${mode}.domComparison: ${leg?.domComparison?.status ?? "missing"}`);
    }
  }
  if (report.scenes?.chromium?.status !== "passing") {
    blockers.push(`scenes.chromium: ${report.scenes?.chromium?.status ?? "missing"}`);
  }
  if (report.scenes?.native?.status !== "passing") {
    blockers.push(`scenes.native: ${report.scenes?.native?.status ?? "missing"}`);
  }
  if (report.scenes?.nativeByAdapter?.software?.status !== "passing") {
    blockers.push(
      `scenes.nativeByAdapter.software: ${report.scenes?.nativeByAdapter?.software?.status ?? "missing"}`,
    );
  }
  if (report.scenes?.nativeByAdapter?.auto?.status !== "passing") {
    blockers.push(
      `scenes.nativeByAdapter.auto: ${report.scenes?.nativeByAdapter?.auto?.status ?? "missing"}`,
    );
  }
  if (report.pan?.status !== "passing") {
    blockers.push(`pan: ${report.pan?.status ?? "missing"}`);
  } else if (report.pan?.reportOnly) {
    blockers.push("pan: report-only (awaiting validated nested-Hyprland benchmark)");
  }
  if (report.checks?.checkSh !== "passed") {
    blockers.push(`checks.checkSh: ${report.checks?.checkSh ?? "not-recorded"}`);
  }
  if (report.checks?.lavapipe !== "passed") {
    blockers.push(`checks.lavapipe: ${report.checks?.lavapipe ?? "not-recorded"}`);
  }
  return {
    verdict: blockers.length === 0 ? "passing" : "blocked",
    blockers,
  };
}

/** Generates the deterministic Excalidraw release report from tests/results/excalidraw/*.json. */
export function generateExcalidrawSummary({
  inputDir = resultsDir,
  outPath = path.join(resultsDir, "summary.json"),
  checkSh = process.env.SQUARK_CHECK_SH_RESULT || "not-recorded",
  lavapipe = process.env.SQUARK_LAVAPIPE_RESULT || "not-recorded",
} = {}) {
  const packaging = summarizePackaging(readJsonIfExists(path.join(inputDir, "packaging.json")));
  const startup = summarizeStartup(readJsonIfExists(path.join(inputDir, "startup.json")));
  const e2eHeadless = summarizeE2eFile(
    readJsonIfExists(path.join(inputDir, "e2e.json")),
    "tests/results/excalidraw/e2e.json",
  );
  const e2eWindowed = summarizeE2eFile(
    readJsonIfExists(path.join(inputDir, "e2e-windowed.json")),
    "tests/results/excalidraw/e2e-windowed.json",
  );
  const scenes = summarizeScenes(
    readJsonIfExists(path.join(inputDir, "scenes/summary.json")),
    readJsonIfExists(path.join(inputDir, "scenes/summary-auto.json")),
  );
  const pan = summarizePan(readJsonIfExists(path.join(inputDir, "pan.json")));

  const report = {
    goal: "Upstream Excalidraw v0.18.1 ships as one native Linux binary with zero source patches",
    referenceChromium: {
      path: findChromiumPath(),
      version: chromiumVersion(),
      playwright: pinnedChromium(),
    },
    packaging,
    startup,
    e2e: {
      headless: e2eHeadless,
      windowed: e2eWindowed,
    },
    scenes,
    pan,
    checks: {
      checkSh,
      lavapipe,
    },
    generatedBy: "tests/excalidraw/src/summary.mjs",
  };
  const { verdict, blockers } = computeVerdict(report);
  report.verdict = verdict;
  report.blockers = blockers;

  if (outPath) {
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
  }
  return report;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const report = generateExcalidrawSummary();
  console.log(
    `Excalidraw summary: verdict ${report.verdict}; ` +
      `packaging ${report.packaging.status} (${report.packaging.sizeMiB ?? "?"} MiB); ` +
      `startup ${report.startup.status} (${report.startup.medianMs ?? "?"} ms); ` +
      `e2e headless ${report.e2e.headless.native?.passed ?? 0}/${report.e2e.headless.native?.total ?? 0}, ` +
      `windowed ${report.e2e.windowed.native?.passed ?? 0}/${report.e2e.windowed.native?.total ?? 0}; ` +
      `scenes chromium ${report.scenes.chromium?.passed ?? 0}/${report.scenes.chromium?.total ?? 0}, ` +
        `native lavapipe ${report.scenes.nativeByAdapter?.software?.passed ?? 0}/${report.scenes.nativeByAdapter?.software?.total ?? 0}, ` +
        `hardware ${report.scenes.nativeByAdapter?.auto?.passed ?? 0}/${report.scenes.nativeByAdapter?.auto?.total ?? 0}; ` +
      `pan ${report.pan.status} (${report.pan.onTimeSlots ?? "?"}/${report.pan.slots ?? "?"})`,
  );
  if (report.blockers.length > 0) {
    console.log(`blockers: ${report.blockers.join(", ")}`);
  }
  console.log("report: tests/results/excalidraw/summary.json");
}

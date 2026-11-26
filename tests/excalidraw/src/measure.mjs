// SPDX-License-Identifier: MIT
// Excalidraw startup and pan measurements:
//   - windowed startup to the first presented frame of the empty scene
//     (median of 5 on the stripped dist runtime, reusing --exit-after-first-frame,
//     squark-host's SQUARK_STARTUP_PROFILE=1 spans, and squark-js's V8 module code cache);
//   - 1,000-element pan on the host frame timeline, reusing squark-host's real
//     Excalidraw pan driver (crates/squark-host/tests/fixtures/excalidraw-pan-driver.mjs),
//     run isolated in a nested Hyprland by scripts/check-excalidraw-pan.sh.
//
// Run via `scripts/measure-excalidraw.sh` (or `npm run measure[:startup|:pan] --workspace=@strange-quark/excalidraw-native`),
// which executes inside `systemd-run --user --scope -p CPUWeight=10000`.
// Packaging checks live in src/packaging.mjs and are not touched here.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  outDir,
  packExcalidraw,
  repoRoot,
  resolveRuntime,
} from "./build.mjs";
import { repoRelative } from "./platform.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const resultsDir = path.join(repoRoot, "tests/results/excalidraw");
const workDir = path.join(repoRoot, "target/excalidraw");
// Runs the pan driver in a private nested Hyprland, so desktop input and
// other windows cannot reach or resize the benchmark's window.
export const panScriptPath = path.join(repoRoot, "scripts/check-excalidraw-pan.sh");

export const panDriverPath = path.join(
  repoRoot,
  "crates/squark-host/tests/fixtures/excalidraw-pan-driver.mjs",
);
const windowed = process.env.SQUARK_WINDOW_TESTS === "1";

export function loadAverage() {
  // os.loadavg() reads /proc/loadavg on Linux and the native equivalent on
  // macOS; Windows reports zeros, which is what the platform offers.
  return os.loadavg().slice(0, 3);
}

export function currentCpuWeight() {
  try {
    const cgroup = fs.readFileSync("/proc/self/cgroup", "utf8").trim().split("::")[1];
    if (!cgroup) return null;
    const raw = fs.readFileSync(path.join("/sys/fs/cgroup", cgroup, "cpu.weight"), "utf8").trim();
    const weight = Number(raw);
    return Number.isFinite(weight) ? weight : null;
  } catch {
    return null;
  }
}

/** Counts cached V8 module files under an isolated XDG_CACHE_HOME. */
function countCodeCacheFiles(cacheHome) {
  const root = path.join(cacheHome, "strange-quark");
  if (!fs.existsSync(root)) return 0;
  let total = 0;
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    if (!entry.isDirectory() || !entry.name.startsWith("modules-")) continue;
    const dir = path.join(root, entry.name);
    for (const file of fs.readdirSync(dir)) {
      if (!file.startsWith(".pending-")) total += 1;
    }
  }
  return total;
}

/** Parses `squark-startup <phase> <ms> ms` lines emitted under SQUARK_STARTUP_PROFILE=1. */
export function parseStartupProfile(stderr) {
  const phases = {};
  for (const line of String(stderr ?? "").split(/\r?\n/)) {
    const match = line.match(/^squark-startup\s+([a-z0-9-]+)\s+([0-9]+(?:\.[0-9]+)?)\s+ms$/);
    if (!match) continue;
    const [, phase, ms] = match;
    phases[phase] = Number(ms);
  }
  return phases;
}

/** Computes the median of a numeric array, rounded to `digits` decimals. */
export function medianOf(values, digits = 1) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Number(sorted[Math.floor(sorted.length / 2)].toFixed(digits));
}

/** Computes per-phase medians across startup samples. */
export function summarizePhaseMedians(samples) {
  const byPhase = new Map();
  for (const sample of samples) {
    for (const [phase, ms] of Object.entries(sample.phases ?? {})) {
      if (!byPhase.has(phase)) byPhase.set(phase, []);
      byPhase.get(phase).push(ms);
    }
  }
  const medians = {};
  for (const [phase, values] of byPhase.entries()) {
    medians[phase] = medianOf(values, 3);
  }
  return medians;
}

/** A deterministic 1,000-element scene in the app's own element format. */
export function thousandElements() {
  const elements = [];
  for (let index = 0; index < 1000; index += 1) {
    const column = index % 20;
    const row = Math.floor(index / 20);
    const x = 60 + column * 120;
    const y = 60 + row * 90;
    const fill = ["#a5d8ff", "#b2f2bb", "#ffec99", "#ffc9c9", "#d0bfff"][index % 5];
    elements.push({
      id: `element-${index}`,
      type: index % 4 === 3 ? "ellipse" : "rectangle",
      x,
      y,
      width: 90,
      height: 60,
      angle: 0,
      strokeColor: "#1e1e1e",
      backgroundColor: fill,
      fillStyle: "solid",
      strokeWidth: 2,
      strokeStyle: "solid",
      roughness: 1,
      opacity: 100,
      groupIds: [],
      frameId: null,
      roundness: index % 4 === 3 ? { type: 2 } : { type: 3 },
      seed: 200000 + index * 7919,
      version: 1,
      versionNonce: index * 104729,
      isDeleted: false,
      boundElements: null,
      updated: 1735689600000,
      link: null,
      locked: false,
    });
  }
  return elements;
}

/** Runs one windowed Excalidraw launch and times process start -> first presented frame. */
function launchStartupSample({ binary, xdgDataHome, xdgCacheHome, adapter, timeoutMs = 60_000 }) {
  return new Promise((resolve, reject) => {
    const childEnv = { ...process.env };
    // Never inherit a lavapipe ICD when measuring hardware startup.
    delete childEnv.VK_ICD_FILENAMES;
    delete childEnv.VK_DRIVER_FILES;
    childEnv.XDG_DATA_HOME = xdgDataHome;
    childEnv.XDG_CACHE_HOME = xdgCacheHome;
    childEnv.SQUARK_STARTUP_PROFILE = "1";

    const args = ["--width", "1280", "--height", "800", "--exit-after-first-frame"];
    if (adapter && adapter !== "auto") {
      args.push("--gpu-adapter", adapter);
    }

    let stderr = "";
    let firstFrameMs = null;
    const started = performance.now();
    const child = spawn(binary, args, {
      stdio: ["ignore", "pipe", "pipe"],
      env: childEnv,
    });

    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`startup run timed out after ${timeoutMs} ms:\n${stderr.slice(-1500)}`));
    }, timeoutMs);

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
      if (firstFrameMs === null && stderr.includes("squark-runtime presented its first frame")) {
        firstFrameMs = performance.now() - started;
      }
    });

    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });

    child.on("close", (code, signal) => {
      clearTimeout(timer);
      const totalMs = performance.now() - started;
      resolve({
        ms: Number((firstFrameMs ?? totalMs).toFixed(1)),
        processMs: Number(totalMs.toFixed(1)),
        presented: firstFrameMs !== null,
        exitCode: code,
        signal: signal ?? null,
        phases: parseStartupProfile(stderr),
        stderr,
      });
    });
  });
}

/**
 * Median of 5 windowed runs, process start to the first presented frame of the
 * empty scene, on the stripped dist runtime.
 */
export async function measureStartup({ binary, runs = 5, adapter = "hardware" } = {}) {
  if (!windowed) {
    return { status: "not-run", reason: "set SQUARK_WINDOW_TESTS=1 to open windows for the timing" };
  }
  const runtime = resolveRuntime();
  const cpuWeight = currentCpuWeight();
  assert.equal(
    cpuWeight,
    10000,
    `run in a systemd user scope with CPUWeight=10000 (got ${cpuWeight})`,
  );

  fs.mkdirSync(workDir, { recursive: true });
  const cacheHome = path.join(workDir, "startup-cache");
  fs.rmSync(cacheHome, { recursive: true, force: true });
  fs.mkdirSync(cacheHome, { recursive: true });

  const loadStart = loadAverage();
  const samples = [];
  for (let run = 0; run < runs; run += 1) {
    const xdgDataHome = fs.mkdtempSync(path.join(os.tmpdir(), "squark-startup-xdg-"));
    const load = loadAverage();
    const cacheEntriesBefore = countCodeCacheFiles(cacheHome);
    try {
      const sample = await launchStartupSample({ binary, xdgDataHome, xdgCacheHome: cacheHome, adapter });
      const cacheEntriesAfter = countCodeCacheFiles(cacheHome);
      samples.push({
        ms: sample.ms,
        processMs: sample.processMs,
        load,
        presented: sample.presented,
        exitCode: sample.exitCode,
        cacheEntriesBefore,
        cacheEntriesAfter,
        phases: sample.phases,
      });
    } finally {
      fs.rmSync(xdgDataHome, { recursive: true, force: true });
    }
  }
  const loadEnd = loadAverage();
  const medianMs = medianOf(samples.map((sample) => sample.ms), 1);
  const phaseMediansMs = summarizePhaseMedians(samples);
  const passed =
    samples.length === runs &&
    medianMs !== null &&
    medianMs < 1500 &&
    samples.every((sample) => sample.presented && sample.exitCode === 0);

  return {
    status: "measured",
    runs: samples.length,
    adapter,
    cpuWeight,
    loadStart,
    loadEnd,
    runtime: {
      path: repoRelative(repoRoot, runtime.path),
      profile: runtime.profile,
      bytes: runtime.bytes,
    },
    binary: {
      path: repoRelative(repoRoot, binary),
      bytes: fs.statSync(binary).size,
    },
    v8CodeCache: {
      dir: repoRelative(repoRoot, cacheHome),
      entriesAfterRuns: countCodeCacheFiles(cacheHome),
    },
    samples,
    phaseMediansMs,
    medianMs,
    limitMs: 1500,
    passed,
  };
}

/** Normalizes excalidraw-pan-driver.mjs's summary into the pan report shape. */
export function formatPanResult(driverSummary, { reportOnly = false, timelinePath = null } = {}) {
  const deadlineRatio = Number(driverSummary.deadlineRatio ?? 0);
  // Only a run the driver judged valid can pass: exactly the scripted wheel
  // events reached the canvas and the scene scrolled the scripted distance.
  const valid = driverSummary.valid === true;
  const invalidReasons = valid
    ? []
    : (driverSummary.invalidReasons?.length
      ? driverSummary.invalidReasons
      : ["the driver did not report the run as valid"]);
  return {
    status: "measured",
    driver: repoRelative(repoRoot, panDriverPath),
    runner: repoRelative(repoRoot, panScriptPath),
    reportOnly,
    adapter: driverSummary.adapter ?? null,
    cpuWeight: driverSummary.cpuWeight ?? null,
    loadStart: driverSummary.loadStart ?? null,
    loadEnd: driverSummary.loadEnd ?? null,
    elements: driverSummary.elements ?? 1000,
    scrollStart: driverSummary.scrollStart ?? null,
    scrollEnd: driverSummary.scrollEnd ?? null,
    expectedScroll: driverSummary.expectedScroll ?? null,
    slots: driverSummary.slots ?? 300,
    wheelEvents: driverSummary.wheelEvents ?? 0,
    pageWheelEvents: driverSummary.pageWheelEvents ?? null,
    canvasWheelEvents: driverSummary.canvasWheelEvents ?? null,
    viewport: driverSummary.viewport ?? null,
    valid,
    invalidReasons,
    submitted: driverSummary.submitted ?? 0,
    fresh: driverSummary.fresh ?? 0,
    onTimeSlots: driverSummary.onTimeSlots ?? 0,
    deadlineRatio: Number(deadlineRatio.toFixed(4)),
    commitLatenessP95Ms: driverSummary.commitLatenessP95Ms ?? null,
    tickToCommitP95Ms: driverSummary.tickToCommitP95Ms ?? null,
    latchAndEncodeP95Ms: driverSummary.latchAndEncodeP95Ms ?? null,
    submitIntervalP95Ms: driverSummary.submitIntervalP95Ms ?? null,
    limitRatio: 0.95,
    passed: valid && Boolean(driverSummary.passed) && deadlineRatio >= 0.95,
    ...(timelinePath ? { timeline: repoRelative(repoRoot, timelinePath) } : {}),
  };
}

/**
 * Aggregates N validated nested-Hyprland pan runs. Passes only when every run
 * is valid and reaches >= 95% on-time fresh commits.
 */
export function formatPanSeries(formattedRuns, { reportOnly = false } = {}) {
  assert.ok(formattedRuns.length > 0, "expected at least one pan run");
  const sortedByRatio = [...formattedRuns].sort((a, b) => a.deadlineRatio - b.deadlineRatio);
  const medianRun = sortedByRatio[Math.floor(sortedByRatio.length / 2)];
  const minDeadlineRatio = sortedByRatio[0].deadlineRatio;
  const medianDeadlineRatio = medianRun.deadlineRatio;
  const allValid = formattedRuns.every((run) => run.valid === true);
  const allPassed = formattedRuns.every((run) => run.valid === true && run.passed === true && run.deadlineRatio >= 0.95);
  const invalidReasons = formattedRuns.flatMap((run, index) =>
    run.valid ? [] : run.invalidReasons.map((reason) => `run ${index + 1}: ${reason}`),
  );
  return {
    ...medianRun,
    reportOnly,
    runCount: formattedRuns.length,
    valid: allValid,
    invalidReasons,
    minDeadlineRatio,
    medianDeadlineRatio,
    passed: allValid && allPassed,
    runs: formattedRuns.map((run, index) => ({
      run: index + 1,
      valid: run.valid,
      invalidReasons: run.invalidReasons,
      passed: run.passed,
      onTimeSlots: run.onTimeSlots,
      slots: run.slots,
      deadlineRatio: run.deadlineRatio,
      wheelEvents: run.wheelEvents,
      pageWheelEvents: run.pageWheelEvents,
      canvasWheelEvents: run.canvasWheelEvents,
      scrollStart: run.scrollStart,
      scrollEnd: run.scrollEnd,
      expectedScroll: run.expectedScroll,
      viewport: run.viewport,
      fresh: run.fresh,
      submitted: run.submitted,
      commitLatenessP95Ms: run.commitLatenessP95Ms,
      tickToCommitP95Ms: run.tickToCommitP95Ms,
      latchAndEncodeP95Ms: run.latchAndEncodeP95Ms,
      submitIntervalP95Ms: run.submitIntervalP95Ms,
      cpuWeight: run.cpuWeight,
      loadStart: run.loadStart,
      loadEnd: run.loadEnd,
      ...(run.timeline ? { timeline: run.timeline } : {}),
    })),
  };
}

/** Runs the validated nested-Hyprland 1,000-element Excalidraw pan benchmark N times. */
export async function measurePan({ binary, runs = 3, reportOnly = false, timeoutMs = 240_000 } = {}) {
  if (!windowed) {
    return { status: "not-run", reason: "set SQUARK_WINDOW_TESTS=1 to open windows for the timing" };
  }
  assert.ok(fs.existsSync(panDriverPath), `missing pan driver at ${panDriverPath}`);
  assert.ok(fs.existsSync(panScriptPath), `missing isolated pan runner at ${panScriptPath}`);
  const cpuWeight = currentCpuWeight();
  assert.equal(
    cpuWeight,
    10000,
    `run in a systemd user scope with CPUWeight=10000 (got ${cpuWeight})`,
  );

  const childEnv = { ...process.env };
  delete childEnv.VK_ICD_FILENAMES;
  delete childEnv.VK_DRIVER_FILES;

  const formattedRuns = [];
  for (let run = 1; run <= runs; run += 1) {
    const panOutDir = runs === 1 ? path.join(workDir, "pan") : path.join(workDir, "pan", `run-${run}`);
    fs.mkdirSync(panOutDir, { recursive: true });
    const summaryPath = path.join(panOutDir, "summary.json");
    const timelinePath = path.join(panOutDir, "timeline.json");
    fs.rmSync(summaryPath, { force: true });
    fs.rmSync(timelinePath, { force: true });

    const { code, stderr } = await new Promise((resolve, reject) => {
      let errText = "";
      const child = spawn(panScriptPath, [binary, panOutDir], {
        cwd: repoRoot,
        env: childEnv,
        stdio: ["ignore", "pipe", "pipe"],
      });
      const timer = setTimeout(() => {
        child.kill("SIGKILL");
        reject(new Error(`excalidraw-pan-driver run ${run} timed out after ${timeoutMs} ms:\n${errText.slice(-1500)}`));
      }, timeoutMs);
      child.stderr.on("data", (chunk) => {
        errText += chunk.toString("utf8");
      });
      child.on("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on("close", (exitCode) => {
        clearTimeout(timer);
        resolve({ code: exitCode, stderr: errText });
      });
    });

    if (!fs.existsSync(summaryPath)) {
      throw new Error(
        `excalidraw-pan-driver run ${run} exited ${code} without writing ${summaryPath}:\n${stderr.slice(-2000)}`,
      );
    }
    const driverSummary = JSON.parse(fs.readFileSync(summaryPath, "utf8"));
    formattedRuns.push(formatPanResult(driverSummary, { reportOnly, timelinePath }));
  }

  return formattedRuns.length === 1
    ? formattedRuns[0]
    : formatPanSeries(formattedRuns, { reportOnly });
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  fs.mkdirSync(resultsDir, { recursive: true });
  const args = process.argv.slice(2);
  const reportOnlyPan = args.includes("--report-only-pan");
  const panRunsFlag = args.indexOf("--pan-runs");
  const panRuns = panRunsFlag !== -1 && args[panRunsFlag + 1] ? Number(args[panRunsFlag + 1]) : 3;
  const onlyStartup = args.includes("--startup");
  const onlyPan = args.includes("--pan");
  const runStartup = onlyStartup || !onlyPan;
  const runPan = onlyPan || !onlyStartup;

  // Always pack fresh onto the current stripped dist runtime so startup and pan
  // measure the current build rather than a stale binary.
  const packed = packExcalidraw();
  const nativeBinary = packed.binary;

  let failed = false;
  if (runStartup) {
    const startup = await measureStartup({ binary: nativeBinary });
    fs.writeFileSync(path.join(resultsDir, "startup.json"), `${JSON.stringify(startup, null, 2)}\n`);
    console.log(
      startup.status === "measured"
        ? `startup: ${startup.passed ? "PASS" : "FAIL"} median ${startup.medianMs} ms over ${startup.runs} ` +
            `(limit ${startup.limitMs} ms, cpuWeight ${startup.cpuWeight}, load ${startup.loadStart.join(" ")} -> ${startup.loadEnd.join(" ")})`
        : `startup: ${startup.reason}`,
    );
    if (startup.status === "measured" && !startup.passed) failed = true;
  }

  if (runPan) {
    const pan = await measurePan({ binary: nativeBinary, runs: panRuns, reportOnly: reportOnlyPan });
    fs.writeFileSync(path.join(resultsDir, "pan.json"), `${JSON.stringify(pan, null, 2)}\n`);
    if (pan.status === "measured") {
      const list = Array.isArray(pan.runs) ? pan.runs : [pan];
      for (const [idx, run] of list.entries()) {
        console.log(
          `pan run ${run.run ?? idx + 1}: ${run.passed ? "PASS" : run.valid ? "FAIL" : `INVALID (${run.invalidReasons.join("; ")})`} ` +
            `${run.onTimeSlots}/${run.slots} (${(run.deadlineRatio * 100).toFixed(1)}%), ` +
            `wheels host=${run.wheelEvents} page=${run.pageWheelEvents} canvas=${run.canvasWheelEvents}, ` +
            `scroll ${run.scrollStart}->${run.scrollEnd} (expected ${run.expectedScroll}), ` +
            `commit p95 ${run.commitLatenessP95Ms} ms, load ${run.loadStart.join(" ")} -> ${run.loadEnd.join(" ")}`,
        );
      }
      console.log(
        `pan (${pan.reportOnly ? "report-only" : "gating"}, ${list.length} runs): ` +
          `${pan.passed ? "PASS" : pan.valid ? "FAIL" : "INVALID"} ` +
          `min ${((pan.minDeadlineRatio ?? pan.deadlineRatio) * 100).toFixed(1)}%, ` +
          `median ${((pan.medianDeadlineRatio ?? pan.deadlineRatio) * 100).toFixed(1)}% (limit 95%)`,
      );
      if (!reportOnlyPan && !pan.passed) failed = true;
    } else {
      console.log(`pan: ${pan.reason}`);
    }
  }

  if (failed) process.exit(1);
}

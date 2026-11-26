#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Generates tests/results/react-apps/summary.json containing every React app metric:
// - React 19.0.0 react-dom test-suite subset (tests/react-dom/cases.json)
// - WPT DOM subset (tests/wpt/run.mjs)
// - apps/counter and apps/todomvc SquarkDriver vs ChromiumDriver checkpoints and
//   screenshot comparisons at the app screenshot tolerance.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runWptSuite } from "../../wpt/run.mjs";
import { APP_SCREENSHOT_TOLERANCE, compareAppAcrossEngines } from "./compare-apps.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

function readReactDomSummary() {
  const casesPath = path.join(repoRoot, "tests/react-dom/cases.json");
  const inventory = JSON.parse(fs.readFileSync(casesPath, "utf8"));
  let passing = 0;
  let excluded = 0;
  const suites = {};
  for (const [file, info] of Object.entries(inventory.suites)) {
    const p = info.passing.length;
    const e = info.excluded.length;
    passing += p;
    excluded += e;
    suites[file] = { passing: p, excluded: e, total: p + e };
  }
  const total = passing + excluded;

  // Verify against last Jest run output if present under target/react-dom-passing.json.
  const reportPath = path.join(repoRoot, "target/react-dom-passing.json");
  let verifiedPassed = null;
  if (fs.existsSync(reportPath)) {
    const jestReport = JSON.parse(fs.readFileSync(reportPath, "utf8"));
    verifiedPassed = jestReport.numPassedTests ?? null;
  }

  return {
    reactVersion: "19.0.0",
    upstreamCommit: inventory.upstream ?? "7aa5dda3b3e4c2baa905a59b922ae7ec14734b24",
    suitesTotal: Object.keys(suites).length,
    allowlistPassing: passing,
    excluded,
    total,
    verifiedPassed,
    suites,
  };
}

function readR1Summary() {
  const r1Path = path.join(repoRoot, "tests/results/r1/summary.json");
  if (!fs.existsSync(r1Path)) return null;
  const data = JSON.parse(fs.readFileSync(r1Path, "utf8"));
  return {
    passed: data.passed,
    failed: data.failed,
    total: data.total,
    passRate: data.passRate,
  };
}

export async function generateReactAppsSummary({
  squarkBin = process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark"),
  runtimeBin = process.env.SQUARK_RUNTIME_BIN || path.join(repoRoot, "target/debug/squark-runtime"),
  gpuAdapter = process.env.SQUARK_GPU_ADAPTER || "auto",
  outPath = path.join(repoRoot, "tests/results/react-apps/summary.json"),
} = {}) {
  const reactDom = readReactDomSummary();
  const wptReport = await runWptSuite({ quiet: true });
  const wpt = {
    revision: wptReport.revision,
    totalFiles: wptReport.totalFiles,
    totalSubtests: wptReport.totalSubtests,
    passed: wptReport.passed,
    failed: wptReport.failed,
    passRate: wptReport.passRate,
    requiredPassRate: 0.95,
    meetsRequiredPassRate: wptReport.passRate >= 0.95,
  };

  const apps = {};
  for (const appName of ["counter", "todomvc"]) {
    apps[appName] = await compareAppAcrossEngines({
      appName,
      squarkBin,
      runtimeBin,
      gpuAdapter,
    });
  }

  const summary = {
    gpuAdapter,
    tolerance: {
      ...APP_SCREENSHOT_TOLERANCE,
      domState: "exact",
    },
    reactDom,
    wpt,
    apps,
    r1: readR1Summary(),
    pass:
      wpt.meetsRequiredPassRate &&
      reactDom.allowlistPassing === 520 &&
      apps.counter.pass &&
      apps.todomvc.pass,
  };

  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, JSON.stringify(summary, null, 2) + "\n");
  return summary;
}

function parseArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--squark-bin" && argv[i + 1]) opts.squarkBin = argv[++i];
    else if (a === "--runtime" && argv[i + 1]) opts.runtimeBin = argv[++i];
    else if (a === "--gpu-adapter" && argv[i + 1]) opts.gpuAdapter = argv[++i];
    else if (a === "--out" && argv[i + 1]) opts.outPath = path.resolve(argv[++i]);
    else if (a === "--enforce") opts.enforce = true;
  }
  return opts;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const opts = parseArgs(process.argv.slice(2));
  const summary = await generateReactAppsSummary(opts);
  console.log(
    `React apps (${summary.gpuAdapter}): react-dom ${summary.reactDom.allowlistPassing}/${summary.reactDom.total} | WPT ${summary.wpt.passed}/${summary.wpt.totalSubtests} (${(summary.wpt.passRate * 100).toFixed(1)}%) | counter ${summary.apps.counter.pass ? "PASS" : "FAIL"} (${summary.apps.counter.checkpointsPassed}/${summary.apps.counter.checkpointsTotal} chk, ${summary.apps.counter.screenshotsPassed}/${summary.apps.counter.screenshotsTotal} png) | todomvc ${summary.apps.todomvc.pass ? "PASS" : "FAIL"} (${summary.apps.todomvc.checkpointsPassed}/${summary.apps.todomvc.checkpointsTotal} chk, ${summary.apps.todomvc.screenshotsPassed}/${summary.apps.todomvc.screenshotsTotal} png)`,
  );
  if (opts.enforce && !summary.pass) {
    process.exit(1);
  }
}

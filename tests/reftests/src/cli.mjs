#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// CLI entry point for @strange-quark/reftests.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { warnIfTestFontsMissing } from "./fonts.mjs";
import { runReftests } from "./runner.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const reftestsRoot = path.resolve(__dirname, "..");
const testsRoot = path.resolve(reftestsRoot, "..");

function parseArgs(argv) {
  const opts = {
    suite: "smoke",
    mode: "chromium",
    filter: null,
    squarkBin: process.env.SQUARK_BIN || "squark",
    runtime: process.env.SQUARK_RUNTIME_BIN || null,
    gpuAdapter: process.env.SQUARK_GPU_ADAPTER || null,
    minPassRate: null,
    reportOnly: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--suite" && argv[i + 1]) opts.suite = argv[++i];
    else if (arg === "--mode" && argv[i + 1]) opts.mode = argv[++i];
    else if (arg === "--filter" && argv[i + 1]) opts.filter = argv[++i];
    else if (arg === "--squark-bin" && argv[i + 1]) opts.squarkBin = argv[++i];
    else if (arg === "--runtime" && argv[i + 1]) opts.runtime = argv[++i];
    else if (arg === "--gpu-adapter" && argv[i + 1]) opts.gpuAdapter = argv[++i];
    else if (arg === "--min-pass-rate" && argv[i + 1]) opts.minPassRate = Number(argv[++i]);
    else if (arg === "--report-only") opts.reportOnly = true;
    else if (arg === "--json") opts.json = true;
  }
  return opts;
}

warnIfTestFontsMissing({
  surface: "Reftests that render system-font text",
});

const opts = parseArgs(process.argv.slice(2));
const summary = await runReftests({
  casesRoot: path.join(reftestsRoot, "cases"),
  resultsRoot: path.join(testsRoot, "results"),
  suite: opts.suite,
  mode: opts.mode,
  filter: opts.filter,
  squarkBin: opts.squarkBin,
  runtime: opts.runtime,
  gpuAdapter: opts.gpuAdapter,
});

if (opts.mode === "compare" && !opts.json) {
  const pct = (summary.passRate * 100).toFixed(1);
  console.log(
    `reftests/${summary.suite} compare (${summary.gpuAdapter}): ${summary.passed}/${summary.total} passed (${pct}%), ${summary.failed} failed, ${summary.renderErrors} render errors`,
  );
  if (summary.renderErrors > 0) {
    for (const c of summary.cases.filter((entry) => entry.reason?.startsWith("render error:"))) {
      console.error(`  RENDER ERROR ${c.id}: ${c.reason}`);
    }
  }
} else {
  console.log(JSON.stringify(summary, null, 2));
}

if (opts.mode === "compare") {
  // Report-only suites record render errors and
  // mismatches instead of failing the run.
  if (!opts.reportOnly && summary.renderErrors > 0) {
    process.exit(1);
  }
  if (!opts.reportOnly) {
    if (opts.minPassRate != null) {
      if (summary.passRate < opts.minPassRate) {
        console.error(
          `pass rate ${(summary.passRate * 100).toFixed(1)}% below required ${(opts.minPassRate * 100).toFixed(1)}%`,
        );
        process.exit(1);
      }
    } else if (summary.failed > 0) {
      process.exit(1);
    }
  }
} else if (summary.failed && summary.failed > 0) {
  process.exit(1);
}

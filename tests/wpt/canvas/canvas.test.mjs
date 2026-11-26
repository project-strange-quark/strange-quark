// SPDX-License-Identifier: MIT
// Canvas 2D WPT subset in Chromium and the packed native runtime.

import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { WPT_REVISION, runCanvasWpt } from "./run.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");

test("Canvas 2D WPT subset passes in Chromium", { timeout: 300_000 }, async () => {
  const summary = await runCanvasWpt({
    engine: "chromium",
    outPath: path.join(repoRoot, "tests/results/wpt-canvas/summary.json"),
  });
  assert.equal(summary.revision, WPT_REVISION);
  assert.ok(summary.total >= 60, `expected the imported canvas subset, got ${summary.total} files`);
  assert.equal(summary.failed, 0, JSON.stringify(summary.cases.filter((entry) => !entry.pass), null, 2));
  assert.equal(summary.passRate, 1);
  assert.equal(summary.subtestsPassed, summary.subtestsTotal);
});

test("native canvas WPT reports missing executables", async () => {
  const summary = await runCanvasWpt({
    engine: "native",
    squarkBin: path.join(repoRoot, "target/wpt-canvas-missing/squark"),
    runtimeBin: path.join(repoRoot, "target/wpt-canvas-missing/squark-runtime"),
  });
  assert.equal(summary.status, "blocked");
  assert.match(summary.reason, /squark-runtime/);
});

test("native canvas WPT packs and runs a pinned pixel assertion", {
  timeout: 60_000,
  skip: process.env.SQUARK_GPU_TESTS !== "1",
}, async () => {
  const summary = await runCanvasWpt({ engine: "native", filter: "2d.fillRect.basic.html" });
  assert.equal(summary.status, "passing", JSON.stringify(summary.cases));
  assert.equal(summary.total, 1);
  assert.equal(summary.passed, 1);
  assert.ok(summary.subtestsPassed >= 1);
});

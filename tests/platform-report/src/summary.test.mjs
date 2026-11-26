// SPDX-License-Identifier: MIT
// Platform report: the Chromium reference leg must pass; the native legs
// are recorded as passing, failing or blocked.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

import { generatePlatformSummary, repoRoot } from "./summary.mjs";

// The test writes under target/ so `npm test` never dirties the committed
// platform report; `node tests/platform-report/src/summary.mjs` writes tests/results/platform/summary.json.
const outPath = path.join(repoRoot, "target/platform-report/summary-test.json");

test("the platform summary records every number", { timeout: 300_000 }, async () => {
  const report = await generatePlatformSummary({ outPath });

  assert.ok(fs.existsSync(outPath), "summary.json must be written");
  assert.ok(report.referenceChromium.version.includes(report.referenceChromium.playwright.browserVersion));

  // The Canvas WPT subset passes in Chromium, the reference.
  assert.equal(report.canvasWpt.chromium.failed, 0, JSON.stringify(report.canvasWpt.chromium));
  assert.equal(report.canvasWpt.chromium.passRate, 1);
  assert.ok(report.canvasWpt.chromium.total >= 60);
  // Native is either blocked on the missing backend or a real pass.
  assert.ok(
    ["blocked", "passing", "failing"].includes(report.canvasWpt.native.status),
    report.canvasWpt.native.status,
  );

  // R3 is report-only, but it must still be recorded.
  assert.ok(
    ["passing", "failing", "blocked", "not-run", "error"].includes(report.r3.status),
    report.r3.status,
  );
  if (report.r3.status === "passing") {
    assert.equal(report.r3.failed, 0);
  }

  // The Excalidraw native checks, each naming the component it exercises.
  assert.ok(["passing", "failing", "blocked", "not-run"].includes(report.excalidraw.status));
  for (const check of report.excalidraw.checks ?? []) {
    assert.equal(typeof check.component, "string", JSON.stringify(check));
  }

  // The verdict can only be passing when every native leg passes.
  if (report.verdict === "passing") {
    assert.equal(report.canvasWpt.native.status, "passing");
    assert.equal(report.r3.status, "passing");
    assert.equal(report.excalidraw.status, "passing");
  } else {
    assert.equal(report.verdict, "blocked");
  }
});

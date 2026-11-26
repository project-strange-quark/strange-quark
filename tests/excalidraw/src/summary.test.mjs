// SPDX-License-Identifier: MIT
// Unit tests for the Excalidraw release report (verdict gating and schema completeness).

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

import { repoRoot } from "./build.mjs";
import { compareCheckpoints } from "./flow/state.mjs";
import { scratchTempDir } from "./platform.mjs";
import { computeVerdict, generateExcalidrawSummary } from "./summary.mjs";

function passingE2eLeg() {
  return {
    nativeMode: "gating",
    nativeStatus: "passing",
    sameFlow: true,
    domComparison: { status: "passing", matched: 20, total: 20, mismatched: 0 },
    chromium: { status: "passing" },
    native: { status: "passing" },
  };
}

function allPassingReport() {
  return {
    packaging: { status: "passing" },
    startup: { status: "passing" },
    e2e: {
      headless: passingE2eLeg(),
      windowed: passingE2eLeg(),
    },
    scenes: {
      chromium: { status: "passing" },
      native: { status: "passing" },
      nativeByAdapter: {
        software: { status: "passing" },
        auto: { status: "passing" },
      },
    },
    pan: { status: "passing" },
    checks: { checkSh: "passed", lavapipe: "passed" },
  };
}

test("compareCheckpoints fails on DOM or scene drift between Chromium and native", () => {
  const base = {
    dom: {
      excalidraw: true,
      canvases: 2,
      toolbarButtons: 12,
      dark: false,
      theme: null,
      libraryOpen: false,
      wysiwyg: null,
      zoomText: "100%",
      canvasFilled: true,
      canvas: { x: 0, y: 0, width: 1280, height: 800 },
    },
    elements: ["rectangle"],
    texts: [],
    appTheme: "light",
  };
  const ok = compareCheckpoints(
    [{ id: "draw-rectangle", status: "pass", checkpoint: base }],
    [{ id: "draw-rectangle", status: "pass", checkpoint: structuredClone(base) }],
  );
  assert.equal(ok.passed, true);
  assert.equal(ok.matched, 1);
  assert.equal(ok.mismatched, 0);

  const drifted = structuredClone(base);
  drifted.dom.dark = true;
  const bad = compareCheckpoints(
    [{ id: "draw-rectangle", status: "pass", checkpoint: base }],
    [{ id: "draw-rectangle", status: "pass", checkpoint: drifted }],
  );
  assert.equal(bad.passed, false);
  assert.equal(bad.mismatched, 1);
  assert.equal(bad.mismatches[0].diffs[0].field, "dom.dark");
});

test("computeVerdict passes only when every native leg and both checks pass", () => {
  const full = allPassingReport();
  assert.deepEqual(computeVerdict(full), { verdict: "passing", blockers: [] });

  const mutations = [
    ["packaging", (r) => { r.packaging.status = "failing"; }],
    ["startup", (r) => { r.startup.status = "failing"; }],
    ["e2e.headless.native", (r) => { r.e2e.headless.native.status = "failing"; }],
    ["e2e.headless.nativeMode", (r) => { r.e2e.headless.nativeMode = "report-only"; }],
    ["e2e.headless.domComparison", (r) => { r.e2e.headless.domComparison.status = "failing"; }],
    ["e2e.windowed.native", (r) => { r.e2e.windowed.native.status = "failing"; }],
    ["e2e.windowed.nativeMode", (r) => { r.e2e.windowed.nativeMode = "report-only"; }],
    ["e2e.windowed.domComparison", (r) => { r.e2e.windowed.domComparison.status = "failing"; }],
    ["scenes.native", (r) => { r.scenes.native.status = "blocked"; }],
    ["scenes.nativeByAdapter.software", (r) => { r.scenes.nativeByAdapter.software.status = "failing"; }],
    ["scenes.nativeByAdapter.auto", (r) => { r.scenes.nativeByAdapter.auto.status = "not-run"; }],
    ["pan", (r) => { r.pan.status = "failing"; }],
    ["pan", (r) => { r.pan.reportOnly = true; }],
    ["checks.checkSh", (r) => { r.checks.checkSh = "not-recorded"; }],
    ["checks.lavapipe", (r) => { r.checks.lavapipe = "not-recorded"; }],
  ];

  for (const [label, mutate] of mutations) {
    const copy = structuredClone(full);
    mutate(copy);
    const out = computeVerdict(copy);
    assert.equal(out.verdict, "blocked", `expected blocked when ${label} is not passing`);
    assert.ok(out.blockers.some((entry) => entry.startsWith(label)), `expected blocker for ${label}: ${out.blockers}`);
  }
});

test("generateExcalidrawSummary folds in packaging, startup, e2e, scenes, and pan without dirtying results", () => {
  const outPath = path.join(repoRoot, "target/excalidraw/summary-test.json");
  const report = generateExcalidrawSummary({
    outPath,
    checkSh: "passed",
    lavapipe: "passed",
  });

  assert.equal(fs.existsSync(outPath), true);
  assert.equal(report.packaging.status, "passing");
  assert.equal(report.packaging.noPatches, true);
  assert.equal(report.packaging.oneBinary, true);
  assert.equal(report.packaging.systemLibsOnly, true);
  assert.equal(report.startup.status, "passing");
  assert.ok(report.startup.medianMs < 1500);
  assert.equal(report.e2e.headless.chromium.passed, 20);
  assert.equal(report.e2e.headless.native.passed, 20);
  assert.equal(report.e2e.windowed.chromium.passed, 20);
  assert.equal(report.e2e.windowed.native.passed, 20);
  assert.equal(report.scenes.chromium.passed, 10);
  assert.equal(
    report.pan.status,
    report.pan.passed ? "passing" : report.pan.valid ? "failing" : "invalid",
  );

  if (report.verdict === "passing") {
    assert.equal(report.scenes.native.status, "passing");
    assert.equal(report.scenes.nativeByAdapter.software.status, "passing");
    assert.equal(report.scenes.nativeByAdapter.auto.status, "passing");
    assert.equal(report.pan.status, "passing");
  } else {
    assert.equal(report.verdict, "blocked");
    assert.ok(report.blockers.length > 0);
  }
});

test("generateExcalidrawSummary ignores platform-suffixed reports", () => {
  const inputDir = scratchTempDir(repoRoot, "excalidraw-summary-platform-");
  try {
    fs.copyFileSync(
      path.join(repoRoot, "tests/results/excalidraw/packaging.json"),
      path.join(inputDir, "packaging.json"),
    );
    fs.writeFileSync(
      path.join(inputDir, "packaging-macos.json"),
      JSON.stringify({
        pass: false,
        expectedTag: "v0.18.1",
        submodule: { clean: false, matchesTag: false },
        binary: {},
        libraries: {},
        smoke: {},
      }),
    );
    fs.mkdirSync(path.join(inputDir, "scenes"));
    fs.writeFileSync(
      path.join(inputDir, "scenes", "summary.json"),
      JSON.stringify({
        status: "passing",
        total: 10,
        passed: 10,
        failed: 0,
        tolerance: { maxChannelDelta: 8, maxDiffRatio: 0.005 },
        native: { status: "passing", total: 10, passed: 10, failed: 0, scenes: [] },
      }),
    );
    fs.mkdirSync(path.join(inputDir, "scenes-macos"));
    fs.writeFileSync(
      path.join(inputDir, "scenes-macos", "summary.json"),
      JSON.stringify({ status: "failing", total: 10, passed: 0, failed: 10, scenes: [] }),
    );

    const report = generateExcalidrawSummary({ inputDir, outPath: null });
    assert.equal(report.packaging.status, "passing");
    assert.equal(report.scenes.chromium.status, "passing");
    assert.equal(report.scenes.chromium.passed, 10);
  } finally {
    fs.rmSync(inputDir, { recursive: true, force: true });
  }
});

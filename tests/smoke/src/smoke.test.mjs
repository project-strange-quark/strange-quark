// SPDX-License-Identifier: MIT
// Scale smoke tests for the real build/pack/runtime pipeline:
// 1. apps/counter: `squark build` with real squark-runtime + headless screenshot.
// 2. vendor/excalidraw: release build -> `squark pack` -> `squark inspect` -> headless squark-runtime
//    (gated on the vendor/excalidraw submodule being checked out).

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runCounterBuildSmoke } from "./counter-build.mjs";
import { isExcalidrawSubmodulePresent, runExcalidrawPackSmoke } from "./excalidraw-pack.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const squarkBin = process.env.SQUARK_BIN || path.join(repoRoot, "target/debug/squark");
const runtimeBin = process.env.SQUARK_RUNTIME_BIN || path.join(repoRoot, "target/debug/squark-runtime");
const gpuEnabled = process.env.SQUARK_GPU_TESTS === "1";

test("apps/counter squark build end-to-end with real squark-runtime", (t) => {
  if (!fs.existsSync(squarkBin) || !fs.existsSync(runtimeBin)) {
    t.skip("requires built target/debug/squark and target/debug/squark-runtime");
    return;
  }
  const report = runCounterBuildSmoke({
    squarkBin,
    runtimeBin,
    skipScreenshot: !gpuEnabled,
  });
  assert.ok(report.binaryBytes > report.packBytes);
  if (gpuEnabled) {
    assert.equal(report.screenshot.width, 800);
    assert.equal(report.screenshot.height, 600);
    assert.ok(report.screenshot.maxDelta <= 2);
  }
});

test("Excalidraw v0.18.1 release build, squark pack, squark inspect, and headless squark-runtime startup", (t) => {
  if (!isExcalidrawSubmodulePresent()) {
    t.skip("vendor/excalidraw submodule is not checked out");
    return;
  }
  if (!fs.existsSync(squarkBin)) {
    t.skip("requires built target/debug/squark");
    return;
  }
  const skipScreenshot = !gpuEnabled || !fs.existsSync(runtimeBin);
  const hasBuiltExcalidraw = fs.existsSync(
    path.join(repoRoot, "vendor/excalidraw/excalidraw-app/build/index.html"),
  );
  const report = runExcalidrawPackSmoke({
    squarkBin,
    runtimeBin,
    skipAppBuild: hasBuiltExcalidraw && process.env.SQUARK_EXCALIDRAW_REBUILD !== "1",
    skipScreenshot,
  });
  assert.equal(report.skipped, false);
  assert.equal(report.inlineStyles, 2);
  assert.equal(report.scripts, 7);
  assert.ok(report.packBytes > 10 * 1024 * 1024, "expected >10 MiB Excalidraw pack");
  if (!skipScreenshot) {
    assert.equal(report.headless.width, 1280);
    assert.equal(report.headless.height, 800);
  }
});

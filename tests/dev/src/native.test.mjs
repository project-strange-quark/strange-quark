// SPDX-License-Identifier: MIT
// Native `squark dev` on apps/shadcn with the real runtime,
// measured from save to the presented frame. Needs a Vulkan device, so it runs
// only when SQUARK_GPU_TESTS=1 (CI's lavapipe job) or SQUARK_NATIVE_DEV_TESTS=1.

import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { measureNativeDevLoop, repoRoot } from "./measure-native.mjs";

const enabled =
  process.env.SQUARK_GPU_TESTS === "1" || process.env.SQUARK_NATIVE_DEV_TESTS === "1";

test(
  "native squark dev: save to presented frame is under 1 s and Fast Refresh keeps state",
  { timeout: 300_000, skip: enabled ? false : "set SQUARK_GPU_TESTS=1 to run the native dev measurement" },
  async () => {
    const report = await measureNativeDevLoop({
      iterations: 3,
      reportPath: path.join(repoRoot, "tests/results/interactive-apps/dev-loop.json"),
    });
    assert.equal(report.engine, "native");
    assert.equal(report.iterations, 3);
    assert.equal(report.fastRefreshKeptState, true);
    assert.ok(report.visibleChange, "expected before/after frames");
    assert.ok(
      report.visibleChange.diffPixels > 100,
      `the presented frame did not visibly change: ${JSON.stringify(report.visibleChange)}`,
    );
    assert.ok(
      report.saveToPresentedMs.max < 1000,
      `save -> presented frame max ${report.saveToPresentedMs.max} ms (limit: < 1000 ms)`,
    );
    assert.equal(report.passed, true);
  },
);

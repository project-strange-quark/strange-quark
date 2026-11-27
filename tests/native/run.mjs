// SPDX-License-Identifier: MIT
// End-to-end check of native modules: `squark build` links module/ into an
// app runtime, then the packed binary runs headless under test control and
// the page's results are read back. Needs a GPU adapter (lavapipe works).
//
//   node tests/native/run.mjs [--skip-build]

import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { SquarkDriver } from "../compat/src/squark-driver.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const squark = path.join(root, "target/debug/squark");
const binary = path.join(here, "dist/native-fixture");

if (!process.argv.includes("--skip-build")) {
  execFileSync(squark, ["build"], { cwd: here, stdio: "inherit", timeout: 3_600_000 });
}

const driver = await SquarkDriver.launch({ binaryPath: binary, headless: true, width: 640, height: 480, timeoutMs: 60_000 });
try {
  const deadline = Date.now() + 30_000;
  while ((await driver.queryText("#status")) !== "ready") {
    if (Date.now() > deadline) throw new Error(`page did not finish: ${driver._stderrRef()}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const results = await driver.eval("results");
  assert.deepEqual(results.modules, ["fixture"], "only modules squark.config.json enables are callable");
  assert.deepEqual(results.echo, { ok: { label: "x", value: [1, "two", { three: 3 }] } });
  assert.deepEqual(results.fail, { err: "No such file" });
  assert.deepEqual(results.count, [1, 2]);
  assert.match(results.unlisted.err, /not available/);
  assert.match(results.badArgs.err, /missing field `value`/);
  assert.deepEqual(results.delayed, { ok: 5 });
  assert.equal(results.burst.count, 10000);
  assert.equal(results.burst.ticks, 10000, "every event arrives");
  assert.equal(results.burst.ordered, true, "events arrive in emission order");
  console.log(`native modules: ok (10000 events in ${results.burst.ms} ms)`);
} finally {
  await driver.close();
}

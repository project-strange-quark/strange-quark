// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [binaryPath, pack, mode] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath,
  args: [pack],
  headless: mode === "headless",
  width: 90,
  height: 60,
});
try {
  await driver.waitForIdle();
  assert.equal(await driver.eval("document.documentElement.style.backgroundColor"), "#2468ac");
  assert.equal(await driver.eval("window.canvasAfterPaint()"), "19,87,155,255");
  let blob;
  for (let attempt = 0; attempt < 100 && !blob; attempt++) {
    blob = await driver.eval("window.canvasBlob ?? null");
    if (!blob) await sleep(20);
  }
  assert.equal(blob?.[0], "image/webp");
  assert.ok(blob[1] > 20);
} finally {
  await driver.close();
}

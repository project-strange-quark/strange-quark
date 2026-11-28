// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [binaryPath, pack] = process.argv.slice(2);
const driver = await SquarkDriver.launch({ binaryPath, args: [pack], width: 160, height: 100, headless: false });

async function waitForVisibility(state) {
  const deadline = Date.now() + 5_000;
  do {
    if (await driver.eval("document.visibilityState") === state) return;
    await sleep(50);
  } while (Date.now() < deadline);
  assert.fail(`timed out waiting for ${state}\n${driver.stderrText()}`);
}

try {
  assert.equal(await driver.eval("document.hidden"), false);
  await driver.eval(`
    globalThis.visibilityChanges = [];
    document.addEventListener("visibilitychange", () => {
      visibilityChanges.push([document.visibilityState, document.hidden]);
    });
  `);

  await driver.request("window.setMinimized", { minimized: true });
  await waitForVisibility("hidden");
  await driver.request("window.setMinimized", { minimized: false });
  await waitForVisibility("visible");
  assert.deepEqual(await driver.eval("visibilityChanges"), [["hidden", true], ["visible", false]]);
} finally {
  await driver.close();
}

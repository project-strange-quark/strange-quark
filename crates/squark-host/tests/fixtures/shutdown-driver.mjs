// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [binaryPath, pack] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath,
  args: [pack],
  width: 120,
  height: 100,
  timeoutMs: 5_000,
});

try {
  const deadline = Date.now() + 2_000;
  let state;
  while (true) {
    state = await driver.eval("({ canvasRequests: window.canvasRequests, layoutReads: window.layoutReads, pendingCanvasRequests: window.pendingCanvasRequests })");
    if (state?.canvasRequests >= 2) break;
    assert.ok(
      Date.now() < deadline,
      `page did not start its canvas/layout loop: ${JSON.stringify(state)}\n${driver.stderrText()}`,
    );
  }
  const quitAt = Date.now();
  await driver.request("app.quit", {}, 2_000);
  const child = driver._child;
  const exited = child.exitCode === null
    ? new Promise((resolve) => child.once("exit", (code, signal) => resolve({ code, signal })))
    : Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  let timer;
  let exit;
  try {
    exit = await Promise.race([
      exited,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("runtime did not exit within 5 seconds")), 5_000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }

  assert.ok(Date.now() - quitAt < 5_000, "runtime exit exceeded the 5 second bound");
  assert.deepEqual(exit, { code: 0, signal: null }, `runtime exit: ${JSON.stringify(exit)}\n${driver.stderrText()}`);
  assert.doesNotMatch(driver.stderrText(), /\bpanicked\b|\bpanic\b/i);
  assert.equal(fs.existsSync(driver._socketPath), false, "test-control socket was not removed");
} finally {
  await driver.close();
}
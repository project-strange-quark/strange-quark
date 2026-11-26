// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, openPath, outputDir] = process.argv.slice(2);
const savePath = path.join(outputDir, "saved.txt");
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("file-dialog") : path.join(outputDir, "squark.sock"),
  width: 120,
  height: 90,
  headless: true,
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

async function waitForFileSize(filePath, expectedSize) {
  for (let attempt = 0; attempt < 200; attempt++) {
    try {
      if (fs.statSync(filePath).size === expectedSize) return;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${filePath} to reach ${expectedSize} bytes`);
}

try {
  assert.deepEqual(
    await driver.request("fileDialog.queue", { kind: "open", path: openPath }),
    { ok: true },
  );
  await driver.eval("window.openPicker()");
  await waitFor("window.openResult?.settled === true");
  assert.deepEqual(await driver.eval("window.openResult"), {
    settled: true,
    name: "open-me.txt",
    size: Buffer.byteLength("opened from a queued dialog"),
    type: "text/plain",
    text: "opened from a queued dialog",
  });

  assert.deepEqual(
    await driver.request("fileDialog.queue", { kind: "save", path: savePath }),
    { ok: true },
  );
  await driver.eval("window.saveFile()");
  const savedBytes = Buffer.from("saved from a queued dialog\n");
  await waitForFileSize(savePath, savedBytes.length);
  assert.deepEqual(fs.readFileSync(savePath), savedBytes);

  await driver.eval("window.openPicker()");
  await waitFor("window.cancelCount === 1");
  assert.deepEqual(await driver.eval("window.dialogErrors"), []);
} finally {
  await driver.close();
}
// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, output, mode = "load"] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("blob") : path.join(output, "squark.sock"),
  width: 40,
  height: 30,
  headless: true,
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

try {
  await waitFor("window.blobLoadFired");
  assert.deepEqual(await driver.eval("[blobImage.naturalWidth, blobImage.naturalHeight]"), [3, 2]);
  if (mode === "revoke") {
    await driver.eval(`{
      URL.revokeObjectURL(blobUrl);
      window.revokedImageError = false;
      window.revokedImageLoad = false;
      const image = document.createElement("img");
      image.addEventListener("error", () => { window.revokedImageError = true; });
      image.addEventListener("load", () => { window.revokedImageLoad = true; });
      document.body.appendChild(image);
      image.src = blobUrl;
    }`);
    await waitFor("window.revokedImageError || window.revokedImageLoad");
    assert.deepEqual(await driver.eval("[window.revokedImageError, window.revokedImageLoad]"), [true, false]);
  }
} finally {
  await driver.close();
}
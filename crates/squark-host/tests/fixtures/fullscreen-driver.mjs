// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, output] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("fullscreen") : path.join(output, "squark.sock"),
  width: 160,
  height: 100,
  headless: true,
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

async function hostFullscreen() {
  return (await driver.request("window.fullscreen", {})).fullscreen;
}

try {
  await driver.waitForIdle();
  assert.deepEqual(await driver.eval("[document.fullscreenEnabled, document.fullscreenElement]"), [true, null]);
  assert.equal(await hostFullscreen(), false);

  // A request outside a user gesture is refused, as in Chromium.
  await driver.eval("document.documentElement.requestFullscreen().catch(e => log.push(`rejected:${e.name}`)), true");
  await waitFor("log.length === 2");
  assert.deepEqual(await driver.eval("log.splice(0)"), ["rejected:TypeError", "error:HTML"]);
  assert.equal(await hostFullscreen(), false);

  // A trusted click enters fullscreen through the host.
  await driver.click({ x: 20, y: 20 });
  await waitFor("log.length === 2");
  assert.deepEqual(await driver.eval("[document.fullscreenElement.nodeName, log.splice(0)]"),
    ["HTML", ["resolved", "change:HTML:HTML"]]);
  assert.equal(await hostFullscreen(), true);

  // A second click exits it.
  await driver.click({ x: 20, y: 20 });
  await waitFor("log.length === 2");
  assert.deepEqual(await driver.eval("[document.fullscreenElement, log.splice(0)]"),
    [null, ["resolved", "change:HTML:null"]]);
  assert.equal(await hostFullscreen(), false);

  // The window system leaves fullscreen on its own: the host sends the change.
  await driver.click({ x: 20, y: 20 });
  await waitFor("log.length === 2");
  assert.deepEqual(await driver.eval("log.splice(0)"), ["resolved", "change:HTML:HTML"]);
  assert.deepEqual(await driver.request("window.leaveFullscreen", {}), { changed: true });
  await waitFor("log.length === 1");
  assert.deepEqual(await driver.eval("[document.fullscreenElement, log.splice(0)]"), [null, ["change:HTML:null"]]);
  assert.equal(await hostFullscreen(), false);
  assert.deepEqual(await driver.request("window.leaveFullscreen", {}), { changed: false });

  assert.deepEqual(await driver.eval("errors"), []);
} finally {
  await driver.close();
}

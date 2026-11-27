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
  socketPath: process.platform === "win32" ? controlSocketPath("menu") : path.join(output, "squark.sock"),
  width: 160,
  height: 100,
  headless: true,
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}: ${JSON.stringify(await driver.eval("[log, errors]"))}`);
}

/** Settles a few frames so a key's decision and its menu task have run. */
async function settle() {
  await driver.waitForIdle();
  await sleep(100);
  await driver.waitForIdle();
}

try {
  await waitFor("log.includes('menu-set')");
  const { menu } = await driver.request("menu.get", {});
  assert.equal(menu[0].label, "&File");
  assert.deepEqual(menu[0].submenu.map(item => item.id ?? item.role ?? item.type),
    ["file.open", "file.save", "strange-quark-menu-1", "file.disabled", "separator", "quit"]);
  assert.equal(await driver.eval("log.splice(0).join()"), "menu-set");

  // An accelerator the page leaves alone runs its item.
  await driver.press("o", { primary: true });
  await waitFor("log.length === 1");
  assert.deepEqual(await driver.eval("log.splice(0)"), ["menu:file.open"]);

  // The page handles Ctrl+S in the editor and cancels it: no menu Save.
  await driver.eval("document.getElementById('editor').focus(), true");
  await driver.press("s", { primary: true });
  await settle();
  assert.deepEqual(await driver.eval("log.splice(0)"), ["editor:save"]);

  // Shifted accelerators, click callbacks, and disabled items.
  await driver.press("R", { primary: true, shift: true });
  await waitFor("log.length === 2");
  assert.deepEqual(await driver.eval("log.splice(0)"), ["click:reload", "menu:strange-quark-menu-1"]);
  await driver.press("d", { primary: true });
  await settle();
  assert.deepEqual(await driver.eval("log.splice(0)"), []);

  // Test control activates items as a click on the menu would.
  await driver.request("menu.activate", { id: "file.save" });
  await waitFor("log.length === 1");
  assert.deepEqual(await driver.eval("log.splice(0)"), ["menu:file.save"]);
  await assert.rejects(driver.request("menu.activate", { id: "file.disabled" }), /no enabled command/);

  // Removing the menu removes its accelerators.
  await driver.eval("globalThis[Symbol.for('strange-quark.native')].window.setMenu(null).then(() => log.push('cleared')), true");
  await waitFor("log.includes('cleared')");
  assert.equal((await driver.request("menu.get", {})).menu, null);
  await driver.eval("document.getElementById('editor').blur(), log.splice(0), true");
  await driver.press("o", { primary: true });
  await settle();
  assert.deepEqual(await driver.eval("[log, errors]"), [[], []]);
  console.log("menu driver: all checks passed");
} finally {
  await driver.close();
}

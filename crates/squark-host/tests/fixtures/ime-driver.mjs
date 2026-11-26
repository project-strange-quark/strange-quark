// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import path from "node:path";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, output] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("ime") : path.join(output, "squark.sock"),
  width: 240,
  height: 140,
  headless: true,
});

async function waitForFocus() {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await driver.eval("document.activeElement?.id === 'editor'")) return;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  assert.fail("textarea did not receive focus");
}

async function ime(kind, text, cursor) {
  const params = { kind, text };
  if (cursor !== undefined) params.cursor = cursor;
  assert.deepEqual(await driver.request("input.ime", params), { ok: true });
}

try {
  await driver.waitForIdle();
  await waitForFocus();
  await ime("preedit", "に");
  await ime("preedit", "日本", [6, 6]);
  await ime("commit", "日本");
  assert.deepEqual(await driver.eval("imeEvents"), [
    ["compositionstart", "", null],
    ["compositionupdate", "に", null],
    ["input", "に", true],
    ["compositionupdate", "日本", null],
    ["input", "日本", true],
    ["compositionupdate", "日本", null],
    ["input", "日本", true],
    ["compositionend", "日本", null],
  ]);
  assert.equal(await driver.eval("editor.value"), "日本");

  await driver.eval(`(() => {
    imeEvents.length = 0;
    editor.value = "";
    return true;
  })()`);
  await ime("preedit", "仮");
  await ime("end", "");
  assert.deepEqual(await driver.eval("imeEvents"), [
    ["compositionstart", "", null],
    ["compositionupdate", "仮", null],
    ["input", "仮", true],
    ["compositionupdate", "", null],
    ["input", null, true],
    ["compositionend", "", null],
  ]);
  assert.equal(await driver.eval("editor.value"), "");
} finally {
  await driver.close();
}
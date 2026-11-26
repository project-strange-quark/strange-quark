// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, png] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  width: 120,
  height: 90,
});

async function waitFor(expression) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (await driver.eval(expression)) return;
    await sleep(25);
  }
  assert.fail(`timed out waiting for ${expression}`);
}

try {
  assert.deepEqual(
    await driver.request("clipboard.write", {
      items: { mime: "text/plain", base64: Buffer.from("hello").toString("base64") },
    }),
    { ok: true },
  );
  await driver.eval("window.readClipboardText()");
  await waitFor("window.clipboardReadResult?.settled");
  assert.equal(await driver.eval("window.clipboardReadResult.value"), "hello");

  await driver.eval("window.writeClipboardText()");
  await waitFor("window.clipboardWriteResult?.settled");
  assert.deepEqual(await driver.request("clipboard.read", {}), {
    items: [{ mime: "text/plain", base64: Buffer.from("from page").toString("base64") }],
  });

  assert.deepEqual(
    await driver.request("clipboard.write", {
      items: [{ mime: "image/png", base64: png }],
    }),
    { ok: true },
  );
  await driver.eval("document.querySelector('input').focus()");
  await driver.request("input.key", {
    kind: "down",
    key: "v",
    code: "KeyV",
    modifiers: 2,
  });
  await driver.request("input.key", {
    kind: "up",
    key: "v",
    code: "KeyV",
    modifiers: 2,
  });
  await waitFor("window.pasteFiles !== null");
  assert.deepEqual(await driver.eval("window.pasteFiles"), [
    { type: "image/png", size: Buffer.from(png, "base64").byteLength },
  ]);
} finally {
  await driver.close();
}
// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack] = process.argv.slice(2);
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  width: 1000,
  height: 700,
  headless: false,
});
try {
  await driver.waitForIdle();
  const lines = await driver.eval(`(() => {
    document.body.style.cssText = "display:grid;grid-template-columns:1fr 1fr;margin:0;height:100vh";
    const field = document.createElement("textarea");
    field.style.cssText = "font:14px/1.5 monospace;padding:12px;resize:none";
    const preview = document.createElement("section");
    preview.style.cssText = "font:16px system-ui;overflow:auto;padding:12px";
    const sections = Array.from({length:150}, (_,i) => [
      "## Heading " + i, "", "A paragraph with a link to page " + i, "", "- first row", "- second row", "",
    ]);
    field.value = sections.flat().join("\\n");
    preview.innerHTML = sections.map((_,i) => '<h2>Heading ' + i + '</h2><p>A <em>paragraph</em> with a <a href="/page' + i + '">link</a> to page ' + i + '</p><ul><li>first row</li><li>second row</li></ul>').join("");
    document.body.replaceChildren(field, preview);
    return field.value.split("\\n").length;
  })()`);
  assert.equal(lines, 1050);
  const idle = await driver.waitForIdle(2);
  await sleep(350);
  assert.equal(await driver.waitForIdle(0), idle, "idle must stop creating document frames");
} finally {
  await driver.close();
}

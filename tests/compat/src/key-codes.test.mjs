// SPDX-License-Identifier: MIT
// The native driver must send the physical code Chromium reports for the same
// key. This test asks Chromium, for every key the drivers send, and compares
// with keyToCode().

import assert from "node:assert/strict";
import test from "node:test";
import { chromium } from "playwright-core";

import { findChromiumPath } from "../../../scripts/chromium-path.mjs";
import { keyToCode } from "./squark-driver.mjs";

export const KEY_TABLE = [
  // letters and digits
  "a", "e", "z", "A", "0", "5", "9",
  // unshifted punctuation
  "-", "=", "[", "]", "\\", ";", "'", ",", ".", "/", "`",
  // shifted punctuation maps to the unshifted physical key
  "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+", "{", "}", "|", ":", '"', "<", ">", "?", "~",
  // named keys
  " ", "Enter", "Escape", "Tab", "Backspace", "Delete", "Insert", "Home", "End",
  "PageUp", "PageDown", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
  "Shift", "Control", "Alt", "Meta", "CapsLock", "ContextMenu", "Pause", "PrintScreen", "ScrollLock",
  "F1", "F5", "F12",
  "Numpad0", "Numpad9", "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal", "NumpadEnter",
];

test("keyToCode matches Chromium's KeyboardEvent.code", { timeout: 120_000 }, async () => {
  const browser = await chromium.launch({
    executablePath: findChromiumPath(),
    headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
  });
  try {
    const page = await browser.newPage();
    await page.setContent('<div id="target" tabindex="0" style="width:100px;height:100px">focus</div>');
    await page.focus("#target");
    await page.evaluate(() => {
      window.__codes = [];
      document.addEventListener("keydown", (event) => {
        window.__codes.push({ key: event.key, code: event.code });
      });
    });

    const mismatches = [];
    for (const key of KEY_TABLE) {
      await page.evaluate(() => {
        window.__codes = [];
      });
      await page.keyboard.press(key);
      const [pressed] = await page.evaluate(() => window.__codes);
      assert.ok(pressed, `Chromium sent no keydown for ${JSON.stringify(key)}`);
      if (pressed.code !== keyToCode(key)) {
        mismatches.push({ key, chromium: pressed.code, driver: keyToCode(key) });
      }
    }
    assert.deepEqual(mismatches, [], `key code mismatches: ${JSON.stringify(mismatches, null, 2)}`);
  } finally {
    await browser.close().catch(() => undefined);
  }
});

test("clickCount is sent on pointer down and up, and doubles as 1 then 2", async () => {
  // The driver's protocol shape, without a runtime: the requests squark-driver
  // builds carry clickCount, and dblclick sends 1 then 2.
  const { SquarkDriver } = await import("./squark-driver.mjs");
  const calls = [];
  const driver = Object.create(SquarkDriver.prototype);
  driver._resolvePoint = async (point) => point;
  driver.request = async (method, params) => {
    calls.push({ method, params });
    return { ok: true };
  };
  await driver.click({ x: 10, y: 20 });
  assert.deepEqual(
    calls.map((call) => [call.params.kind, call.params.clickCount]),
    [
      ["down", 1],
      ["up", 1],
    ],
  );
  calls.length = 0;
  await driver.dblclick({ x: 30, y: 40 });
  assert.deepEqual(
    calls.map((call) => [call.params.kind, call.params.clickCount]),
    [
      ["down", 1],
      ["up", 1],
      ["down", 2],
      ["up", 2],
    ],
  );
});

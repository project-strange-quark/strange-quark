// SPDX-License-Identifier: MIT
// Shared interaction script for apps/counter (runs identically on ChromiumDriver and SquarkDriver).

import assert from "node:assert/strict";
import path from "node:path";

async function snapshotCounterState(driver) {
  const count = (await driver.queryText('[data-testid="count"]'))?.trim() ?? null;
  const historyCount = await driver.queryCount('[data-testid="history-item"]');
  const history = await driver.eval(
    `Array.from(document.querySelectorAll('[data-testid="history-item"]'), (el) => el.textContent.trim())`,
  );
  const step1Pressed = await driver.queryAttr('[data-testid="step-1"]', "aria-pressed");
  const step5Pressed = await driver.queryAttr('[data-testid="step-5"]', "aria-pressed");
  const step10Pressed = await driver.queryAttr('[data-testid="step-10"]', "aria-pressed");
  return { count, historyCount, history, step1Pressed, step5Pressed, step10Pressed };
}

export async function runCounterScript(driver, { screenshotDir = null } = {}) {
  const screenshots = {};
  const checkpoints = [];
  const snap = async (name) => {
    if (!screenshotDir) return;
    const file = path.join(screenshotDir, `${name}.png`);
    await driver.screenshot(file);
    screenshots[name] = file;
  };
  const record = async (name) => {
    const state = await snapshotCounterState(driver);
    checkpoints.push({ name, state });
    return state;
  };

  await driver.waitForSelector('[data-testid="count"]');
  await driver.waitForIdle();

  const initial = await record("initial");
  assert.equal(initial.count, "0");
  assert.equal(initial.historyCount, 0);
  await snap("counter-initial");

  // +1 three times -> 3
  await driver.click('[data-testid="inc"]');
  await driver.click('[data-testid="inc"]');
  await driver.click('[data-testid="inc"]');
  await driver.waitForIdle();
  const afterInc3 = await record("after-inc-3");
  assert.equal(afterInc3.count, "3");

  // -1 once -> 2
  await driver.click('[data-testid="dec"]');
  await driver.waitForIdle();
  const afterDec1 = await record("after-dec-1");
  assert.equal(afterDec1.count, "2");

  // Switch step to 5, increment twice -> 12
  await driver.click('[data-testid="step-5"]');
  await driver.waitForIdle();
  assert.equal(await driver.queryAttr('[data-testid="step-5"]', "aria-pressed"), "true");
  await driver.click('[data-testid="inc"]');
  await driver.click('[data-testid="inc"]');
  await driver.waitForIdle();
  const afterOps = await record("after-step-5-inc-2");
  assert.equal(afterOps.count, "12");
  assert.equal(afterOps.historyCount, 6);
  await snap("counter-after-ops");

  const countRect = await driver.queryRect('[data-testid="count"]');
  assert.ok(countRect && countRect.width > 0 && countRect.height > 0);

  // Reset -> 0 and empty history
  await driver.click('[data-testid="reset"]');
  await driver.waitForIdle();
  const afterReset = await record("after-reset");
  assert.equal(afterReset.count, "0");
  assert.equal(afterReset.historyCount, 0);

  return {
    finalCount: "0",
    countRect,
    checkpoints,
    screenshots,
  };
}

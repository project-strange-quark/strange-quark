// SPDX-License-Identifier: MIT
// Shared interaction script for apps/virtual-list (10,000 TanStack Virtual rows).
// Performs a scripted wheel fling and reads the frame timeline summary.

import assert from "node:assert/strict";
import path from "node:path";

/**
 * Polls a DOM condition through `dom.eval`. Used instead of `frame.waitIdle`
 * during the fling: a virtualizer that re-renders every frame never reports an
 * idle frame queue, so an idle wait can time out while the app is healthy.
 */
async function waitForCondition(driver, expression, { label, timeoutMs = 10_000, intervalMs = 16 }) {
  const deadline = performance.now() + timeoutMs;
  let last;
  while (performance.now() < deadline) {
    last = await driver.eval(expression);
    if (last) return last;
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error(`timed out waiting for ${label ?? expression} (last=${JSON.stringify(last)})`);
}

async function snapshotVirtualListState(driver) {
  const rowCountText = (await driver.queryText('[data-testid="row-count"]'))?.trim() ?? null;
  const mountedRows = await driver.queryCount('[data-testid="virtual-row"]');
  const scrollTop = await driver.eval(
    `Math.round(document.querySelector('[data-testid="scroll-container"]')?.scrollTop ?? 0)`,
  );
  const minIndex = await driver.eval(
    `(() => {
      const nodes = Array.from(document.querySelectorAll('[data-testid="virtual-row"]'));
      if (nodes.length === 0) return -1;
      return Math.min(...nodes.map((el) => Number(el.getAttribute("data-index"))));
    })()`,
  );
  const maxIndex = await driver.eval(
    `(() => {
      const nodes = Array.from(document.querySelectorAll('[data-testid="virtual-row"]'));
      if (nodes.length === 0) return -1;
      return Math.max(...nodes.map((el) => Number(el.getAttribute("data-index"))));
    })()`,
  );
  return { rowCountText, mountedRows, scrollTop, minIndex, maxIndex };
}

export async function runVirtualListScript(
  driver,
  { screenshotDir = null, flingSteps = 24, deltaYPerStep = 360 } = {},
) {
  const screenshots = {};
  const checkpoints = [];
  const snap = async (name) => {
    if (!screenshotDir) return;
    const file = path.join(screenshotDir, `${name}.png`);
    await driver.screenshot(file);
    screenshots[name] = file;
  };
  const record = async (name) => {
    const state = await snapshotVirtualListState(driver);
    checkpoints.push({ name, state });
    return state;
  };

  await driver.waitForSelector('[data-testid="scroll-container"]');
  await driver.waitForSelector('[data-testid="virtual-row"]');
  await driver.waitForIdle();

  const top = await record("top");
  assert.equal(top.rowCountText, "10,000 rows");
  assert.ok(top.mountedRows > 0 && top.mountedRows < 60, `expected virtualized DOM count < 60, got ${top.mountedRows}`);
  assert.equal(top.minIndex, 0);
  assert.equal(top.scrollTop, 0);
  await snap("virtual-list-top");

  const scrollerRect = await driver.queryRect('[data-testid="scroll-container"]');
  assert.ok(scrollerRect && scrollerRect.width > 100 && scrollerRect.height > 200);
  const cx = scrollerRect.x + scrollerRect.width / 2;
  const cy = scrollerRect.y + scrollerRect.height / 2;

  // Start frame-timeline recording and execute a scripted wheel fling down the 10k-row list.
  await driver.eval(`window.__squarkFrameTimeline?.start()`);
  let lastScrollTop = 0;
  for (let i = 0; i < flingSteps; i++) {
    await driver.wheel(cx, cy, 0, deltaYPerStep);
    // The virtualizer keeps animating, so wait for scroll progress, not idle.
    lastScrollTop = await waitForCondition(
      driver,
      `(() => {
        const top = document.querySelector('[data-testid="scroll-container"]')?.scrollTop ?? 0;
        return top > ${lastScrollTop} ? top : null;
      })()`,
      { label: `scroll advance on wheel step ${i}` },
    );
  }
  // Settle: the position and the rendered range must hold for two polls.
  await (async () => {
    const expression = `(() => {
      const top = Math.round(document.querySelector('[data-testid="scroll-container"]')?.scrollTop ?? 0);
      const rows = document.querySelectorAll('[data-testid="virtual-row"]').length;
      return { top, rows };
    })()`;
    let previous = await driver.eval(expression);
    const deadline = performance.now() + 10_000;
    while (performance.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      const current = await driver.eval(expression);
      if (current.top === previous.top && current.rows === previous.rows) return;
      previous = current;
    }
  })();
  const frameTimeline = await driver.eval(`window.__squarkFrameTimeline?.stop() ?? null`);

  const scrolled = await record("after-fling");
  assert.ok(scrolled.scrollTop >= 4000, `expected scrollTop >= 4000 after fling, got ${scrolled.scrollTop}`);
  assert.ok(scrolled.minIndex >= 90, `expected first rendered row >= 90, got ${scrolled.minIndex}`);
  assert.ok(scrolled.mountedRows > 0 && scrolled.mountedRows < 60, `virtualization broke after fling (${scrolled.mountedRows} rows)`);
  assert.ok(frameTimeline && frameTimeline.frameCount > 0, "expected frame timeline samples during fling");
  await snap("virtual-list-scrolled");

  return {
    checkpoints,
    frameTimeline,
    screenshots,
  };
}

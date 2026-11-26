// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack] = process.argv.slice(2);
assert.ok(process.env.WAYLAND_DISPLAY && process.env.HYPRLAND_INSTANCE_SIGNATURE);
const hyprctl = (...args) =>
  execFileSync("hyprctl", args, { encoding: "utf8", timeout: 1500 }).trim();
const monitors = JSON.parse(hyprctl("-j", "monitors"));
assert.ok(monitors.some((monitor) => monitor.dpmsStatus), "the test needs an active output");

const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack, "--gpu-adapter", "hardware"],
  headless: false,
  width: 800,
  height: 600,
  timeoutMs: 3500,
});

try {
  let window;
  for (let attempt = 0; attempt < 40 && !window; attempt++) {
    window = JSON.parse(hyprctl("-j", "clients")).find((client) => client.pid === driver._child.pid);
    if (!window) await sleep(20);
  }
  assert.ok(window && !window.xwayland, "runtime must map a native Wayland surface");
  const selector = JSON.stringify(`address:${window.address}`);
  const dispatch = (action) => assert.equal(hyprctl("eval", `hl.dispatch(${action})`), "ok");
  dispatch(`hl.dsp.window.float({window=${selector},action="set"})`);
  dispatch(`hl.dsp.window.set_prop({window=${selector},prop="idle_inhibit",value="always"})`);
  dispatch(`hl.dsp.window.set_prop({window=${selector},prop="no_anim",value="1"})`);

  // Widen the interval between the configure and the matching document commit.
  await driver.eval(`addEventListener("resize", () => {
    const until = performance.now() + 40;
    while (performance.now() < until) {}
  })`);
  const atSize = (png, width, height, dpr) =>
    Math.abs(png.readUInt32BE(16) - width * dpr) <= dpr &&
    Math.abs(png.readUInt32BE(20) - height * dpr) <= dpr;
  let resizedCaptures = 0;
  let previous = await driver.waitForIdle(2);
  for (let step = 0; step < 50; step++) {
    const width = 380 + (step % 5) * 77;
    const height = 280 + (step % 4) * 61;
    const settled = await driver.eval("[innerWidth,innerHeight,devicePixelRatio]");
    dispatch(`hl.dsp.window.resize({window=${selector},x=${width},y=${height},relative=false})`);
    await sleep(7);
    const early = await driver.screenshot();
    if (atSize(early, width, height, settled[2])) resizedCaptures++;
    else assert.ok(atSize(early, ...settled), `unexpected screenshot size at resize ${step + 1}`);
    let actual = settled;
    const deadline = Date.now() + 1300;
    const matchesSize = ([w, h]) => Math.abs(w - width) <= 1 && Math.abs(h - height) <= 1;
    while (!matchesSize(actual) && Date.now() < deadline) {
      await sleep(8);
      actual = await driver.eval("[innerWidth,innerHeight,devicePixelRatio]");
    }
    assert.ok(matchesSize(actual), `resize ${step + 1} requested ${width}x${height}, saw ${actual}`);
    const frame = await driver.waitForIdle(2);
    assert.ok(frame > previous, `resize ${step + 1} did not present a new frame`);
    previous = frame;
    const screenshot = await driver.screenshot();
    assert.ok(atSize(screenshot, ...actual));
  }
  assert.ok(resizedCaptures > 0, "a screenshot should overlap a pending resize");
  console.log(`${resizedCaptures}/50 immediate screenshots captured the new viewport`);
} finally {
  await driver.close();
}

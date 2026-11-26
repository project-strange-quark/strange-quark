// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { PNG } from "pngjs";

import { SquarkDriver, controlSocketPath } from "../../../../tests/compat/src/squark-driver.mjs";

const [runtime, pack, output] = process.argv.slice(2);
const headless = process.env.SQUARK_CONTROL_WINDOW !== "1";
const driver = await SquarkDriver.launch({
  binaryPath: runtime,
  args: [pack],
  // Windows has no Unix sockets: the runtime listens on a named pipe there.
  socketPath:
    process.platform === "win32" ? controlSocketPath("control") : path.join(output, "squark.sock"),
  width: 80,
  height: 60,
  dpr: 1.5,
  headless,
});

function pixel(png, x, y) {
  return [...png.data.subarray((y * png.width + x) * 4, (y * png.width + x) * 4 + 4)];
}

try {
  assert.ok(await driver.waitForIdle() >= 3);
  assert.equal(await driver.eval("location.pathname"), "/");
  assert.deepEqual(await driver.eval("startupPointerMedia"), [true, true, true, true]);
  assert.equal(await driver.eval("getComputedStyle(document.body).color"), "rgb(17, 34, 51)");
  const beforeProbe = await driver.waitForIdle(0);
  const firstPresented = await driver.request("frame.nextPresented", {});
  const secondPresented = await driver.request("frame.nextPresented", {});
  assert.ok(firstPresented.frameId > beforeProbe);
  assert.ok(secondPresented.frameId > firstPresented.frameId);
  assert.ok(Number.isFinite(firstPresented.presentedAtMs) && firstPresented.presentedAtMs > 0);
  assert.ok(secondPresented.presentedAtMs > firstPresented.presentedAtMs);
  const innerWidth = await driver.eval("innerWidth");
  const innerHeight = await driver.eval("innerHeight");
  if (headless) {
    assert.equal(innerWidth, 80);
    assert.equal(innerHeight, 60);
  }
  const dpr = await driver.eval("devicePixelRatio");
  const sample = Math.floor(6 * dpr);
  assert.deepEqual(await driver.queryRect("#clipped"), { x: 54, y: 4, width: 16, height: 12 });
  assert.equal(await driver.queryRect("#missing"), null);
  await driver.click({ x: 55, y: 8 });
  await driver.click({ x: 65, y: 8 });
  await driver.eval("document.getElementById('clipped').style.pointerEvents = 'none'");
  await driver.waitForIdle();
  await driver.click({ x: 55, y: 8 });
  await driver.click({ x: 6, y: 36 });
  await driver.press("K", { shift: true });
  await driver.wheel(6, 6, 0, 20);
  await driver.waitForIdle();

  const before = PNG.sync.read(await driver.screenshot());
  assert.equal(before.width, Math.round(innerWidth * dpr));
  assert.equal(before.height, Math.round(innerHeight * dpr));
  assert.deepEqual(pixel(before, sample, sample), [255, 0, 0, 255]);

  await driver.eval("cancelWheel = false");
  await driver.wheel(6, 6, 0, 20);
  await driver.waitForIdle();
  const screenshot = path.join(output, "scrolled.png");
  const after = PNG.sync.read(await driver.screenshot(screenshot));
  assert.deepEqual(PNG.sync.read(fs.readFileSync(screenshot)).data, after.data);
  assert.deepEqual(pixel(after, sample, sample), [0, 0, 255, 255]);
  assert.equal(await driver.eval("document.elementFromPoint(6, 6).id"), "after");
  assert.equal((await driver.queryRect("#after")).y, 4);
  assert.equal(await driver.eval("document.getElementById('scroller').scrollTop"), 20);
  await driver.click({ x: 6, y: 6 });
  assert.deepEqual(await driver.eval("actions"), ["clipped", ["K", true], [20, true], [20, true], "after"]);

  await driver.wheel(6, 6, 0, -20);
  await driver.waitForIdle();
  assert.equal((await driver.queryRect("#after")).y, 24);
  await driver.eval("document.getElementById('after').setAttribute('tabindex', '0'); document.getElementById('after').focus()");
  await driver.waitForIdle();
  assert.equal((await driver.queryRect("#after")).y, 4);
  assert.equal(await driver.eval("document.elementFromPoint(6, 6).id"), "after");
  assert.equal(await driver.eval("document.getElementById('scroller').scrollTop"), 20);
  assert.deepEqual(pixel(PNG.sync.read(await driver.screenshot()), sample, sample), [0, 0, 255, 255]);

  await driver.eval("document.getElementById('scroller').scrollTop = 0");
  await driver.waitForIdle();
  assert.equal(await driver.eval("document.elementFromPoint(6, 6).id"), "before");

  await driver.eval(`{
    const scroller = document.createElement("div");
    scroller.id = "sticky-scroll";
    scroller.style.cssText = "position:absolute;left:40px;top:26px;width:32px;height:26px;overflow:auto";
    scroller.innerHTML = '<div style="height:70px"><div style="height:6px"></div><div id="sticky" style="position:sticky;top:2px;height:6px;background:#ff0000"></div></div>';
    window.passiveWheels = [];
    scroller.addEventListener("wheel", event => passiveWheels.push(event.cancelable), {passive:true});
    document.body.appendChild(scroller);
  }`);
  await driver.waitForIdle();
  assert.equal((await driver.queryRect("#sticky")).y, 32);
  await driver.wheel(42, 36, 0, 20);
  await driver.waitForIdle();
  assert.deepEqual(await driver.eval("passiveWheels"), [false]);
  assert.equal(await driver.eval("document.getElementById('sticky-scroll').scrollTop"), 20);
  assert.equal((await driver.queryRect("#sticky")).y, 28);
  assert.equal(await driver.eval("document.elementFromPoint(44, 30).id"), "sticky");
  const sticky = PNG.sync.read(await driver.screenshot());
  assert.deepEqual(pixel(sticky, Math.floor(44 * dpr), Math.floor(30 * dpr)), [255, 0, 0, 255]);

  await driver.eval(`{
    const box = document.createElement("div");
    box.id = "motion-probe";
    box.style.cssText = "position:absolute;z-index:2;left:3px;top:46px;width:10px;height:9px;background:red;transition:transform 160ms linear,opacity 160ms linear,filter 160ms linear";
    window.motionEvents = [];
    box.addEventListener("transitionend", event => motionEvents.push(event.propertyName));
    document.body.appendChild(box);
  }`);
  await driver.waitForIdle();
  const atProbe = (png, x) => pixel(png, Math.floor(x * dpr), Math.floor(50 * dpr));
  const initialProbe = PNG.sync.read(await driver.screenshot());
  assert.deepEqual(atProbe(initialProbe, 8), [255, 0, 0, 255]);
  await driver.eval(`{
    const {style} = document.getElementById("motion-probe");
    style.transform = "translateX(32px)";
    style.opacity = "0.5";
    style.filter = "brightness(0.5)";
  }`);
  await driver.waitForIdle();
  const finalProbe = PNG.sync.read(await driver.screenshot());
  assert.deepEqual(atProbe(finalProbe, 8), [255, 255, 255, 255]);
  assert.ok(atProbe(finalProbe, 39).every((channel, i) => Math.abs(channel - [191, 127, 127, 255][i]) <= 1));
  assert.equal(await driver.eval("document.elementFromPoint(39, 50).id"), "motion-probe");
  assert.deepEqual(await driver.eval("motionEvents.sort()"), ["filter", "opacity", "transform"]);

  await driver.eval("startAnimation()");
  await driver.waitForIdle();
  assert.equal(await driver.eval("animations"), 4);
  assert.equal(await driver.eval("frameClockAligned"), true);
  const idle = await driver.waitForIdle();
  await sleep(80);
  assert.equal(await driver.waitForIdle(0), idle);
  await driver.eval("paintLater()");
  const deadline = Date.now() + 2000;
  const point = Math.floor(52 * dpr);
  for (;;) {
    const latest = PNG.sync.read(await driver.screenshot());
    const color = pixel(latest, point, point);
    if (color[0] === 0x22) {
      assert.deepEqual(color, [0x22, 0x55, 0x77, 255]);
      break;
    }
    assert.ok(Date.now() < deadline, "off-frame timer did not wake the clock");
    await sleep(25);
  }
} finally {
  await driver.close();
}

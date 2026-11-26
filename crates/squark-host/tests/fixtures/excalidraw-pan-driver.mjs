// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { repoRoot } from "../../../../tests/excalidraw/src/build.mjs";
import { NativeExcalidraw } from "../../../../tests/excalidraw/src/flow/drivers.mjs";
import { elementTypes, waitForScene } from "../../../../tests/excalidraw/src/flow/state.mjs";
import { thousandElements } from "../../../../tests/excalidraw/src/measure.mjs";

const [binary, output = path.join(repoRoot, "target/excalidraw-pan")] = process.argv.slice(2);
assert.ok(binary, "pass the path to the packed Excalidraw binary");
function currentCpuWeight() {
  try {
    const cgroup = fs.readFileSync("/proc/self/cgroup", "utf8").trim().split("::")[1];
    if (!cgroup) return null;
    const raw = fs.readFileSync(path.join("/sys/fs/cgroup", cgroup, "cpu.weight"), "utf8").trim();
    const weight = Number(raw);
    return Number.isFinite(weight) ? weight : null;
  } catch {
    return null;
  }
}
const cpuWeight = currentCpuWeight();
if (process.platform === "linux") {
  assert.equal(cpuWeight, 10000, "run in a systemd user scope with CPUWeight=10000");
}

const work = path.resolve(output);
fs.mkdirSync(work, { recursive: true });
const timeline = path.join(work, "timeline.json");
fs.rmSync(timeline, { force: true });
process.env.SQUARK_FRAME_TIMELINE = timeline;
process.env.SQUARK_FRAME_TIMELINE_HZ = "60";

const elements = thousandElements();
const template = JSON.parse(fs.readFileSync(
  path.join(repoRoot, "tests/excalidraw/fixtures/01-basic-shapes.excalidraw"), "utf8",
));
const scene = path.join(work, "1000.excalidraw");
fs.writeFileSync(scene, JSON.stringify({ ...template, elements }));
const xdg = fs.mkdtempSync(path.join(work, "xdg-"));

const loadAverage = () => os.loadavg().slice(0, 3);

let driver;
let loadStart, loadEnd, scrollStart, scrollEnd, zoom, before, after;
const slots = 300;
const period = 1000 / 60;
const wheelPoint = { x: 640, y: 400 };
const wheelDelta = 38;
// The scripted pan: 41 periods down, 41 up, and so on.
const deltas = Array.from({ length: slots }, (_, index) =>
  (Math.floor(index / 41) % 2 === 0 ? 1 : -1) * wheelDelta);
const reasons = [];

// Facts the run needs to be valid, read from the page itself: the viewport,
// the element under the scripted wheel point, and trusted wheel events seen.
const PAGE_PROBE = `(() => {
  const target = document.elementFromPoint(${wheelPoint.x}, ${wheelPoint.y});
  return {
    viewport: [innerWidth, innerHeight],
    wheelTarget: target ? target.nodeName.toLowerCase() + (target.className ? "." + String(target.className).split(" ")[0] : "") : null,
    wheelOnCanvas: target instanceof HTMLCanvasElement,
    wheels: window.__squarkPanWheels?.all ?? null,
    canvasWheels: window.__squarkPanWheels?.canvas ?? null,
  };
})()`;
try {
  driver = await NativeExcalidraw.launch({
    binaryPath: path.resolve(binary),
    workDir: work,
    xdgDataHome: xdg,
    headless: false,
    gpuAdapter: "hardware",
    timeoutMs: 20_000,
  });
  await driver.openFile(scene);
  const opened = await waitForScene(driver, (state) => elementTypes(state).length === elements.length, {
    label: "the opened 1,000-element scene",
  });
  scrollStart = opened.scene.appState.scrollY;
  zoom = opened.scene.appState.zoom?.value ?? 1;
  await sleep(500);
  await driver.eval(`(() => {
    window.__squarkPanWheels = { all: 0, canvas: 0 };
    addEventListener("wheel", event => {
      if (!event.isTrusted) return;
      __squarkPanWheels.all++;
      if (event.target instanceof HTMLCanvasElement) __squarkPanWheels.canvas++;
    }, { capture: true, passive: true });
    return true;
  })()`);
  before = await driver.eval(PAGE_PROBE);

  loadStart = loadAverage();
  const start = performance.now();
  for (let index = 0; index < slots; index++) {
    const wait = start + index * period - performance.now();
    if (wait > 0) await sleep(wait);
    await driver.wheel(wheelPoint.x, wheelPoint.y, 0, deltas[index]);
  }
  await sleep(Math.max(0, start + slots * period - performance.now()));
  loadEnd = loadAverage();
  after = await driver.eval(PAGE_PROBE);
  // Excalidraw scrolls by -deltaY / zoom for a plain wheel. It persists the
  // scene on a debounce, so poll for the expected position, then judge it.
  const expected = scrollStart - deltas.reduce((sum, delta) => sum + delta, 0) / zoom;
  let panned;
  try {
    panned = await waitForScene(driver, (state) =>
      elementTypes(state).length === elements.length
      && Math.abs(state.scene.appState?.scrollY - expected) <= 1, {
      label: "the scene at the expected scroll", timeoutMs: 5_000,
    });
  } catch {
    panned = await driver.state();
  }
  scrollEnd = panned.scene?.appState?.scrollY ?? null;
} finally {
  if (driver) {
    fs.writeFileSync(path.join(work, "runtime.log"), driver.driver.stderrText());
    await driver.close();
  }
  fs.rmSync(xdg, { recursive: true, force: true });
}

const recorded = JSON.parse(fs.readFileSync(timeline, "utf8"));
const [firstWheel] = recorded.wheelNs;
const active = recorded.frames.filter((frame) =>
  frame.vsyncNs >= firstWheel && frame.vsyncNs < firstWheel + slots * period * 1e6,
);
const expectedScroll = scrollStart - deltas.reduce((sum, delta) => sum + delta, 0) / zoom;
const epochs = [...new Set(active.map((frame) => frame.epoch))];

// A run is valid only if exactly the scripted input reached the canvas, the
// window kept one size, and the scene ended where the script puts it.
if (recorded.wheelNs.length !== slots) {
  reasons.push(`the host received ${recorded.wheelNs.length} wheel events, not the ${slots} sent`);
}
if (after.wheels !== slots) reasons.push(`the page saw ${after.wheels} wheel events, not the ${slots} sent`);
if (after.canvasWheels !== slots) {
  reasons.push(`${after.canvasWheels} of ${slots} wheel events targeted the canvas`);
}
if (!before.wheelOnCanvas) reasons.push(`the wheel point hit ${before.wheelTarget}, not the canvas`);
if (String(before.viewport) !== String(after.viewport) || epochs.length !== 1) {
  reasons.push(`the viewport changed during the pan (${before.viewport} to ${after.viewport}, epochs ${epochs})`);
}
if (scrollEnd == null || Math.abs(scrollEnd - expectedScroll) > 1) {
  reasons.push(`the scene scrolled to ${scrollEnd}, not the expected ${expectedScroll}`);
}
const valid = reasons.length === 0;
const submitted = active.filter((frame) => frame.submitNs != null);
const fresh = submitted.filter((frame) => frame.commitSequence === frame.sequence);
const onTime = fresh.filter((frame) => frame.commitNs != null && frame.commitNs <= frame.deadlineNs);
// Count one fresh commit per target period. Reusing a commit from an earlier
// tick leaves this tick unserved, even if the surface submits on time.
const onTimeSlots = new Set(onTime.map((frame) =>
  Math.round((frame.vsyncNs - active[0].vsyncNs) / (period * 1e6)),
).filter((slot) => slot >= 0 && slot < slots));
const p95 = (values) => {
  if (!values.length) return null;
  values.sort((a, b) => a - b);
  return Number(values[Math.floor(0.95 * values.length)].toFixed(3));
};
const stageP95 = (frames, end, start) => p95(frames
  .filter((frame) => frame[end] != null && frame[start] != null)
  .map((frame) => (frame[end] - frame[start]) / 1e6));
const result = {
  adapter: recorded.adapter, cpuWeight, loadStart, loadEnd,
  elements: elements.length, scrollStart, scrollEnd, expectedScroll, slots,
  wheelEvents: recorded.wheelNs.length, pageWheelEvents: after.wheels, canvasWheelEvents: after.canvasWheels,
  viewport: after.viewport, wheelTarget: before.wheelTarget, valid, invalidReasons: reasons,
  submitted: submitted.length,
  fresh: fresh.length, onTimeSlots: onTimeSlots.size, deadlineRatio: onTimeSlots.size / slots,
  commitLatenessP95Ms: stageP95(fresh, "commitNs", "deadlineNs"),
  tickToCommitP95Ms: stageP95(fresh, "commitNs", "vsyncNs"),
  latchAndEncodeP95Ms: stageP95(submitted, "encodeEndNs", "latchNs"),
  submitIntervalP95Ms: p95(submitted.slice(1).map((frame, index) =>
    (frame.submitNs - submitted[index].submitNs) / 1e6,
  )),
  passed: valid && onTimeSlots.size / slots >= 0.95,
};
const report = JSON.stringify(result, null, 2);
console.log(report);
fs.writeFileSync(path.join(work, "summary.json"), report + "\n");
process.exitCode = result.passed ? 0 : 1;

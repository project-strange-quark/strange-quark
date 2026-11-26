// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../tests/compat/src/squark-driver.mjs";

const HZ = 60;
const SECONDS = 5;
const EXPECTED = HZ * SECONDS;
const SCROLLER = '[data-testid="scroll-container"]';
const ROW = '[data-testid="virtual-row"]';
const [executable, tracePath = "target/virtual-list-timeline.json"] = process.argv.slice(2);
assert(executable, "usage: measure-fling.mjs <squark-built virtual-list> [timeline.json]");
const timeline = path.resolve(tracePath);
fs.mkdirSync(path.dirname(timeline), { recursive: true });
const profile = fs.mkdtempSync(path.join(path.dirname(timeline), "fling-profile-"));
let driver;
let before;
let after;
try {
  driver = await SquarkDriver.launch({
    binaryPath: path.resolve(executable),
    width: 800,
    height: 600,
    gpuAdapter: "hardware",
    headless: false,
    env: {
      XDG_DATA_HOME: profile,
      SQUARK_FRAME_TIMELINE: timeline,
      SQUARK_FRAME_TIMELINE_HZ: String(HZ),
    },
  });
  await driver.waitForSelector(ROW);
  await driver.waitForIdle();
  const rect = await driver.queryRect(SCROLLER);
  assert(rect?.width > 100 && rect.height > 200, "virtual list needs a visible scrollport");
  const readState = () => driver.eval(`(() => {
    const rows = [...document.querySelectorAll('${ROW}')];
    return {
      scrollTop: document.querySelector('${SCROLLER}').scrollTop,
      mountedRows: rows.length,
      firstRow: Number(rows[0]?.dataset.index ?? -1),
    };
  })()`);
  before = await readState();
  assert.equal(before.scrollTop, 0);
  assert.equal(before.firstRow, 0);

  const x = rect.x + rect.width / 2;
  const y = rect.y + rect.height / 2;
  const start = performance.now();
  for (let i = 0; i < EXPECTED; i++) {
    const delay = start + i * 1000 / HZ - performance.now();
    if (delay > 0) await sleep(delay);
    await driver.wheel(x, y, 0, 360);
  }
  await driver.waitForIdle(2);
  after = await readState();
  assert(after.scrollTop >= 4000 && after.firstRow >= 90, JSON.stringify(after));
  assert(before.mountedRows > 0 && before.mountedRows < 60);
  assert(after.mountedRows > 0 && after.mountedRows < 60);
} finally {
  if (driver) await driver.close();
  fs.rmSync(profile, { recursive: true, force: true });
}

const recording = JSON.parse(fs.readFileSync(timeline, "utf8"));
assert.equal(recording.dropped, 0, "timeline overflowed");
assert.equal(recording.wheelNs.length, EXPECTED);
const start = recording.wheelNs[0];
const end = start + SECONDS * 1e9;
const frames = recording.frames.filter((frame) => frame.vsyncNs >= start && frame.vsyncNs < end);
const submitted = frames.filter((frame) => frame.submitNs !== null).length;
const onTime = frames.filter((frame) =>
  frame.submitNs !== null && frame.commitSequence === frame.sequence && frame.commitNs <= frame.deadlineNs
).length;
const ticks = new Map(recording.frames.map((frame) => [frame.sequence, frame]));
const commits = new Map(frames
  .filter((frame) => frame.commitSequence !== null && ticks.has(frame.commitSequence))
  .map((frame) => [frame.commitSequence, frame]));
const p95 = (samples) => {
  const sorted = samples.sort((a, b) => a - b);
  return sorted[Math.ceil(sorted.length * 0.95) - 1] ?? null;
};
const commitMs = [...commits].map(([sequence, frame]) => (frame.commitNs - ticks.get(sequence).vsyncNs) / 1e6);
const paintMs = [...commits.values()].map((frame) => (frame.commitNs - frame.paintStartNs) / 1e6);
const onTimeRatio = onTime / Math.max(EXPECTED, frames.length);
const targetAdapter = /\bRX 9060 XT\b/i.test(recording.adapter);
const result = {
  adapter: recording.adapter,
  seconds: SECONDS,
  expectedFrames: EXPECTED,
  frameCount: frames.length,
  submitted,
  fps: submitted / SECONDS,
  onTime,
  onTimeRatio,
  commitP95Ms: p95(commitMs),
  paintP95Ms: p95(paintMs),
  before,
  after,
  pass: targetAdapter && submitted >= EXPECTED - 1 && onTimeRatio >= 0.95,
  timeline,
};
console.error(
  `${submitted} frames presented, ${onTime}/${Math.max(EXPECTED, frames.length)} deadlines met ` +
  `(${(onTimeRatio * 100).toFixed(2)}%), commit p95 ${result.commitP95Ms?.toFixed(3) ?? "unavailable"} ms`,
);
if (!targetAdapter) {
  console.error(`This benchmark needs an RX 9060 XT. Wgpu selected ${recording.adapter}.`);
}
console.log(JSON.stringify(result, null, 2));
process.exitCode = result.pass ? 0 : 1;

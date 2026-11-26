// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { SquarkDriver } from "../../../tests/compat/src/squark-driver.mjs";

const [runtime, input, out] = process.argv.slice(2).map(file => path.resolve(file));
assert(out, "usage: measure-canvas.mjs <squark-runtime> <scene.sqpak> <output-dir>");
fs.mkdirSync(out, { recursive: true });
const timeline = path.join(out, "timeline.json");
fs.rmSync(timeline, { force: true });

const HZ = 60, SECONDS = 5, EXPECTED = HZ * SECONDS;
const load = () => os.loadavg().slice(0, 3);
function currentCpuWeight() {
  try {
    const cgroup = fs.readFileSync("/proc/self/cgroup", "utf8").trim().split("::").at(-1);
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
  assert.equal(cpuWeight, 10_000, "run the benchmark in the systemd user scope");
}

let before, after, loadBefore, loadAfter;
const profile = fs.mkdtempSync(path.join(out, "profile-"));
let driver;
try {
  driver = await SquarkDriver.launch({
    binaryPath: runtime, args: [input], width: 1280, height: 800,
    gpuAdapter: "hardware", headless: false, timeoutMs: 35_000,
    env: {
      XDG_DATA_HOME: profile,
      SQUARK_FRAME_TIMELINE: timeline,
      SQUARK_FRAME_TIMELINE_HZ: String(HZ),
    },
  });
  const startupDeadline = performance.now() + 90_000;
  do {
    before = await driver.eval("window.benchmark ?? null");
    if (before?.ready) break;
    await sleep(100);
  } while (performance.now() < startupDeadline);
  assert.equal(before?.cached, 1000, "expected 1,000 cached element bitmaps");
  assert.equal(before.maxVisible, 1000, "all 1,000 bitmaps must be visible at fit zoom");
  assert.deepEqual(before.sample, [165, 216, 255, 255], "the first cached shape must render");
  await driver.waitForIdle();
  const viewport = await driver.queryRect("#scene");
  const x = viewport.x + viewport.width / 2;
  const y = viewport.y + viewport.height / 2;
  loadBefore = load();
  const start = performance.now();
  for (let index = 0; index < EXPECTED; index++) {
    const delay = start + index * 1000 / HZ - performance.now();
    if (delay > 0) await sleep(delay);
    const direction = Math.floor(index / 25) % 2 ? -1 : 1;
    if (index < EXPECTED / 2) {
      await driver.wheel(x, y, direction * 4, direction * 3);
    } else {
      await driver.wheel(x, y, 0, direction * 10, { ctrl: true });
    }
  }
  loadAfter = load();
  await driver.waitForIdle();
  after = await driver.eval("window.benchmark");
  assert.equal(after.panEvents, EXPECTED / 2);
  assert.equal(after.zoomEvents, EXPECTED / 2);
  assert(after.maxZoom - after.minZoom >= .1, JSON.stringify(after));
} finally {
  await driver?.close();
  fs.rmSync(profile, { recursive: true, force: true });
}

const recording = JSON.parse(fs.readFileSync(timeline, "utf8"));
assert.equal(recording.dropped, 0);
assert.equal(recording.wheelNs.length, EXPECTED);
const p95 = samples => {
  const ordered = [...samples].sort((a, b) => a - b);
  return ordered[Math.ceil(ordered.length * .95) - 1] ?? null;
};
function summarize(start, end) {
  const frames = recording.frames.filter(frame => frame.vsyncNs >= start && frame.vsyncNs < end);
  const submitted = frames.filter(frame => frame.submitNs !== null);
  const commits = submitted.filter(frame => frame.commitSequence === frame.sequence);
  const onTime = commits.filter(frame => frame.commitNs <= frame.deadlineNs).length;
  const seconds = (end - start) / 1e9;
  const expected = Math.round(seconds * HZ);
  return {
    frames: frames.length, submitted: submitted.length, fresh: commits.length, onTime,
    fps: submitted.length / seconds, onTimeRatio: onTime / Math.max(expected, frames.length),
    commitP95Ms: p95(commits.map(frame => (frame.commitNs - frame.vsyncNs) / 1e6)),
    encodeP95Ms: p95(submitted.map(frame => (frame.encodeEndNs - frame.latchNs) / 1e6)),
  };
}
const start = recording.wheelNs[0];
const end = start + SECONDS * 1e9;
const mid = start + (end - start) / 2;
const all = summarize(start, end);
const pan = summarize(start, mid);
const zoom = summarize(mid, end);
const passed = /\bRX 9060 XT\b/i.test(recording.adapter) &&
  all.submitted >= EXPECTED - 1 && pan.onTimeRatio >= .95 && zoom.onTimeRatio >= .95;
const windowSystem = process.env.WAYLAND_DISPLAY ? "wayland" : "x11";
const report = { adapter: recording.adapter, windowSystem, cpuWeight, loadBefore, loadAfter, before, after, all, pan, zoom, passed };
fs.writeFileSync(path.join(out, "summary.json"), JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report, null, 2));
process.exitCode = passed ? 0 : 1;

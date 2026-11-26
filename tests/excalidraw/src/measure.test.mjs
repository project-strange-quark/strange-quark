// SPDX-License-Identifier: MIT
// Unit tests for the Excalidraw measurement parsers and helpers (no window required).

import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import {
  formatPanResult,
  formatPanSeries,
  medianOf,
  panDriverPath,
  parseStartupProfile,
  summarizePhaseMedians,
  thousandElements,
} from "./measure.mjs";

test("parseStartupProfile extracts squark-startup phase durations and aggregates medians", () => {
  const sample1 = [
    "squark-startup pack-v8-and-event-loop 14.250 ms",
    "squark-startup initial-frame-tick 0.120 ms",
    "squark-startup instance-and-surface 24.500 ms",
    "squark-startup document-bootstrap-and-first-commit 85.400 ms",
    "squark-runtime presented its first frame",
  ].join("\n");
  const sample2 = [
    "squark-startup pack-v8-and-event-loop 12.100 ms",
    "squark-startup initial-frame-tick 0.100 ms",
    "squark-startup instance-and-surface 22.000 ms",
    "squark-startup document-bootstrap-and-first-commit 60.000 ms",
  ].join("\n");
  const sample3 = [
    "squark-startup pack-v8-and-event-loop 13.000 ms",
    "squark-startup initial-frame-tick 0.110 ms",
    "squark-startup instance-and-surface 23.000 ms",
    "squark-startup document-bootstrap-and-first-commit 70.000 ms",
  ].join("\n");

  const parsed = [sample1, sample2, sample3].map((stderr) => ({
    phases: parseStartupProfile(stderr),
  }));
  assert.equal(parsed[0].phases["pack-v8-and-event-loop"], 14.25);
  assert.equal(parsed[0].phases["document-bootstrap-and-first-commit"], 85.4);

  const medians = summarizePhaseMedians(parsed);
  assert.deepEqual(medians, {
    "pack-v8-and-event-loop": 13,
    "initial-frame-tick": 0.11,
    "instance-and-surface": 23,
    "document-bootstrap-and-first-commit": 70,
  });
  assert.equal(medianOf([205.6, 128.6, 179.7, 153.8, 184.0]), 179.7);
});

test("formatPanResult preserves honest failure below 95% and links the pan driver", () => {
  assert.equal(fs.existsSync(panDriverPath), true, "the pan driver must exist");

  const failing = formatPanResult(
    {
      adapter: "AMD Radeon RX 9060 XT (RADV GFX1200)",
      cpuWeight: 10000,
      loadStart: [1.5, 1.2, 1.0],
      loadEnd: [1.8, 1.3, 1.0],
      elements: 1000,
      scrollStart: 0,
      scrollEnd: -418,
      slots: 300,
      wheelEvents: 300,
      submitted: 280,
      fresh: 230,
      onTimeSlots: 228,
      deadlineRatio: 228 / 300,
      commitLatenessP95Ms: 42.5,
      tickToCommitP95Ms: 59.1,
      latchAndEncodeP95Ms: 3.2,
      submitIntervalP95Ms: 17.1,
      passed: false,
    },
    { reportOnly: true },
  );
  assert.equal(failing.status, "measured");
  assert.equal(failing.driver, "crates/squark-host/tests/fixtures/excalidraw-pan-driver.mjs");
  assert.equal(failing.reportOnly, true);
  assert.equal(failing.deadlineRatio, 0.76);
  assert.equal(failing.passed, false);

  const passing = formatPanResult(
    {
      slots: 300,
      onTimeSlots: 290,
      deadlineRatio: 290 / 300,
      valid: true,
      invalidReasons: [],
      passed: true,
    },
    { reportOnly: false },
  );
  assert.equal(passing.reportOnly, false);
  assert.equal(passing.valid, true);
  assert.equal(passing.passed, true);
});

test("formatPanResult never passes a run the driver did not judge valid", () => {
  // Run 6 of the 2026-11-26 measurements: a window resize moved the wheel
  // point off the canvas, yet 287 of 300 periods were on time.
  const invalid = formatPanResult(
    {
      slots: 300,
      onTimeSlots: 287,
      deadlineRatio: 287 / 300,
      scrollStart: 254,
      scrollEnd: 140,
      expectedScroll: -810,
      wheelEvents: 300,
      pageWheelEvents: 300,
      canvasWheelEvents: 3,
      valid: false,
      invalidReasons: ["the scene scrolled to 140, not the expected -810"],
      passed: false,
    },
    { reportOnly: false },
  );
  assert.equal(invalid.valid, false);
  assert.equal(invalid.passed, false);
  assert.deepEqual(invalid.invalidReasons, ["the scene scrolled to 140, not the expected -810"]);

  // A summary from before the validity checks claims nothing it cannot show.
  const unchecked = formatPanResult({ slots: 300, onTimeSlots: 299, deadlineRatio: 299 / 300, passed: true });
  assert.equal(unchecked.valid, false);
  assert.equal(unchecked.passed, false);
  assert.deepEqual(unchecked.invalidReasons, ["the driver did not report the run as valid"]);
});

test("formatPanSeries gates on all three runs being valid and >= 95%", () => {
  const mk = (onTimeSlots, valid = true) =>
    formatPanResult({
      slots: 300,
      onTimeSlots,
      deadlineRatio: onTimeSlots / 300,
      wheelEvents: 300,
      pageWheelEvents: 300,
      canvasWheelEvents: 300,
      scrollStart: 254,
      scrollEnd: -810,
      expectedScroll: -810,
      loadStart: [1, 1, 1],
      loadEnd: [1, 1, 1],
      valid,
      invalidReasons: valid ? [] : ["extra wheel event"],
      passed: valid && onTimeSlots / 300 >= 0.95,
    });

  const allPass = formatPanSeries([mk(297), mk(300), mk(294)]);
  assert.equal(allPass.reportOnly, false);
  assert.equal(allPass.valid, true);
  assert.equal(allPass.passed, true);
  assert.equal(allPass.runCount, 3);
  assert.equal(allPass.minDeadlineRatio, 0.98);
  assert.equal(allPass.medianDeadlineRatio, 0.99);

  const oneBelow = formatPanSeries([mk(300), mk(284), mk(300)]);
  assert.equal(oneBelow.valid, true);
  assert.equal(oneBelow.passed, false);

  const oneInvalid = formatPanSeries([mk(300), mk(300, false), mk(300)]);
  assert.equal(oneInvalid.valid, false);
  assert.equal(oneInvalid.passed, false);
  assert.deepEqual(oneInvalid.invalidReasons, ["run 2: extra wheel event"]);
});

test("thousandElements builds 1,000 deterministic scene elements", () => {
  const elements = thousandElements();
  assert.equal(elements.length, 1000);
  assert.equal(elements[0].id, "element-0");
  assert.equal(elements[999].id, "element-999");
});

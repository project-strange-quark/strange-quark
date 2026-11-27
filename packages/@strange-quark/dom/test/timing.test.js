// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { installTiming } from "@strange-quark/dom";

/** A timing realm on a clock the test moves by hand. */
function timing(start = 0) {
  const clock = { now: start, armed: null };
  const global = {};
  const loop = installTiming(global, {
    now: () => clock.now,
    armTimer(deadline) { clock.armed = deadline; },
    queueMicrotask: callback => queueMicrotask(callback),
    postTask() {},
    cloneMessage: value => structuredClone(value),
  });
  return { clock, global, loop };
}

test("requestIdleCallback queues callbacks for idle periods and hands them an IdleDeadline", () => {
  const { clock, global, loop } = timing(100);
  assert.throws(() => global.requestIdleCallback(), TypeError);
  assert.throws(() => new global.IdleDeadline(), TypeError);
  assert.equal(loop.idleWakeup(), null);
  const seen = [];
  const first = global.requestIdleCallback(function (deadline) {
    assert.equal(this, global);
    assert(deadline instanceof global.IdleDeadline);
    assert.equal(Object.prototype.toString.call(deadline), "[object IdleDeadline]");
    seen.push(["first", deadline.timeRemaining(), deadline.didTimeout]);
    clock.now += 45;
    seen.push(["remaining", deadline.timeRemaining()]);
  });
  const cancelled = global.requestIdleCallback(() => seen.push(["cancelled"]));
  const last = global.requestIdleCallback(deadline => seen.push(["last", deadline.timeRemaining()]));
  assert.deepEqual([first, cancelled, last], [1, 2, 3]);
  global.cancelIdleCallback(cancelled);
  assert.equal(loop.idleWakeup(), 0);
  loop.beginIdle(140);
  for (let callback = loop.nextIdle(); callback !== undefined; callback = loop.nextIdle()) callback();
  assert.deepEqual(seen, [["first", 40, false], ["remaining", 0]], "the deadline passed before the last callback");
  assert.equal(loop.idleWakeup(), 140, "the remaining callback waits for the next period");
  clock.now = 150;
  loop.beginIdle(190);
  loop.nextIdle()();
  assert.deepEqual(seen.at(-1), ["last", 40]);
  assert.equal(loop.idleWakeup(), null);
});

test("performance records User Timing marks and measures", () => {
  const before = Date.now();
  const { clock, global } = timing(1000);
  const { performance } = global;
  assert(performance instanceof global.Performance);
  assert.equal(Object.prototype.toString.call(performance), "[object Performance]");
  assert.throws(() => new global.Performance(), TypeError);
  assert.throws(() => new global.PerformanceEntry(), TypeError);
  assert.throws(() => new global.PerformanceMeasure(), TypeError);
  assert(Math.abs(performance.timeOrigin + 1000 - before) < 1000);
  assert.deepEqual(performance.toJSON(), { timeOrigin: performance.timeOrigin });
  assert.equal(performance.now(), 1000);

  const detail = { nested: [1] };
  const start = performance.mark("start", { detail });
  assert(start instanceof global.PerformanceMark && start instanceof global.PerformanceEntry);
  assert.deepEqual([start.name, start.entryType, start.startTime, start.duration], ["start", "mark", 1000, 0]);
  assert.deepEqual(start.detail, detail);
  assert.notEqual(start.detail, detail, "detail is a structured clone");
  assert.equal(performance.mark("untimed").detail, null);
  assert.throws(() => performance.mark("navigationStart"), { name: "SyntaxError" });
  assert.throws(() => performance.mark("x", { startTime: -1 }), TypeError);
  assert.throws(() => performance.mark("x", { detail: () => {} }), { name: "DataCloneError" });
  const back = new global.PerformanceMark("back", { startTime: 500 });
  assert.equal(back.startTime, 500);
  assert.equal(performance.getEntriesByName("back").length, 0, "a constructed mark is not recorded");

  clock.now = 1250;
  performance.mark("end");
  const between = performance.measure("between", "start", "end");
  assert.deepEqual([between.entryType, between.startTime, between.duration, between.detail], ["measure", 1000, 250, null]);
  assert.deepEqual(performance.measure("since start", "start").duration, 250);
  assert.deepEqual(performance.measure("whole").startTime, 0);
  const timed = performance.measure("timed", { start: 1100, duration: 50, detail: "d" });
  assert.deepEqual([timed.startTime, timed.duration, timed.detail], [1100, 50, "d"]);
  const ending = performance.measure("ending", { end: "end", duration: 100 });
  assert.deepEqual([ending.startTime, ending.duration], [1150, 100]);
  assert.equal(performance.measure("origin", "navigationStart", "start").duration, 1000);
  assert.throws(() => performance.measure("missing", "nope"), { name: "SyntaxError" });
  assert.throws(() => performance.measure("both", { start: 1 }, "end"), TypeError);
  assert.throws(() => performance.measure("neither", { duration: 1 }), TypeError);
  assert.throws(() => performance.measure("all", { start: 1, end: 2, duration: 1 }), TypeError);

  assert.deepEqual(performance.getEntries().map(entry => entry.name),
    ["whole", "origin", "start", "untimed", "between", "since start", "timed", "ending", "end"]);
  assert.deepEqual(performance.getEntriesByType("mark").map(entry => entry.name), ["start", "untimed", "end"]);
  assert.deepEqual(performance.getEntriesByType("navigation"), []);
  assert.deepEqual(performance.getEntriesByName("start", "measure"), []);
  assert.deepEqual(JSON.parse(JSON.stringify(between)), { name: "between", entryType: "measure", startTime: 1000, duration: 250, detail: null });
  performance.clearMarks("start");
  assert.deepEqual(performance.getEntriesByType("mark").map(entry => entry.name), ["untimed", "end"]);
  performance.clearMeasures();
  assert.deepEqual(performance.getEntriesByType("measure"), []);
  performance.clearMarks();
  assert.deepEqual(performance.getEntries(), []);
  assert.equal("PerformanceObserver" in global, false);
});

test("an idle callback timeout runs it as a timer task with didTimeout", () => {
  const { clock, global, loop } = timing();
  const seen = [];
  global.requestIdleCallback(deadline => seen.push([deadline.didTimeout, deadline.timeRemaining()]), { timeout: 25 });
  assert.equal(clock.armed, 25);
  assert.equal(global.setTimeout(() => {}, 1000), 1, "internal timeout timers take no timeout IDs");
  clock.now = 25;
  loop.fireTimer();
  assert.deepEqual(seen, [[true, 0]]);
  assert.equal(loop.idleWakeup(), null);
  global.requestIdleCallback(() => seen.push("idle"), { timeout: 10 });
  loop.beginIdle(40);
  loop.nextIdle()();
  clock.now = 35;
  loop.fireTimer();
  assert.deepEqual(seen, [[true, 0], "idle"], "running in an idle period cancels the timeout");
});

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

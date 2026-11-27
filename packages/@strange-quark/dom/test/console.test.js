// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { formatConsole, installConsole } from "@strange-quark/dom";

test("formatConsole substitutes directives like browsers", () => {
  assert.equal(formatConsole(["%s has %d items (%i%%)", "list", 3.7, 42]), "list has 3 items (42%)");
  assert.equal(formatConsole(["%c styled", "color: red"]), " styled");
  assert.equal(formatConsole(["value:", { a: 1, b: [1, "x"] }]), 'value: { a: 1, b: [ 1, "x" ] }');
  assert.equal(formatConsole(["missing %s"]), "missing %s");
  assert.equal(formatConsole([]), "");
});

test("formatConsole survives cycles, getters that throw and errors", () => {
  const cyclic = { name: "loop" };
  cyclic.self = cyclic;
  assert.equal(formatConsole([cyclic]), '{ name: "loop", self: [Circular] }');
  const hostile = {};
  Object.defineProperty(hostile, "bad", { enumerable: true, get() { throw new Error("no"); } });
  assert.equal(formatConsole([hostile]), '{ bad: "[Getter threw]" }');
  const error = new TypeError("boom");
  assert.match(formatConsole([error]), /^TypeError: boom/);
});

test("installConsole writes only when the host supplies consoleWrite", () => {
  const silent = { console: { log() {} } };
  const before = silent.console.log;
  installConsole(silent, {});
  assert.equal(silent.console.log, before);

  const lines = [];
  const calls = [];
  const global = { console: { log: (...args) => calls.push(args) } };
  installConsole(global, { consoleWrite: (level, text) => lines.push([level, text]) });
  global.console.log("a", 1);
  global.console.error(new Error("bad"));
  global.console.group("g");
  global.console.warn("inside");
  global.console.groupEnd();
  global.console.count();
  global.console.count();
  global.console.assert(false, "nope");
  assert.deepEqual(calls, [["a", 1]]);
  assert.deepEqual(lines[0], ["log", "a 1"]);
  assert.equal(lines[1][0], "error");
  assert.match(lines[1][1], /^Error: bad/);
  assert.deepEqual(lines.slice(2), [
    ["log", "g"],
    ["warn", "  inside"],
    ["log", "default: 1"],
    ["log", "default: 2"],
    ["error", "Assertion failed: nope"],
  ]);
});

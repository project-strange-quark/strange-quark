// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { bootstrapDocument, installInput } from "@strange-quark/dom";
import { Journal } from "@strange-quark/dom/journal";
import { nodeId } from "../dist/document.js";

test("explicit click counts bypass timing and preserve trusted click details", () => {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const root = document.appendChild(document.createElement("html"));
  const body = root.appendChild(document.createElement("body"));
  const button = body.appendChild(document.createElement("button"));
  const target = nodeId(button);
  const dispatch = installInput(document, { inputResult() {} });
  const events = [];
  for (const type of ["mousedown", "mouseup", "click", "dblclick"]) {
    button.addEventListener(type, event => events.push([type, event.detail, event.isTrusted]));
  }

  function click(time, x, downCount, upCount = downCount) {
    for (const [type, buttons, clickCount] of [
      ["pointerdown", 1, downCount], ["pointerup", 0, upCount],
    ]) {
      dispatch({ id: 1, type, target, related: 0, time: time + 1 - buttons, cancelable: true, fields: {
        pointerType: "mouse", pointerId: 1, button: 0, buttons, isPrimary: true, boundaryUpdate: false,
        x, y: 20, pressure: buttons * 0.5, samples: [], clickCount,
        ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
      } });
    }
  }

  click(10, 10, 1);
  click(842, 16, 2);
  click(900, 16, 1);
  click(1800, 16);
  click(1850, 16, null);
  click(2600, 16, 2, null);
  click(3400, 16, undefined, 2);
  click(3450, 16, 0);

  assert.deepEqual(events, [
    ["mousedown", 1, true], ["mouseup", 1, true], ["click", 1, true],
    ["mousedown", 2, true], ["mouseup", 2, true], ["click", 2, true], ["dblclick", 2, true],
    ["mousedown", 1, true], ["mouseup", 1, true], ["click", 1, true],
    ["mousedown", 1, true], ["mouseup", 1, true], ["click", 1, true],
    ["mousedown", 2, true], ["mouseup", 2, true], ["click", 2, true], ["dblclick", 2, true],
    ["mousedown", 2, true], ["mouseup", 2, true], ["click", 2, true], ["dblclick", 2, true],
    ["mousedown", 1, true], ["mouseup", 2, true], ["click", 2, true], ["dblclick", 2, true],
    ["mousedown", 0, true], ["mouseup", 0, true], ["click", 0, true],
  ]);
});

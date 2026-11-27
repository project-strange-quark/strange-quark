// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { bootstrapDocument, installDocumentBindings, installDOM, Range } from "@strange-quark/dom";
import { Journal, RECORD_BYTES } from "@strange-quark/dom/journal";
import { nodeId } from "../dist/document.js";

// A stub engine: every element has one fixed box, Text lays out at 10px per
// UTF-16 code unit on the row its node id names, and hit tests come from a table.
function setup({ elements = new Map(), caret = () => null, controlCaret } = {}) {
  const global = {};
  const document = bootstrapDocument(new Journal((buffer) => new DataView(buffer).getUint32(0, true) / RECORD_BYTES), 1, 0, []);
  installDOM(global, document);
  const html = document.appendChild(document.createElement("html"));
  html.append(document.createElement("head"), document.createElement("body"));
  const queries = [];
  installDocumentBindings(global, document, {
    parseFragment() { return []; },
    query(kind, id, arg) {
      queries.push([kind, id, arg]);
      if (kind === 2) return elements.has(id) ? [elements.get(id)] : [];
      if (kind === 14) {
        const [start, end] = arg;
        return id === hidden ? [] : [{ x: 10 * start, y: 20 * id, width: 10 * (end - start), height: 20 }];
      }
      if (kind === 15) return caret(arg[0], arg[1]);
      if (kind === 11) return controlCaret(id, arg);
      throw new Error(`unexpected query ${kind}`);
    },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber() { return 0; },
    postTask(callback) { queueMicrotask(callback); },
  });
  let hidden = -1;
  return { global, document, queries, hide(node) { hidden = nodeId(node); } };
}

const plain = (rect) => [rect.x, rect.y, rect.width, rect.height];

test("range client rects cover selected text substrings and top-most contained elements", () => {
  const elements = new Map();
  const { document, queries } = setup({ elements });
  const text = document.createTextNode("Hello world");
  const bold = document.createElement("b");
  bold.append("bold", document.createElement("i"));
  const tail = document.createTextNode("tail");
  const div = document.body.appendChild(document.createElement("div"));
  div.append(text, bold, tail);
  elements.set(nodeId(bold), { x: 1, y: 2, width: 30, height: 4 });
  elements.set(nodeId(bold.lastChild), { x: 5, y: 6, width: 7, height: 8 });
  elements.set(nodeId(div), { x: 0, y: 0, width: 999, height: 99 });

  const range = document.createRange();
  range.setStart(text, 6);
  range.setEnd(tail, 2);
  const rects = range.getClientRects();
  assert.deepEqual([...rects].map(plain), [
    [60, 20 * nodeId(text), 50, 20],
    [1, 2, 30, 4],
    [0, 20 * nodeId(bold.firstChild), 40, 20],
    [0, 20 * nodeId(tail), 20, 20],
  ], "the i inside the selected b is not listed again, and the div holding the end is not listed");
  assert.equal(rects.length, 4);
  assert.deepEqual(plain(rects.item(1)), [1, 2, 30, 4]);
  assert.equal(rects.item(9), null);
  assert(queries.some(([kind, id, arg]) => kind === 14 && id === nodeId(text) && arg[0] === 6 && arg[1] === 11));

  const bounds = range.getBoundingClientRect();
  assert.equal(bounds.constructor, rects[0].constructor);
  const top = Math.min(2, 20 * nodeId(text));
  const bottom = 20 * nodeId(tail) + 20;
  assert.deepEqual(plain(bounds), [0, top, 110, bottom - top]);
  assert.deepEqual([bounds.left, bounds.right], [0, 110]);
});

test("collapsed, empty, hidden and detached ranges report Chromium's rects", () => {
  const { document, hide } = setup();
  const text = document.body.appendChild(document.createTextNode("caret"));
  const range = document.createRange();
  range.setStart(text, 3);
  assert.deepEqual([...range.getClientRects()].map(plain), [[30, 20 * nodeId(text), 0, 20]]);
  assert.deepEqual(plain(range.getBoundingClientRect()), [30, 20 * nodeId(text), 0, 20], "an all-empty list returns its first rect");

  range.setStart(document.body, 0);
  range.collapse(true);
  assert.equal(range.getClientRects().length, 0);
  assert.deepEqual(plain(range.getBoundingClientRect()), [0, 0, 0, 0]);

  hide(text);
  range.selectNodeContents(text);
  assert.equal(range.getClientRects().length, 0, "unrendered text has no rects");

  const detached = document.createElement("p");
  detached.append("away");
  range.selectNodeContents(detached);
  assert.equal(range.getClientRects().length, 0);
  assert.deepEqual(plain(range.getBoundingClientRect()), [0, 0, 0, 0]);
});

test("caretPositionFromPoint and caretRangeFromPoint resolve the engine's hit", () => {
  let hit = null;
  const { global, document, queries } = setup({
    caret: () => hit,
    controlCaret: (id, offset) => ({ x: 100 + offset, y: 5, width: 0, height: 18 }),
  });
  assert.equal(typeof global.CaretPosition, "function");
  assert.throws(() => new global.CaretPosition(), TypeError);
  const text = document.body.appendChild(document.createTextNode("Hello"));
  const input = document.body.appendChild(document.createElement("input"));
  input.value = "field";

  hit = [nodeId(text), 2];
  const position = document.caretPositionFromPoint(12.5, 30);
  assert(position instanceof global.CaretPosition);
  assert.equal(position.offsetNode, text);
  assert.equal(position.offset, 2);
  assert.deepEqual(plain(position.getClientRect()), [20, 20 * nodeId(text), 0, 20]);
  assert.deepEqual(queries.find(([kind]) => kind === 15), [15, nodeId(document), [12.5, 30]]);
  const range = document.caretRangeFromPoint(12.9, 30.2);
  assert(range instanceof Range);
  assert.deepEqual([range.startContainer, range.startOffset, range.collapsed], [text, 2, true]);
  assert.deepEqual(queries.filter(([kind]) => kind === 15).at(-1)[2], [12, 30], "caretRangeFromPoint takes long coordinates");

  hit = [nodeId(input), 3];
  const inControl = document.caretPositionFromPoint(1, 1);
  assert.deepEqual([inControl.offsetNode, inControl.offset], [input, 3]);
  assert.deepEqual(plain(inControl.getClientRect()), [103, 5, 0, 18]);
  const controlRange = document.caretRangeFromPoint(1, 1);
  assert.deepEqual([controlRange.startContainer, controlRange.startOffset], [document.body, 1]);

  hit = [nodeId(document.body), 9];
  assert.equal(document.caretPositionFromPoint(1, 1).offset, 2, "an element offset is clamped to its child count");

  hit = null;
  assert.equal(document.caretPositionFromPoint(1, 1), null);
  assert.equal(document.caretRangeFromPoint(1, 1), null);
  hit = [987654, 0];
  assert.equal(document.caretPositionFromPoint(1, 1), null, "an unknown node is no caret");
  assert.throws(() => document.caretPositionFromPoint(Number.NaN, 0), TypeError);
  assert.throws(() => document.caretPositionFromPoint(1), TypeError);
  hit = [nodeId(text), 0];
  assert.equal(document.caretRangeFromPoint().startOffset, 0);
  assert.deepEqual(queries.filter(([kind]) => kind === 15).at(-1)[2], [0, 0]);
});

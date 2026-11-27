// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument, DragController, DragEvent, dragSource, installDOM } from "@strange-quark/dom";
import { DataTransfer } from "../dist/clipboard.js";
import { Journal, RECORD_BYTES } from "@strange-quark/dom/journal";

function setup() {
  const global = {};
  const journal = new Journal(buffer => new DataView(buffer).getUint32(0, true) / RECORD_BYTES);
  const doc = createDocument(journal);
  installDOM(global, doc, { url: "https://app.test/index.html" });
  const root = doc.appendChild(doc.createElement("html"));
  root.append(doc.createElement("head"), doc.createElement("body"));
  const images = [];
  const tasks = [];
  const controller = new DragController(doc, {
    postTask: callback => tasks.push(callback),
    dragImage: (element, x, y) => images.push(element === null ? null : [element.id, x, y]),
  });
  return { doc, controller, images, tasks };
}

// Two tabs, as an editor's tab bar reorders them.
function tabs(doc, log, { accept = true } = {}) {
  const bar = doc.body.appendChild(doc.createElement("div"));
  const [a, b] = ["a", "b"].map(id => {
    const tab = bar.appendChild(doc.createElement("div"));
    tab.id = id;
    tab.setAttribute("draggable", "true");
    return tab;
  });
  for (const type of ["dragstart", "drag", "dragenter", "dragleave", "dragover", "drop", "dragend"]) {
    bar.addEventListener(type, event => {
      assert.ok(event instanceof DragEvent);
      const transfer = event.dataTransfer;
      log.push([type, event.target.id, transfer.getData("text/plain"), transfer.types.join(","), transfer.dropEffect]);
      if (type === "dragstart") {
        transfer.setData("text/plain", event.target.id);
        transfer.effectAllowed = "move";
      }
      if (accept && (type === "dragover" || type === "drop")) event.preventDefault();
    });
  }
  return { a, b };
}

const at = (x, y) => ({ clientX: x, clientY: y });

test("a draggable tab drags onto another and drops with the negotiated effect", () => {
  const { doc, controller, images } = setup();
  const log = [];
  const { a, b } = tabs(doc, log);
  let cancelled = 0;
  controller.arm(a, 10, 10, 1);
  assert.equal(controller.move(a, at(12, 11), 1, () => cancelled++), false, "under the threshold the move is the page's");
  assert.equal(controller.move(a, at(15, 10), 1, () => cancelled++), true);
  assert.equal(cancelled, 1, "the pointer is cancelled once the drag starts");
  assert.ok(controller.dragging);
  assert.deepEqual(images, [["a", 0, 0]]);
  controller.move(b, at(60, 10), 1, () => {});
  assert.equal(controller.release(at(60, 10)), true);
  assert.deepEqual(log, [
    ["dragstart", "a", "", "", "none"],
    ["drag", "a", "", "text/plain", "none"],
    ["dragenter", "a", "", "text/plain", "move"],
    ["dragover", "a", "", "text/plain", "move"],
    ["drag", "a", "", "text/plain", "move"],
    ["dragenter", "b", "", "text/plain", "move"],
    ["dragleave", "a", "", "text/plain", "move"],
    ["dragover", "b", "", "text/plain", "move"],
    ["drop", "b", "a", "text/plain", "move"],
    ["dragend", "a", "", "text/plain", "move"],
  ]);
  assert.ok(!controller.dragging);
  assert.deepEqual(images.at(-1), null);
});

test("an unaccepted target drops nothing; Escape and a cancelled dragstart end cleanly", () => {
  const { doc, controller } = setup();
  const log = [];
  const { a, b } = tabs(doc, log, { accept: false });
  controller.arm(a, 0, 0, 1);
  controller.move(b, at(10, 0), 1, () => {});
  controller.release(at(10, 0));
  assert.deepEqual(log.slice(-2).map(entry => entry.slice(0, 2)), [["dragleave", "b"], ["dragend", "a"]]);
  assert.equal(log.at(-1)[4], "none");

  log.length = 0;
  controller.arm(a, 0, 0, 1);
  controller.move(b, at(10, 0), 1, () => {});
  assert.equal(controller.cancel(), true);
  assert.deepEqual(log.slice(-2).map(entry => entry.slice(0, 2)), [["dragleave", "b"], ["dragend", "a"]]);

  log.length = 0;
  a.addEventListener("dragstart", event => event.preventDefault());
  controller.arm(a, 0, 0, 1);
  assert.equal(controller.move(b, at(10, 0), 1, () => assert.fail("not cancelled")), false);
  assert.ok(!controller.dragging);
  assert.deepEqual(log.map(entry => entry[0]), ["dragstart"]);
});

test("drag sources follow draggable, links, images and form controls", () => {
  const { doc } = setup();
  const link = doc.body.appendChild(doc.createElement("a"));
  link.setAttribute("href", "/docs");
  const inner = link.appendChild(doc.createElement("span"));
  const image = doc.body.appendChild(doc.createElement("img"));
  const off = doc.body.appendChild(doc.createElement("img"));
  off.setAttribute("draggable", "false");
  const plain = doc.body.appendChild(doc.createElement("div"));
  const input = doc.body.appendChild(doc.createElement("input"));
  input.setAttribute("draggable", "true");
  assert.equal(dragSource(inner), link);
  assert.equal(dragSource(image), image);
  assert.equal(dragSource(off), null);
  assert.equal(dragSource(plain), null);
  assert.equal(dragSource(input), input);
});

test("DataTransfer outside a drag keeps its clipboard behaviour", () => {
  const transfer = new DataTransfer();
  transfer.effectAllowed = "move";
  assert.equal(transfer.effectAllowed, "none", "only dragstart may set effectAllowed");
  transfer.dropEffect = "copy";
  assert.equal(transfer.dropEffect, "copy");
  transfer.dropEffect = "bogus";
  assert.equal(transfer.dropEffect, "copy");
  transfer.setData("text", "hi");
  assert.equal(transfer.getData("text/plain"), "hi");
});

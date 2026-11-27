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

test("pointer input drives a drag: pointercancel, no mouseup or click, and Escape cancels", async () => {
  const { bootstrapDocument, installInput } = await import("@strange-quark/dom");
  const { Journal } = await import("@strange-quark/dom/journal");
  const { nodeId } = await import("../dist/document.js");
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const root = document.appendChild(document.createElement("html"));
  const body = root.appendChild(document.createElement("body"));
  const a = body.appendChild(document.createElement("div"));
  const b = body.appendChild(document.createElement("div"));
  a.setAttribute("draggable", "true");
  const tasks = [];
  const dispatch = installInput(document, { inputResult() {}, postTask: callback => tasks.push(callback) });
  const seen = [];
  for (const type of ["pointerdown", "pointercancel", "mousedown", "mousemove", "mouseup", "click", "dragstart", "dragenter", "dragleave", "dragover", "drop", "dragend"]) {
    body.addEventListener(type, event => seen.push(`${type}:${event.target === a ? "a" : event.target === b ? "b" : "?"}`));
  }
  b.addEventListener("dragover", event => event.preventDefault());
  b.addEventListener("drop", event => event.preventDefault());
  let time = 0;
  const pointer = (type, target, x, buttons) => dispatch({
    id: 1, type, target: nodeId(target), related: 0, time: time += 5, cancelable: true, fields: {
      pointerType: "mouse", pointerId: 1, button: type === "pointermove" ? -1 : 0, buttons, isPrimary: true,
      boundaryUpdate: false, x, y: 10, pressure: buttons ? 0.5 : 0, samples: [], clickCount: null,
      ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
    },
  });
  pointer("pointerdown", a, 10, 1);
  pointer("pointermove", a, 12, 1);
  pointer("pointermove", b, 40, 1);
  pointer("pointermove", b, 41, 1);
  pointer("pointerup", b, 41, 0);
  assert.deepEqual(seen, [
    "pointerdown:a", "mousedown:a", "mousemove:a",
    "dragstart:a", "pointercancel:a", "dragenter:b", "dragover:b",
    "dragover:b", "drop:b", "dragend:a",
  ]);

  seen.length = 0;
  pointer("pointerdown", a, 10, 1);
  pointer("pointermove", b, 40, 1);
  dispatch({ id: 2, type: "keydown", target: 0, related: 0, time: time += 5, cancelable: true, fields: {
    key: "Escape", code: "Escape", location: 0, repeat: false, isComposing: false,
    ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
  } });
  pointer("pointerup", b, 40, 0);
  assert.deepEqual(seen.slice(-2), ["dragleave:b", "dragend:a"]);
  assert.ok(!seen.includes("mouseup:b") && !seen.includes("click:b"));
});

test("DragEvent is a global and drag handlers reflect as properties", async () => {
  const { installDOM: install, installPlatform, createDocument: create } = await import("@strange-quark/dom");
  const { Journal: J, RECORD_BYTES: R } = await import("@strange-quark/dom/journal");
  const global = { Blob, File, reportError() {} };
  const doc = create(new J(buffer => new DataView(buffer).getUint32(0, true) / R));
  install(global, doc, { url: "https://app.test/" });
  installPlatform(global, doc, { utf8Encode: t => new TextEncoder().encode(t), utf8Decode: b => new TextDecoder().decode(b), postTask() {} });
  assert.equal(global.DragEvent, DragEvent);
  const div = doc.createElement("div");
  let fired = 0;
  div.ondragover = () => fired++;
  div.dispatchEvent(new DragEvent("dragover"));
  assert.equal(fired, 1);
});

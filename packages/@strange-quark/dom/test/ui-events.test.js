// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createContext, runInContext } from "node:vm";
import {
  bootstrapDocument, CompositionEvent, Event, FocusEvent, InputEvent, installDocumentBindings, installDOM, installInput,
  KeyboardEvent, MouseEvent, PointerEvent, UIEvent, WheelEvent,
} from "@strange-quark/dom";
import { Journal } from "@strange-quark/dom/journal";
import { nodeId } from "../dist/document.js";

test("UI events inherit from UIEvent and carry a Window view and a detail", () => {
  const global = {};
  installDOM(global);
  for (const Type of [MouseEvent, KeyboardEvent, FocusEvent, InputEvent, CompositionEvent, WheelEvent, PointerEvent]) {
    const event = new Type("x", { view: global, detail: 3 });
    assert(event instanceof UIEvent, Type.name);
    assert(event instanceof Event, Type.name);
    assert.equal(event.view, global, Type.name);
    assert.equal(event.detail, 3, Type.name);
    assert.throws(() => new Type(), TypeError, Type.name);
    assert.doesNotThrow(() => new Type("x", null), Type.name);
  }
  assert.equal(global.UIEvent, UIEvent);
  const event = new UIEvent("custom", { view: null, detail: 2 ** 32 + 5, bubbles: true });
  assert.deepEqual([event.view, event.detail, event.which, event.bubbles], [null, 5, 0, true]);
  assert.throws(() => new UIEvent("x", { view: {} }), TypeError);
  event.initUIEvent("renamed", false, true, global, 7);
  assert.deepEqual([event.type, event.cancelable, event.view, event.detail], ["renamed", true, global, 7]);
  assert(global.document.createEvent("UIEvents") instanceof UIEvent);

  class Gesture extends UIEvent {
    constructor(type, init) { super(type, init); this.scale = init.scale; }
  }
  const gesture = new Gesture("gesture", { scale: 2, detail: 1 });
  assert.deepEqual([gesture.scale, gesture.detail, gesture instanceof UIEvent], [2, 1, true]);
});

test("mouse event coordinates are read-only accessors and getModifierState reports init modifiers", () => {
  const event = new MouseEvent("mousedown", {
    clientX: 12.5, clientY: 7, screenX: 100, screenY: 200, movementX: -3, movementY: 4, button: 2, buttons: 2,
    ctrlKey: true, modifierCapsLock: true, modifierAltGraph: true,
  });
  assert.deepEqual([event.x, event.y, event.screenX, event.screenY, event.movementX, event.movementY],
    [12.5, 7, 100, 200, -3, 4]);
  assert.deepEqual([event.pageX, event.pageY, event.offsetX, event.offsetY, event.layerX, event.layerY],
    [12.5, 7, 12.5, 7, 12, 7], "an undispatched event measures from the page");
  assert.equal(Object.hasOwn(event, "clientX"), false);
  assert.throws(() => { "use strict"; event.clientX = 1; }, TypeError);
  assert.equal(event.which, 3);
  for (const [key, active] of [["Control", true], ["CapsLock", true], ["AltGraph", true], ["NumLock", false], ["Shift", false]]) {
    assert.equal(event.getModifierState(key), active, key);
  }
  const keyboard = new KeyboardEvent("keydown", { modifierNumLock: true, location: KeyboardEvent.DOM_KEY_LOCATION_NUMPAD });
  assert.equal(keyboard.getModifierState("NumLock"), true);
  assert.equal(keyboard.location, keyboard.DOM_KEY_LOCATION_NUMPAD);
  event.initMouseEvent("mouseup", true, true, null, 1, 5, 6, 7, 8, false, false, true, false, 0, null);
  assert.deepEqual([event.type, event.clientX, event.screenY, event.shiftKey, event.ctrlKey, event.button, event.detail],
    ["mouseup", 7, 6, true, false, 0, 1]);
});

test("pageX adds the view scroll and offsetX and layerX measure from the target's boxes", () => {
  const context = createContext({});
  const global = runInContext("globalThis", context);
  const document = installDOM(global);
  const outer = document.body.appendChild(document.createElement("div"));
  const target = outer.appendChild(document.createElement("span"));
  const rects = new Map([[nodeId(outer), { x: 40, y: 30, width: 200, height: 100 }], [nodeId(target), { x: 50, y: 35, width: 20, height: 10 }]]);
  installDocumentBindings(global, document, {
    parseFragment() { return []; },
    query(kind, id, argument) {
      if (kind === 1) return rects.get(id);
      if (kind === 4) return argument.property === "position" && id === nodeId(outer) ? "relative" :
        { position: "static", transform: "none", filter: "none", "overflow-x": "visible", opacity: "1" }[argument.property];
      return null;
    },
    scrollOffset() { return { left: 0, top: 0 }; },
    queryNumber(_kind, id, field) { return id === nodeId(target) && field === 6 ? 2 : id === nodeId(target) && field === 5 ? 1 : 0; },
    postTask(callback) { callback(); },
  });
  Object.defineProperties(global, { scrollX: { value: 15, configurable: true }, scrollY: { value: 25, configurable: true } });
  const seen = [];
  target.addEventListener("mousemove", event => {
    seen.push([event.pageX, event.pageY, event.offsetX, event.offsetY, event.layerX, event.layerY]);
  });
  target.dispatchEvent(new MouseEvent("mousemove", { clientX: 61, clientY: 40, view: global }));
  assert.deepEqual(seen, [[76, 65, 9, 4, 21, 10]]);
});

test("trusted pointer moves report movement since the pointer's last position and screen coordinates", () => {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const body = document.appendChild(document.createElement("html")).appendChild(document.createElement("body"));
  const dispatch = installInput(document, { inputResult() {} });
  const seen = [];
  for (const type of ["pointermove", "mousemove", "pointerdown"]) {
    body.addEventListener(type, event => seen.push([type, event.movementX, event.movementY, event.screenX, event.screenY,
      event.getCoalescedEvents?.().map(sample => [sample.movementX, sample.screenX]) ?? []]));
  }
  const send = (type, x, y, samples = []) => dispatch({ id: 1, type, target: nodeId(body), related: 0, time: 1, cancelable: true, fields: {
    pointerType: "mouse", pointerId: 1, button: type === "pointermove" ? -1 : 0, buttons: type === "pointerdown" ? 1 : 0,
    isPrimary: true, boundaryUpdate: false, x, y, pressure: 0, samples, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
  } });
  send("pointermove", 10, 20);
  send("pointermove", 13, 18, [{ x: 12, y: 19, pressure: 0, time: 1 }, { x: 13, y: 18, pressure: 0, time: 1 }]);
  send("pointerdown", 20, 20);
  assert.deepEqual(seen, [
    ["pointermove", 0, 0, 10, 20, []], ["mousemove", 0, 0, 10, 20, []],
    ["pointermove", 3, -2, 13, 18, [[2, 12], [1, 13]]], ["mousemove", 3, -2, 13, 18, []],
    ["pointerdown", 0, 0, 20, 20, []],
  ]);
});

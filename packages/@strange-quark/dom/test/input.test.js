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

function pointerScene(os) {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  const root = document.appendChild(document.createElement("html"));
  const body = root.appendChild(document.createElement("body"));
  const list = body.appendChild(document.createElement("ul"));
  const first = list.appendChild(document.createElement("li"));
  const second = list.appendChild(document.createElement("li"));
  const dispatch = installInput(document, { os, inputResult() {} });
  const events = [];
  for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup", "click", "auxclick", "contextmenu"]) {
    body.addEventListener(type, event => events.push([type, event.target.localName, event.button, event.buttons, event.detail,
      event.constructor.name]));
  }
  let time = 0;
  const send = (type, target, button, buttons, clickCount = undefined) => dispatch({
    id: 1, type, target: nodeId(target), related: 0, time: time += 10, cancelable: true, fields: {
      pointerType: "mouse", pointerId: 1, button, buttons, isPrimary: true, boundaryUpdate: false, x: 5, y: 5,
      pressure: buttons === 0 ? 0 : 0.5, samples: [], clickCount, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
    },
  });
  return { body, first, second, events, send };
}

test("a right click fires contextmenu on press except on Windows, and auxclick on release", () => {
  const linux = pointerScene("linux");
  linux.send("pointerdown", linux.first, 2, 2);
  linux.send("pointerup", linux.first, 2, 0);
  assert.deepEqual(linux.events, [
    ["pointerdown", "li", 2, 2, 0, "PointerEvent"], ["mousedown", "li", 2, 2, 1, "MouseEvent"],
    ["contextmenu", "li", 2, 2, 0, "PointerEvent"],
    ["pointerup", "li", 2, 0, 0, "PointerEvent"], ["mouseup", "li", 2, 0, 1, "MouseEvent"],
    ["auxclick", "li", 2, 0, 1, "PointerEvent"],
  ]);

  const windows = pointerScene("windows");
  windows.body.addEventListener("pointerdown", event => event.preventDefault(), { once: true });
  windows.send("pointerdown", windows.first, 2, 2);
  windows.send("pointerup", windows.first, 2, 0);
  assert.deepEqual(windows.events.map(([type]) => type), ["pointerdown", "pointerup", "auxclick", "contextmenu"],
    "a canceled pointerdown suppresses the mouse events but not auxclick or contextmenu");
});

test("auxclick fires for other non-primary buttons at the common ancestor and counts repeats", () => {
  const { first, second, events, send } = pointerScene("linux");
  send("pointerdown", first, 1, 4);
  send("pointerup", second, 1, 0);
  send("pointerdown", second, 1, 4);
  send("pointerup", second, 1, 0);
  send("pointerdown", second, 1, 4);
  send("pointerup", second, 1, 0);
  assert.deepEqual(events.filter(([type]) => type === "auxclick" || type === "click"), [
    ["auxclick", "ul", 1, 0, 1, "PointerEvent"], ["auxclick", "li", 1, 0, 1, "PointerEvent"], ["auxclick", "li", 1, 0, 2, "PointerEvent"],
  ]);
  assert.equal(events.some(([type]) => type === "contextmenu"), false);
});

test("keyCode follows Chromium's virtual-key codes for each key value and position", () => {
  const document = bootstrapDocument(new Journal(), 1, 0, []);
  document.appendChild(document.createElement("html")).appendChild(document.createElement("body"));
  const dispatch = installInput(document, { inputResult() {} });
  const seen = [];
  document.addEventListener("keydown", event => seen.push([event.keyCode, event.which, event.getModifierState("CapsLock")]));
  const cases = [
    ["F1", "F1", 112], ["F12", "F12", 123], ["F24", "F24", 135], ["Insert", "Insert", 45], ["CapsLock", "CapsLock", 20],
    ["NumLock", "NumLock", 144], ["ScrollLock", "ScrollLock", 145], ["Pause", "Pause", 19], ["PrintScreen", "PrintScreen", 44],
    ["ContextMenu", "ContextMenu", 93], ["AudioVolumeUp", "AudioVolumeUp", 175], ["MediaPlayPause", "MediaPlayPause", 179],
    [";", "Semicolon", 186], ["=", "Equal", 187], [",", "Comma", 188], ["-", "Minus", 189], [".", "Period", 190],
    ["/", "Slash", 191], ["`", "Backquote", 192], ["[", "BracketLeft", 219], ["\\", "Backslash", 220], ["]", "BracketRight", 221],
    ["'", "Quote", 222], ["<", "IntlBackslash", 226], ["!", "Digit1", 49], ["a", "KeyQ", 65], ["Z", "KeyZ", 90], ["é", "Digit2", 50],
    ["Dead", "Quote", 222], ["1", "Numpad1", 97], ["End", "Numpad1", 35], ["+", "NumpadAdd", 107], ["Enter", "NumpadEnter", 13],
    ["*", "NumpadMultiply", 106], [" ", "Space", 32], ["Unidentified", "Unidentified", 0],
  ];
  for (const [key, code] of cases) {
    dispatch({ id: 1, type: "keydown", target: 0, related: 0, time: 1, cancelable: true, fields: {
      key, code, location: 0, repeat: false, isComposing: false, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false,
    } });
  }
  assert.deepEqual(seen, cases.map(([, , keyCode]) => [keyCode, keyCode, false]));
});

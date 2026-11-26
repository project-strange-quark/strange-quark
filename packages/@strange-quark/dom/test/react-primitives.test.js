// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  AbortController, AbortSignal, createDocument, HTMLElement, HTMLIFrameElement, HTMLInputElement,
  HTMLSelectElement, HTMLTextAreaElement, installDOM, MutationObserver,
} from "@strange-quark/dom";
import { Journal } from "@strange-quark/dom/journal";

test("CSSStyleDeclaration emits a shared important flag and reflects style attribute writes", () => {
  const journal = new Journal();
  const element = createDocument(journal).createElement("div");
  const style = element.style;
  style.setProperty("color", "red", "important");
  const expected = readFileSync(new URL("./fixtures/style-important.bin", import.meta.url));
  assert.deepEqual(Buffer.from(journal.snapshot()), expected);
  assert.equal(style.getPropertyPriority("color"), "important");
  assert.equal(style.getPropertyValue("color"), "red");
  assert.equal(element.getAttribute("style"), "color: red !important;");
  assert.equal(style.removeProperty("color"), "red");
  assert.equal(style.length, 0);
  element.setAttribute("style", 'background-image: url("a;b"); --a: 4 !important');
  assert.equal(style.backgroundImage, 'url("a;b")');
  assert.equal(style.getPropertyPriority("--a"), "important");
  style.display = "block";
  assert.equal(element.style, style);
  assert.equal(style.display, "block");
  assert.equal(style.item(2), "display");
});

test("form prototype accessors keep live and default state separate", () => {
  const document = installDOM({});
  const input = document.createElement("input");
  assert(input instanceof HTMLElement && input instanceof HTMLInputElement);
  const value = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
  const checked = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "checked");
  assert.equal(typeof value.get, "function");
  assert.equal(typeof checked.set, "function");
  input.defaultValue = "initial";
  assert.equal(input.value, "initial");
  value.set.call(input, "draft");
  input.defaultValue = "next";
  assert.equal(input.value, "draft");
  assert.equal(input.getAttribute("value"), "next");
  checked.set.call(input, true);
  input.defaultChecked = false;
  assert.equal(input.checked, true);
  assert.equal(input.hasAttribute("checked"), false);
  assert.equal(input.cloneNode().value, "draft");
  assert.equal(input.cloneNode().checked, true);

  const textarea = document.createElement("textarea");
  assert(textarea instanceof HTMLTextAreaElement);
  textarea.defaultValue = "one\r\ntwo";
  assert.equal(textarea.value, "one\ntwo");
  textarea.value = "active";
  textarea.defaultValue = "different";
  assert.equal(textarea.value, "active");
  assert.equal(textarea.textContent, "different");

  const select = document.createElement("select");
  assert(select instanceof HTMLSelectElement);
  const first = document.createElement("option");
  const second = document.createElement("option");
  first.value = "first";
  second.value = "second";
  second.disabled = true;
  select.append(first, second);
  assert.equal(select.value, "first");
  select.value = "second";
  assert.equal(second.selected, true);
  assert.equal(first.selected, false);
  select.selectedIndex = -1;
  assert.equal(select.value, "");
});

test("focus changes activeElement and dispatches focus events in tree order", () => {
  const document = installDOM({});
  const input = document.body.appendChild(document.createElement("input"));
  const button = document.body.appendChild(document.createElement("button"));
  const events = [];
  for (const type of ["focusin", "focusout"]) {
    document.body.addEventListener(type, event => events.push([type, event.target, event.relatedTarget]));
  }
  assert.equal(document.activeElement, document.body);
  input.focus({ preventScroll: true });
  assert.equal(document.activeElement, input);
  button.focus();
  assert.equal(document.activeElement, button);
  button.blur();
  assert.equal(document.activeElement, document.body);
  assert.deepEqual(events, [
    ["focusin", input, null], ["focusout", input, button], ["focusin", button, input], ["focusout", button, null],
  ]);
  const iframe = document.createElement("iframe");
  assert(iframe instanceof HTMLIFrameElement);
  iframe.src = "/sub";
  assert.equal(iframe.getAttribute("src"), "/sub");
  assert.equal(iframe.contentWindow, null);
  assert.equal(iframe.contentDocument, null);
});

test("MutationObserver delivers grouped children and preserves detached subtree old values", async () => {
  const document = installDOM({});
  const root = document.createElement("div");
  const fragment = document.createDocumentFragment();
  const first = document.createElement("p");
  const second = document.createElement("p");
  fragment.append(first, second);
  const deliveries = [];
  const observer = new MutationObserver(records => deliveries.push(records));
  observer.observe(root, { childList: true, subtree: true, attributeFilter: ["data-a"], attributeOldValue: true });
  root.append(fragment);
  first.setAttribute("data-a", "one");
  first.setAttribute("other", "ignored");
  root.removeChild(first);
  first.setAttribute("data-a", "two");
  assert.equal(deliveries.length, 0);
  await Promise.resolve();
  assert.equal(deliveries.length, 1);
  const [addition, attribute, removal, detached] = deliveries[0];
  assert.deepEqual([...addition.addedNodes], [first, second]);
  assert.equal(attribute.attributeName, "data-a");
  assert.equal(attribute.oldValue, null);
  assert.deepEqual([...removal.removedNodes], [first]);
  assert.equal(removal.nextSibling, second);
  assert.equal(detached.oldValue, "one");
  first.setAttribute("data-a", "three");
  assert.equal(observer.takeRecords().length, 0);
  observer.disconnect();
});

test("abort reasons, dependent signals, and trusted synchronous events use the DOM realm", () => {
  const owner = new AbortController();
  const dependent = AbortSignal.any([owner.signal]);
  const seen = [];
  owner.signal.onabort = event => seen.push([event.isTrusted, dependent.aborted]);
  dependent.onabort = event => seen.push([event.isTrusted, dependent.reason]);
  owner.abort("reason");
  owner.abort("different reason");
  assert.deepEqual(seen, [[true, true], [true, "reason"]]);
  assert.throws(() => dependent.throwIfAborted(), error => error === "reason");
  assert.equal(AbortSignal.abort().reason.name, "AbortError");
});

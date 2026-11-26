// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { installDOM, NodeFilter, Selection } from "@strange-quark/dom";

test("focus handlers can redirect a pending focus change", () => {
  const document = installDOM({});
  const first = document.body.appendChild(document.createElement("button"));
  const outside = document.body.appendChild(document.createElement("button"));
  const redirected = document.body.appendChild(document.createElement("button"));
  const seen = [];
  document.addEventListener("focusin", e => seen.push([e.type, e.target, e.relatedTarget]));
  document.addEventListener("focusout", e => seen.push([e.type, e.target, e.relatedTarget]), true);
  first.focus();
  first.addEventListener("focusout", () => redirected.focus(), { once: true });

  outside.focus();
  assert.equal(document.activeElement, redirected);
  assert.deepEqual(seen, [
    ["focusin", first, null], ["focusout", first, outside], ["focusin", redirected, null],
  ]);
  redirected.addEventListener("blur", () => first.focus(), { once: true });
  outside.focus();
  assert.equal(document.activeElement, first);
  assert.deepEqual(seen.slice(-2), [["focusin", first, null], ["focusout", redirected, null]]);
  assert.equal(seen.some(([, target]) => target === outside), false);
});

test("text controls select UTF-16 ranges and expose the focused text through getSelection", async () => {
  const window = {};
  const document = installDOM(window);
  const input = document.body.appendChild(document.createElement("input"));
  const area = document.body.appendChild(document.createElement("textarea"));
  input.value = "A💡BC";
  input.setSelectionRange(1, 3, "backward");
  assert.equal(window.getSelection().toString(), "");
  assert(window.getSelection() instanceof Selection);
  assert.equal(window.getSelection(), document.getSelection());
  input.focus();
  assert.equal(window.getSelection().toString(), "💡");
  input.selectionStart = 2;
  assert.equal(input.selectionDirection, "backward");
  input.selectionEnd = 1;
  assert.deepEqual([input.selectionStart, input.selectionEnd, input.selectionDirection], [1, 1, "backward"]);
  input.selectionStart = -1;
  assert.deepEqual([input.selectionStart, input.selectionEnd], [5, 5]);
  assert.throws(() => input.setSelectionRange(1), TypeError);

  area.value = "one\ntwo";
  const selected = new Promise(resolve => area.addEventListener("select", resolve, { once: true }));
  area.select();
  assert.equal(document.activeElement, area);
  assert.deepEqual([area.selectionStart, area.selectionEnd], [0, 7]);
  assert.equal(window.getSelection().toString(), "one\ntwo");
  assert.equal((await selected).isTrusted, true);

  input.type = "number";
  assert.equal(input.selectionStart, null);
  assert.throws(() => input.setSelectionRange(0, 1), { name: "InvalidStateError" });
  input.type = "password";
  input.value = "secret";
  input.select();
  assert.equal(window.getSelection().toString(), "••••••");
});

test("TreeWalker distinguishes accepting, skipping and pruning live branches", () => {
  const document = installDOM({});
  const root = document.body.appendChild(document.createElement("section"));
  const skip = root.appendChild(document.createElement("div"));
  const first = skip.appendChild(document.createElement("button"));
  const reject = root.appendChild(document.createElement("p"));
  reject.appendChild(document.createElement("button"));
  const last = root.appendChild(document.createElement("button"));
  last.append("ignored text");
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, { acceptNode(node) {
    if (node === reject) return NodeFilter.FILTER_REJECT;
    return node === skip ? NodeFilter.FILTER_SKIP : NodeFilter.FILTER_ACCEPT;
  } });
  assert.equal(walker.currentNode, root);
  assert.equal(walker.nextNode(), first);
  assert.equal(walker.nextNode(), last);
  assert.equal(walker.nextNode(), null);
  assert.equal(walker.previousNode(), first);
  assert.equal(walker.parentNode(), root);
  assert.equal(walker.lastChild(), last);
  assert.equal(walker.previousSibling(), first);
  const appended = root.appendChild(document.createElement("input"));
  assert.equal(walker.nextSibling(), last);
  assert.equal(walker.nextNode(), appended);
});

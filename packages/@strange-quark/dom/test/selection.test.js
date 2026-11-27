// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import { createDocument, installDOM, Range, Selection, StaticRange } from "@strange-quark/dom";
import { Journal } from "@strange-quark/dom/journal";

function setup() {
  const window = {};
  const document = installDOM(window);
  const events = [];
  document.addEventListener("selectionchange", (event) => {
    events.push([event.target === document ? "document" : event.target.id, event.bubbles, event.isTrusted]);
  });
  const paragraph = document.body.appendChild(document.createElement("p"));
  paragraph.id = "p";
  paragraph.append("Hello world");
  return { window, document, selection: document.getSelection(), events, paragraph, text: paragraph.firstChild };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const state = (selection) => ({
  anchor: [selection.anchorNode, selection.anchorOffset],
  focus: [selection.focusNode, selection.focusOffset],
  type: selection.type, direction: selection.direction, collapsed: selection.isCollapsed, count: selection.rangeCount,
});

test("each document with a browsing context has one Selection", () => {
  const { window, document, selection } = setup();
  assert(selection instanceof Selection);
  assert.equal(window.Selection, Selection);
  assert.equal(window.getSelection(), selection);
  assert.equal(document.getSelection(), selection);
  assert.throws(() => new Selection(), TypeError);
  assert.equal(createDocument(new Journal()).getSelection(), null, "a document without a view has no selection");
  assert.deepEqual(state(selection), {
    anchor: [null, 0], focus: [null, 0], type: "None", direction: "none", collapsed: true, count: 0,
  });
  assert.equal(selection.toString(), "");
  assert.throws(() => selection.getRangeAt(0), { name: "IndexSizeError" });
  assert.equal("modify" in selection, false, "modify needs line and word layout, so it is not offered");
  assert.equal(typeof document.onselectionchange, "object");
  assert.equal("onselectionchange" in document.body, true);
});

test("addRange keeps the given range and follows its later changes", async () => {
  const { document, selection, events, paragraph, text } = setup();
  const range = document.createRange();
  range.setStart(text, 1);
  range.setEnd(text, 3);
  selection.addRange(range);
  assert.equal(selection.getRangeAt(0), range);
  assert.equal(selection.getRangeAt(0), selection.getRangeAt(0));
  assert.deepEqual(state(selection), {
    anchor: [text, 1], focus: [text, 3], type: "Range", direction: "forward", collapsed: false, count: 1,
  });
  assert.equal(selection.toString(), "el");
  assert.deepEqual(events, [], "selectionchange is queued, not dispatched synchronously");
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]]);

  const other = document.createRange();
  other.selectNode(paragraph);
  selection.addRange(other);
  assert.equal(selection.getRangeAt(0), range, "a second range is ignored");
  range.setStart(text, 0);
  assert.deepEqual(state(selection).anchor, [text, 0]);
  assert.equal(selection.toString(), "Hel");
  range.setStart(text, 0);
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]], "a Range method that changes the range is a selection change");

  text.data = "Different";
  paragraph.prepend("x");
  await tick();
  assert.deepEqual(events, [], "DOM mutations move the range without selectionchange, like Chromium");
  assert.deepEqual([range.startContainer, range.startOffset], [text, 0]);

  range.selectNodeContents(document.createElement("div"));
  assert.equal(selection.rangeCount, 0, "a range moved out of the document leaves the selection");
  range.selectNodeContents(paragraph);
  assert.equal(selection.rangeCount, 0);
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]]);

  const detached = document.createRange();
  detached.selectNodeContents(document.createElement("div"));
  selection.addRange(detached);
  assert.equal(selection.rangeCount, 0, "a range outside the document is not added");
  assert.throws(() => selection.addRange({}), TypeError);
});

test("selection methods set new ranges with Chromium's anchor, focus and direction", () => {
  const { document, selection, paragraph, text } = setup();
  selection.setBaseAndExtent(text, 1, text, 4);
  assert.deepEqual(state(selection), {
    anchor: [text, 1], focus: [text, 4], type: "Range", direction: "forward", collapsed: false, count: 1,
  });
  const forward = selection.getRangeAt(0);
  selection.setBaseAndExtent(text, 4, text, 1);
  assert.notEqual(selection.getRangeAt(0), forward, "each call associates a new range");
  assert.deepEqual(state(selection), {
    anchor: [text, 4], focus: [text, 1], type: "Range", direction: "backward", collapsed: false, count: 1,
  });
  assert.deepEqual([selection.baseNode, selection.baseOffset, selection.extentNode, selection.extentOffset], [text, 4, text, 1]);
  selection.getRangeAt(0).setStart(text, 0);
  assert.deepEqual([state(selection).anchor, state(selection).focus, selection.direction], [[text, 0], [text, 4], "forward"],
    "Chromium resets the direction when a script changes the range");

  selection.collapse(text, 2);
  assert.deepEqual(state(selection), {
    anchor: [text, 2], focus: [text, 2], type: "Caret", direction: "none", collapsed: true, count: 1,
  });
  selection.extend(text, 5);
  assert.deepEqual([state(selection).anchor, state(selection).focus, selection.direction], [[text, 2], [text, 5], "forward"]);
  selection.extend(text, 0);
  assert.deepEqual([state(selection).anchor, state(selection).focus, selection.direction], [[text, 2], [text, 0], "backward"]);
  assert.deepEqual([selection.getRangeAt(0).startOffset, selection.getRangeAt(0).endOffset], [0, 2]);
  selection.collapseToEnd();
  assert.deepEqual([state(selection).anchor, selection.direction], [[text, 2], "none"]);
  selection.extend(paragraph, 0);
  selection.collapseToStart();
  assert.deepEqual(state(selection).focus, [paragraph, 0]);
  selection.selectAllChildren(paragraph);
  assert.deepEqual(state(selection), {
    anchor: [paragraph, 0], focus: [paragraph, 1], type: "Range", direction: "forward", collapsed: false, count: 1,
  });
  selection.setPosition(document.body, 1);
  assert.deepEqual(state(selection).anchor, [document.body, 1]);
  selection.collapse(null);
  assert.equal(selection.rangeCount, 0);
  selection.selectAllChildren(text);
  assert.deepEqual([state(selection).anchor, state(selection).focus], [[text, 0], [text, 0]], "a Text node has no children");
  selection.empty();
  assert.equal(selection.rangeCount, 0);
});

test("selection methods throw and ignore points like Chromium", () => {
  const { document, selection, paragraph, text } = setup();
  assert.throws(() => selection.extend(text, 1), { name: "InvalidStateError" });
  assert.throws(() => selection.collapseToStart(), { name: "InvalidStateError" });
  assert.throws(() => selection.collapseToEnd(), { name: "InvalidStateError" });
  assert.throws(() => selection.collapse(paragraph, 9), { name: "IndexSizeError" });
  assert.throws(() => selection.collapse(document.doctype, 0), { name: "InvalidNodeTypeError" });
  assert.throws(() => selection.setBaseAndExtent(document.doctype, 0, text, 0), { name: "InvalidNodeTypeError" });
  assert.throws(() => selection.setBaseAndExtent(text, 99, text, 0), { name: "IndexSizeError" });
  assert.throws(() => selection.setBaseAndExtent(text, 0, text), TypeError);
  assert.throws(() => selection.selectAllChildren(document.doctype), { name: "InvalidNodeTypeError" });
  assert.throws(() => selection.removeRange(document.createRange()), { name: "NotFoundError" });
  assert.throws(() => selection.collapse(), TypeError);
  assert.throws(() => selection.collapse({}), TypeError);

  const detached = document.createElement("div");
  selection.collapse(paragraph, 0);
  selection.collapse(detached, 0);
  selection.setBaseAndExtent(detached, 0, detached, 0);
  selection.selectAllChildren(detached);
  selection.extend(detached, 0);
  assert.deepEqual(state(selection).anchor, [paragraph, 0], "points outside the document are ignored");
  assert.throws(() => selection.extend(text, 99), { name: "IndexSizeError" });
  assert.throws(() => selection.getRangeAt(1), { name: "IndexSizeError" });
  assert.throws(() => selection.getRangeAt(-1), { name: "IndexSizeError" });
  const range = selection.getRangeAt(0);
  selection.removeRange(range);
  assert.equal(selection.rangeCount, 0);
});

test("selectionchange fires once per task and only for real changes", async () => {
  const { document, selection, events, paragraph } = setup();
  selection.setBaseAndExtent(paragraph, 0, paragraph, 1);
  selection.selectAllChildren(paragraph);
  selection.collapse(paragraph, 0);
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]]);
  selection.collapse(paragraph, 0);
  selection.setBaseAndExtent(paragraph, 0, paragraph, 0);
  await tick();
  assert.deepEqual(events, [], "setting the same selection again is no change");
  selection.removeAllRanges();
  await tick();
  selection.removeAllRanges();
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]]);

  let fired = 0;
  document.onselectionchange = (event) => {
    fired++;
    assert.equal(event.target, document);
    if (fired === 1) selection.collapse(paragraph, 1);
  };
  selection.collapse(paragraph, 0);
  await tick();
  await tick();
  assert.equal(fired, 2, "a change made by a selectionchange listener queues another event");
  document.onselectionchange = null;
});

test("containsNode, getComposedRanges, toString and deleteFromDocument use the selection's range", () => {
  const { document, selection, paragraph } = setup();
  paragraph.textContent = "";
  const bold = document.createElement("b");
  bold.append("two");
  paragraph.append("one ", bold, " three");
  const [first, , last] = paragraph.childNodes;
  selection.setBaseAndExtent(first, 2, last, 2);
  assert.deepEqual([
    selection.containsNode(bold), selection.containsNode(bold, true), selection.containsNode(paragraph),
    selection.containsNode(paragraph, true), selection.containsNode(first), selection.containsNode(first, true),
    selection.containsNode(document.createElement("i"), true), selection.containsNode(document),
  ], [true, true, false, true, true, true, false, false], "matches Chromium, which counts a partly selected Text node");
  assert.equal(selection.toString(), "e two t");

  const [composed] = selection.getComposedRanges({ shadowRoots: [] });
  assert(composed instanceof StaticRange);
  assert.deepEqual([composed.startContainer, composed.startOffset, composed.endContainer, composed.endOffset], [first, 2, last, 2]);
  assert.equal(selection.getComposedRanges().length, 1);

  selection.deleteFromDocument();
  assert.equal(paragraph.textContent, "onhree");
  assert.deepEqual([selection.anchorNode, selection.anchorOffset, selection.isCollapsed], [paragraph, 1, true]);
  selection.removeAllRanges();
  selection.deleteFromDocument();
  assert.deepEqual(selection.getComposedRanges(), []);
  assert.equal(selection.containsNode(first, true), false);
});

test("a focused text control carries the document selection like Chromium", async () => {
  const { document, selection, events, paragraph } = setup();
  const wrapper = document.body.appendChild(document.createElement("div"));
  wrapper.id = "w";
  wrapper.append("x");
  const input = wrapper.appendChild(document.createElement("input"));
  input.id = "i";
  input.value = "Hello world";
  wrapper.append("y");
  selection.selectAllChildren(paragraph);
  await tick();
  events.length = 0;

  input.setSelectionRange(1, 4);
  await tick();
  assert.deepEqual(events.splice(0), [["i", true, true]], "an unfocused control fires its own bubbling selectionchange");
  assert.equal(selection.toString(), "Hello world", "an unfocused control leaves the document selection alone");

  input.focus();
  await tick();
  assert.deepEqual(events.splice(0), [["i", true, true]]);
  assert.deepEqual(state(selection), {
    anchor: [wrapper, 1], focus: [wrapper, 1], type: "Range", direction: "forward", collapsed: true, count: 1,
  });
  assert.equal(selection.toString(), "ell");
  const inControl = selection.getRangeAt(0);
  assert.deepEqual([inControl.startContainer, inControl.startOffset, inControl.collapsed], [wrapper, 1, true]);
  assert.equal(selection.getRangeAt(0), inControl);
  assert.equal(selection.containsNode(input, true), false);
  assert.deepEqual(selection.getComposedRanges().map((range) => [range.startContainer, range.startOffset, range.endOffset]), [[wrapper, 1, 2]]);

  input.setSelectionRange(2, 5, "backward");
  await tick();
  assert.deepEqual(events.splice(0), [["i", true, true]]);
  assert.deepEqual([selection.toString(), selection.direction, selection.type], ["llo", "backward", "Range"]);
  assert.notEqual(selection.getRangeAt(0), inControl);
  input.value = "Changed value";
  input.setSelectionRange(3, 3);
  await tick();
  assert.deepEqual(events.splice(0), [["i", true, true]], "control changes in one task coalesce");
  assert.deepEqual([selection.toString(), selection.direction, selection.type, selection.isCollapsed], ["", "none", "Caret", true]);

  selection.addRange(document.createRange());
  assert.equal(selection.getRangeAt(0).startContainer, wrapper, "the control's selection counts as a range");
  selection.collapse(paragraph.firstChild, 2);
  await tick();
  assert.deepEqual(events.splice(0), [["document", false, true]]);
  assert.equal(document.activeElement, input, "moving the selection out does not blur the control");
  assert.deepEqual(state(selection).anchor, [paragraph.firstChild, 2]);
  input.setSelectionRange(0, 3);
  await tick();
  assert.deepEqual(events.splice(0), [["i", true, true]]);
  assert.deepEqual([selection.toString(), state(selection).anchor], ["Cha", [wrapper, 1]]);

  wrapper.prepend("z");
  assert.deepEqual(state(selection).anchor, [wrapper, 2], "the caret before the control follows DOM changes");
  input.blur();
  await tick();
  assert.deepEqual(events, []);
  assert.equal(selection.toString(), "Cha", "Chromium keeps the selection in a blurred control");
  input.setSelectionRange(1, 2);
  assert.equal(selection.toString(), "Cha", "an unfocused control's later changes stay in the control");
  input.focus();
  assert.equal(selection.toString(), "h");

  input.remove();
  assert.deepEqual(state(selection), {
    anchor: [wrapper, 2], focus: [wrapper, 2], type: "Caret", direction: "none", collapsed: true, count: 1,
  }, "removing the control leaves a caret where it was");
  assert.equal(selection.toString(), "");
  selection.removeAllRanges();
  await tick();
  events.length = 0;

  const area = document.body.appendChild(document.createElement("textarea"));
  area.id = "t";
  area.value = "one\ntwo";
  area.select();
  await tick();
  assert.deepEqual(events.splice(0), [["t", true, true]]);
  assert.equal(selection.toString(), "one\ntwo");
  selection.removeAllRanges();
  assert.equal(selection.rangeCount, 0);
  assert.equal(selection.toString(), "");
  area.setSelectionRange(0, 3);
  assert.equal(selection.toString(), "one", "a focused control's change brings the selection back");
  selection.deleteFromDocument();
  assert.deepEqual([area.value, selection.toString()], ["one\ntwo", "one"], "Chromium leaves the control's value alone");

  const password = document.body.appendChild(document.createElement("input"));
  password.type = "password";
  password.value = "secret";
  password.select();
  assert.equal(selection.toString(), "••••••");

  let handled = 0;
  password.onselectionchange = (event) => { handled++; assert.equal(event.bubbles, true); };
  password.setSelectionRange(1, 2);
  password.setSelectionRange(1, 2);
  await tick();
  assert.equal(handled, 1);
});

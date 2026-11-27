// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import v8 from "node:v8";
import vm from "node:vm";
import { AbstractRange, installDOM, NodeRange, Range, StaticRange } from "@strange-quark/dom";
import { liveRangeCount } from "../dist/range.js";

function setup() {
  const window = {};
  const document = installDOM(window);
  const build = (tag, ...children) => {
    const element = document.createElement(tag);
    for (const child of children) element.append(child);
    return element;
  };
  return { window, document, build };
}

const points = (range) => [range.startContainer, range.startOffset, range.endContainer, range.endOffset];

test("ranges expose the DOM interfaces, constants and constructors Chromium has", () => {
  const { window, document } = setup();
  assert.equal(window.Range, Range);
  assert.equal(window.StaticRange, StaticRange);
  assert.equal(window.AbstractRange, AbstractRange);
  assert.equal(window.NodeRange, NodeRange);
  assert.equal(Object.getPrototypeOf(Range.prototype), NodeRange.prototype);
  assert.equal(Object.getPrototypeOf(StaticRange.prototype), NodeRange.prototype);
  assert.equal(Object.getPrototypeOf(NodeRange.prototype), AbstractRange.prototype);
  assert.throws(() => new AbstractRange(), TypeError);
  assert.throws(() => new NodeRange(), TypeError);
  assert.deepEqual([Range.START_TO_START, Range.START_TO_END, Range.END_TO_END, Range.END_TO_START], [0, 1, 2, 3]);
  assert.equal(new Range().END_TO_START, 3);

  const range = new Range();
  assert.deepEqual(points(range), [document, 0, document, 0]);
  assert.equal(range.collapsed, true);
  assert.equal(range.commonAncestorContainer, document);
  const created = document.createRange();
  assert(created instanceof Range);
  assert.notEqual(created, document.createRange());
  assert.deepEqual(points(created), [document, 0, document, 0]);
  assert.throws(() => Object.getOwnPropertyDescriptor(AbstractRange.prototype, "startOffset").get.call({}), TypeError);
  // Geometry and caret APIs need the engine, so a realm without its bindings lacks them.
  assert.equal("getClientRects" in Range.prototype, false);
  assert.equal("getBoundingClientRect" in Range.prototype, false);
  assert.equal("caretPositionFromPoint" in document, false);
  assert.equal("caretRangeFromPoint" in document, false);
  assert.equal("CaretPosition" in window, false);
});

test("setStart and setEnd keep the start before the end and validate the boundary point", () => {
  const { document, build } = setup();
  const text = document.createTextNode("hello");
  const div = document.body.appendChild(build("div", text, build("b", "bold")));
  const range = document.createRange();
  range.setStart(text, 2);
  assert.deepEqual(points(range), [text, 2, text, 2]);
  range.setEnd(div, 2);
  assert.deepEqual(points(range), [text, 2, div, 2]);
  range.setStart(div, 2);
  assert.deepEqual(points(range), [div, 2, div, 2]);
  range.setEnd(text, 1);
  assert.deepEqual(points(range), [text, 1, text, 1], "an end before the start moves the start");

  const detached = build("p", "x");
  range.setEnd(detached, 1);
  assert.deepEqual(points(range), [detached, 1, detached, 1], "another root collapses the range");
  range.setStart(text, 0);
  assert.deepEqual(points(range), [text, 0, text, 0]);

  assert.throws(() => range.setStart(text, 6), { name: "IndexSizeError" });
  assert.throws(() => range.setEnd(document.doctype, 0), { name: "InvalidNodeTypeError", code: 24 });
  assert.throws(() => range.setStart(text), TypeError);
  assert.throws(() => range.setStart({}, 0), TypeError);
  range.setStart(text, -4294967295);
  assert.equal(range.startOffset, 1, "offsets convert as unsigned long");

  range.setStartBefore(div.lastChild);
  range.setEndAfter(div.lastChild);
  assert.deepEqual(points(range), [div, 1, div, 2]);
  range.setStartAfter(text);
  range.setEndBefore(text);
  assert.deepEqual(points(range), [div, 0, div, 0]);
  assert.throws(() => range.setStartBefore(document), { name: "InvalidNodeTypeError" });
  assert.throws(() => range.setEndAfter(build("i")), { name: "InvalidNodeTypeError" });
});

test("collapse, selectNode and selectNodeContents follow the DOM Standard", () => {
  const { document, build } = setup();
  const div = document.body.appendChild(build("div", "a", build("span", "b"), "c"));
  const range = document.createRange();
  range.selectNode(div.childNodes[1]);
  assert.deepEqual(points(range), [div, 1, div, 2]);
  range.collapse();
  assert.deepEqual(points(range), [div, 2, div, 2]);
  range.selectNodeContents(div);
  assert.deepEqual(points(range), [div, 0, div, 3]);
  range.collapse(true);
  assert.deepEqual(points(range), [div, 0, div, 0]);
  range.selectNodeContents(div.firstChild);
  assert.deepEqual(points(range), [div.firstChild, 0, div.firstChild, 1]);
  range.selectNodeContents(document);
  assert.deepEqual(points(range), [document, 0, document, 2]);
  assert.throws(() => range.selectNode(document), { name: "InvalidNodeTypeError" });
  assert.throws(() => range.selectNodeContents(document.doctype), { name: "InvalidNodeTypeError" });
  assert.throws(() => range.selectNode(null), TypeError);
  range.selectNode(document.doctype);
  assert.deepEqual(points(range), [document, 0, document, 1]);
});

test("boundary point comparisons and point queries match the DOM Standard", () => {
  const { document, build } = setup();
  const inner = build("span", "inner");
  const div = document.body.appendChild(build("div", "one", inner, "three"));
  const range = document.createRange();
  range.setStart(div.firstChild, 1);
  range.setEnd(div.lastChild, 2);
  const other = document.createRange();
  other.selectNode(inner);

  assert.equal(range.compareBoundaryPoints(Range.START_TO_START, other), -1);
  assert.equal(range.compareBoundaryPoints(Range.START_TO_END, other), 1);
  assert.equal(range.compareBoundaryPoints(Range.END_TO_END, other), 1);
  assert.equal(range.compareBoundaryPoints(Range.END_TO_START, other), -1);
  assert.equal(other.compareBoundaryPoints(Range.START_TO_END, range), 1);
  assert.equal(range.compareBoundaryPoints(65536, range), 0, "how converts as unsigned short");
  assert.throws(() => range.compareBoundaryPoints(4, other), { name: "NotSupportedError" });
  assert.throws(() => range.compareBoundaryPoints(-1, other), { name: "NotSupportedError" });
  assert.throws(() => range.compareBoundaryPoints(0, {}), TypeError);
  const detached = document.createRange();
  detached.selectNode(build("p").appendChild(build("i")));
  assert.throws(() => range.compareBoundaryPoints(0, detached), { name: "WrongDocumentError" });

  assert.equal(range.comparePoint(div.firstChild, 0), -1);
  assert.equal(range.comparePoint(div.firstChild, 1), 0);
  assert.equal(range.comparePoint(inner, 0), 0);
  assert.equal(range.comparePoint(div, 3), 1);
  assert.equal(range.comparePoint(document.body, 0), -1);
  assert.throws(() => range.comparePoint(build("p"), 0), { name: "WrongDocumentError" });
  assert.throws(() => range.comparePoint(div, 4), { name: "IndexSizeError" });
  assert.throws(() => range.comparePoint(document.doctype, 0), { name: "InvalidNodeTypeError" });

  assert.equal(range.isPointInRange(inner.firstChild, 5), true);
  assert.equal(range.isPointInRange(div.lastChild, 2), true);
  assert.equal(range.isPointInRange(div.lastChild, 3), false);
  assert.equal(range.isPointInRange(build("p"), 0), false);
  assert.throws(() => range.isPointInRange(div, 9), { name: "IndexSizeError" });

  assert.equal(range.intersectsNode(inner), true);
  assert.equal(range.intersectsNode(div.firstChild), true);
  assert.equal(range.intersectsNode(div), true);
  assert.equal(range.intersectsNode(document), true);
  assert.equal(range.intersectsNode(document.head), false);
  assert.equal(range.intersectsNode(build("p")), false);
  assert.throws(() => range.intersectsNode(null), TypeError);

  assert.equal(range.commonAncestorContainer, div);
  range.setEnd(inner.firstChild, 2);
  assert.equal(range.commonAncestorContainer, div);
  range.setStart(inner.firstChild, 0);
  assert.equal(range.commonAncestorContainer, inner.firstChild);
});

test("toString concatenates the selected Text data in tree order", () => {
  const { document, build } = setup();
  const div = document.body.appendChild(build("div", "Hello ", build("b", "big ", document.createComment("skip"), build("i", "deep")), " world"));
  const range = document.createRange();
  range.selectNodeContents(div);
  assert.equal(range.toString(), "Hello big deep world");
  assert.equal(String(range), "Hello big deep world");
  range.setStart(div.firstChild, 3);
  range.setEnd(div.lastChild, 3);
  assert.equal(range.toString(), "lo big deep wo");
  range.setEnd(div.firstChild, 5);
  assert.equal(range.toString(), "lo");
  range.setStart(div.childNodes[1], 1);
  range.setEnd(div, 3);
  assert.equal(range.toString(), "deep world");
  const comment = div.childNodes[1].childNodes[1];
  range.setStart(comment, 1);
  range.setEnd(comment, 3);
  assert.equal(range.toString(), "", "Comment data is not text");
});

function fixture(document, build) {
  const div = document.body.appendChild(build("div",
    "Alpha", build("p", "one", build("b", "two"), "three"), build("p", "four", build("i", "five")), "Omega"));
  return { div, alpha: div.childNodes[0], first: div.childNodes[1], second: div.childNodes[2], omega: div.childNodes[3] };
}

test("cloneContents copies partially selected ancestors shallowly and leaves the tree alone", () => {
  const { document, build } = setup();
  const { div, alpha, first, second } = fixture(document, build);
  const before = div.outerHTML;
  const range = document.createRange();
  range.setStart(alpha, 2);
  range.setEnd(second.lastChild.firstChild, 2);
  const fragment = range.cloneContents();
  assert.equal(fragment.ownerDocument, document);
  assert.deepEqual([...fragment.childNodes].map(node => node.nodeName), ["#text", "P", "P"]);
  assert.equal(fragment.childNodes[0].data, "pha");
  assert.equal(fragment.childNodes[1].outerHTML, "<p>one<b>two</b>three</p>");
  assert.equal(fragment.childNodes[2].outerHTML, "<p>four<i>fi</i></p>");
  assert.notEqual(fragment.childNodes[1], first);
  assert.equal(div.outerHTML, before);
  assert.deepEqual(points(range), [alpha, 2, second.lastChild.firstChild, 2]);

  range.setStart(first.firstChild, 1);
  range.setEnd(first.firstChild, 3);
  assert.equal(range.cloneContents().firstChild.data, "ne");
  range.collapse();
  assert.equal(range.cloneContents().childNodes.length, 0);
  range.selectNodeContents(document);
  assert.throws(() => range.cloneContents(), { name: "HierarchyRequestError" });
});

test("extractContents moves contained nodes and collapses the range where they were", () => {
  const { document, build } = setup();
  const { div, alpha, first, second, omega } = fixture(document, build);
  const range = document.createRange();
  range.setStart(first.firstChild, 1);
  range.setEnd(second.lastChild.firstChild, 2);
  const kept = first.childNodes[1];
  const fragment = range.extractContents();
  assert.equal(fragment.childNodes.length, 2);
  assert.equal(fragment.childNodes[0].outerHTML, "<p>ne<b>two</b>three</p>");
  assert.equal(fragment.childNodes[0].childNodes[1], kept, "contained nodes move rather than clone");
  assert.equal(fragment.childNodes[1].outerHTML, "<p>four<i>fi</i></p>");
  assert.equal(div.outerHTML, "<div>Alpha<p>o</p><p><i>ve</i></p>Omega</div>");
  assert.deepEqual(points(range), [div, 2, div, 2]);

  range.setStart(alpha, 1);
  range.setEnd(alpha, 4);
  assert.equal(range.extractContents().firstChild.data, "lph");
  assert.equal(alpha.data, "Aa");
  assert.deepEqual(points(range), [alpha, 1, alpha, 1]);

  range.setStart(div, 1);
  range.setEnd(omega, 2);
  const tail = range.extractContents();
  assert.deepEqual([...tail.childNodes].map(node => node.nodeName), ["P", "P", "#text"]);
  assert.equal(tail.lastChild.data, "Om");
  assert.equal(div.outerHTML, "<div>Aaega</div>");
  assert.deepEqual(points(range), [div, 1, div, 1]);
});

test("deleteContents removes the selection and collapses the range to the start", () => {
  const { document, build } = setup();
  const { div, alpha, first, second } = fixture(document, build);
  const range = document.createRange();
  range.setStart(first.childNodes[1].firstChild, 1);
  range.setEnd(second.firstChild, 2);
  range.deleteContents();
  assert.equal(div.outerHTML, "<div>Alpha<p>one<b>t</b></p><p>ur<i>five</i></p>Omega</div>");
  assert.deepEqual(points(range), [div, 2, div, 2]);

  range.setStart(alpha, 0);
  range.setEnd(alpha, 2);
  range.deleteContents();
  assert.equal(alpha.data, "pha");
  assert.deepEqual(points(range), [alpha, 0, alpha, 0]);

  range.selectNodeContents(div);
  range.deleteContents();
  assert.equal(div.outerHTML, "<div></div>");
  assert.deepEqual(points(range), [div, 0, div, 0]);
  range.deleteContents();
  assert.equal(div.childNodes.length, 0);
});

test("insertNode splits text, inserts before the start and grows a collapsed range", () => {
  const { document, build } = setup();
  const text = document.createTextNode("HelloWorld");
  const div = document.body.appendChild(build("div", text));
  const range = document.createRange();
  range.setStart(text, 5);
  const span = build("span", " ");
  range.insertNode(span);
  assert.equal(div.outerHTML, "<div>Hello<span> </span>World</div>");
  assert.deepEqual(points(range), [text, 5, div, 2]);

  range.setStart(div, 0);
  range.setEnd(div, 1);
  const fragment = document.createDocumentFragment();
  fragment.append(build("b", "1"), build("b", "2"));
  range.insertNode(fragment);
  assert.equal(div.outerHTML, "<div><b>1</b><b>2</b>Hello<span> </span>World</div>");
  assert.deepEqual(points(range), [div, 0, div, 3]);

  range.setStart(div, 5);
  range.collapse(true);
  range.insertNode(div.firstChild);
  assert.equal(div.outerHTML, "<div><b>2</b>Hello<span> </span>World<b>1</b></div>");
  assert.deepEqual(points(range), [div, 4, div, 5]);

  const comment = div.appendChild(document.createComment("c"));
  range.setStart(comment, 0);
  assert.throws(() => range.insertNode(build("i")), { name: "HierarchyRequestError" });
  range.setStart(div, 0);
  assert.throws(() => range.insertNode(div), { name: "HierarchyRequestError" });
  const loose = document.createTextNode("loose");
  range.setStart(loose, 1);
  assert.throws(() => range.insertNode(build("i")), { name: "HierarchyRequestError" });
  assert.equal(loose.data, "loose");
  range.setStart(text, 2);
  assert.throws(() => range.insertNode(document), { name: "HierarchyRequestError" });
  assert.equal(text.data, "Hello", "validation runs before the text splits");
  range.setStart(document, 1);
  assert.throws(() => range.insertNode(build("i")), { name: "HierarchyRequestError" });
  assert.throws(() => range.insertNode(document.createTextNode("x")), { name: "HierarchyRequestError" });
});

test("surroundContents wraps the selection and selects the new parent", () => {
  const { document, build } = setup();
  const text = document.createTextNode("one two three");
  const div = document.body.appendChild(build("div", text));
  const range = document.createRange();
  range.setStart(text, 4);
  range.setEnd(text, 7);
  const strong = build("strong", "old child");
  range.surroundContents(strong);
  assert.equal(div.outerHTML, "<div>one <strong>two</strong> three</div>");
  assert.deepEqual(points(range), [div, 1, div, 2]);

  range.setStart(div.firstChild, 1);
  range.setEnd(strong.firstChild, 1);
  assert.throws(() => range.surroundContents(build("em")), { name: "InvalidStateError" });
  range.selectNodeContents(div);
  assert.throws(() => range.surroundContents(document.createDocumentFragment()), { name: "InvalidNodeTypeError" });
  assert.throws(() => range.surroundContents(document), { name: "InvalidNodeTypeError" });
  range.surroundContents(build("section"));
  assert.equal(div.outerHTML, "<div><section>one <strong>two</strong> three</section></div>");
});

test("cloneRange copies the boundary points into an independent live range and detach does nothing", () => {
  const { document, build } = setup();
  const div = document.body.appendChild(build("div", "abc"));
  const range = document.createRange();
  range.setStart(div.firstChild, 1);
  range.setEnd(div, 1);
  const clone = range.cloneRange();
  assert.notEqual(clone, range);
  assert.deepEqual(points(clone), points(range));
  clone.collapse(true);
  assert.deepEqual(points(range), [div.firstChild, 1, div, 1]);
  div.prepend("x");
  assert.deepEqual(points(clone), [div.childNodes[1], 1, div.childNodes[1], 1]);
  assert.deepEqual(points(range), [div.childNodes[1], 1, div, 2]);
  range.detach();
  assert.deepEqual(points(range), [div.childNodes[1], 1, div, 2]);
});

test("StaticRange stores its boundary points unchecked and does not track mutations", () => {
  const { document, build } = setup();
  const div = document.body.appendChild(build("div", "abc", "def"));
  const range = new StaticRange({ startContainer: div.firstChild, startOffset: 1, endContainer: div, endOffset: 15 });
  assert.deepEqual(points(range), [div.firstChild, 1, div, 15]);
  assert.equal(range.collapsed, false);
  assert.equal("commonAncestorContainer" in range, false);
  div.firstChild.remove();
  assert.deepEqual(points(range), [range.startContainer, 1, div, 15]);
  const inverted = new StaticRange({ startContainer: div, startOffset: 1, endContainer: document.body, endOffset: 0 });
  assert.equal(inverted.collapsed, false);
  const wrapped = new StaticRange({ startContainer: div, startOffset: -1, endContainer: div, endOffset: 2 ** 32 + 5 });
  assert.deepEqual([wrapped.startOffset, wrapped.endOffset], [4294967295, 5]);
  assert.throws(() => new StaticRange(), TypeError);
  assert.throws(() => new StaticRange({ startOffset: 0, endContainer: div, endOffset: 0 }), TypeError);
  assert.throws(() => new StaticRange({ startContainer: div, endContainer: div, endOffset: 0 }), TypeError);
  assert.throws(() => new StaticRange({ startContainer: null, startOffset: 0, endContainer: div, endOffset: 0 }), TypeError);
  assert.throws(() => new StaticRange({ startContainer: document.doctype, startOffset: 0, endContainer: div, endOffset: 0 }),
    { name: "InvalidNodeTypeError" });
});

test("live ranges follow insertions, removals and replacements", () => {
  const { document, build } = setup();
  const [a, b, c] = [build("a", "A"), build("b", "B"), build("c", "C")];
  const div = document.body.appendChild(build("div", a, b, c));
  const range = document.createRange();
  range.setStart(div, 1);
  range.setEnd(div, 3);
  div.insertBefore(build("x"), b);
  assert.deepEqual(points(range), [div, 1, div, 4], "an insertion at the start offset leaves the start");
  div.appendChild(build("y"));
  assert.deepEqual(points(range), [div, 1, div, 4], "appending never moves a boundary");
  div.prepend("text");
  assert.deepEqual(points(range), [div, 2, div, 5]);
  div.insertBefore(c, b);
  assert.deepEqual(points(range), [div, 2, div, 5], "a move is a removal and an insertion");
  range.setEnd(c.firstChild, 1);
  c.remove();
  assert.deepEqual(points(range), [div, 2, div, 3], "a removed boundary collapses to the removed node's place");
  div.replaceChild(build("z"), div.childNodes[1]);
  assert.deepEqual(points(range), [div, 1, div, 3]);
  range.selectNodeContents(b.firstChild);
  div.replaceChild(build("w"), b);
  assert.deepEqual(points(range), [div, 3, div, 3]);
  const fragment = document.createDocumentFragment();
  fragment.append("f1", "f2", "f3");
  const fragmentRange = document.createRange();
  fragmentRange.setStart(fragment, 1);
  fragmentRange.setEnd(fragment.lastChild, 1);
  div.insertBefore(fragment, div.firstChild);
  assert.deepEqual(points(range), [div, 6, div, 6], "a fragment insertion shifts by its child count");
  assert.deepEqual(points(fragmentRange), [fragment, 0, fragment, 0]);
  div.textContent = "";
  assert.deepEqual(points(range), [div, 0, div, 0]);
  const html = document.documentElement;
  range.setStart(document.body, 0);
  document.body.remove();
  assert.deepEqual(points(range), [html, 1, html, 1]);
});

test("live ranges follow character data changes, splitText and normalize", () => {
  const { document, build } = setup();
  const text = document.createTextNode("Hello world");
  const div = document.body.appendChild(build("div", text));
  const range = document.createRange();
  range.setStart(text, 2);
  range.setEnd(text, 8);
  text.insertData(0, ">>");
  assert.deepEqual(points(range), [text, 4, text, 10]);
  text.deleteData(5, 3);
  assert.deepEqual(points(range), [text, 4, text, 7]);
  text.replaceData(3, 2, "abcd");
  assert.deepEqual(points(range), [text, 3, text, 9]);
  text.appendData("!");
  assert.deepEqual(points(range), [text, 3, text, 9]);
  text.data = "reset";
  assert.deepEqual(points(range), [text, 0, text, 0]);
  range.setEnd(text, 5);
  text.textContent = "again";
  assert.deepEqual(points(range), [text, 0, text, 0]);
  range.setStart(text, 1);
  range.setEnd(text, 4);
  text.nodeValue = "n";
  assert.deepEqual(points(range), [text, 0, text, 0]);

  text.data = "abcdef";
  range.setStart(text, 2);
  range.setEnd(text, 5);
  const after = document.createRange();
  after.setStart(div, 1);
  const tail = text.splitText(3);
  assert.equal(text.data, "abc");
  assert.equal(tail.data, "def");
  assert.deepEqual(points(range), [text, 2, tail, 2]);
  assert.deepEqual(points(after), [div, 2, div, 2]);
  const loose = document.createTextNode("loose");
  const looseRange = document.createRange();
  looseRange.setStart(loose, 4);
  loose.splitText(2);
  assert.deepEqual(points(looseRange), [loose, 2, loose, 2], "without a parent only replace data applies");

  div.insertBefore(document.createTextNode(""), tail);
  div.append("ghi");
  range.setEnd(div.lastChild, 1);
  after.setStart(div, 2);
  after.setEnd(div, 4);
  div.normalize();
  assert.equal(div.childNodes.length, 1);
  assert.equal(text.data, "abcdefghi");
  assert.deepEqual(points(range), [text, 2, text, 7]);
  assert.deepEqual(points(after), [text, 3, div, 1]);
});

function lcg(seed) {
  return () => (seed = (Math.imul(seed, 1103515245) + 12345) >>> 0) / 2 ** 32;
}

function treeNodes(root) {
  const all = [root];
  for (let index = 0; index < all.length; index++) all.push(...all[index].childNodes);
  return all;
}

function length(node) {
  return node.nodeType === 3 || node.nodeType === 8 ? node.data.length : node.childNodes.length;
}

function indexOf(node) { return [...node.parentNode.childNodes].indexOf(node); }

function contains(ancestor, node) {
  for (let current = node; current !== null; current = current.parentNode) if (current === ancestor) return true;
  return false;
}

function forRemove(removed, [node, offset]) {
  const parent = removed.parentNode;
  if (parent === null) return [node, offset];
  const index = indexOf(removed);
  if (contains(removed, node)) return [parent, index];
  return node === parent && offset > index ? [node, offset - 1] : [node, offset];
}

function forInsert(inserted, [node, offset]) {
  return node === inserted.parentNode && offset > indexOf(inserted) ? [node, offset + 1] : [node, offset];
}

function forReplaceData(target, offset, count, added, [node, offset_]) {
  if (node !== target) return [node, offset_];
  if (offset_ > offset && offset_ <= offset + count) return [node, offset];
  return offset_ > offset + count ? [node, offset_ + added - count] : [node, offset_];
}

test("live ranges match the WPT mutation model across random trees and mutations", () => {
  const { document, build } = setup();
  const random = lcg(20261127);
  const pick = (items) => items[Math.floor(random() * items.length)];
  for (let round = 0; round < 300; round++) {
    const root = document.body.appendChild(build("div"));
    for (let count = 0; count < 12; count++) {
      const parent = pick(treeNodes(root).filter((node) => node.nodeType === 1));
      const kind = random();
      parent.appendChild(kind < 0.45 ? document.createTextNode("text".slice(0, Math.floor(random() * 5))) :
        kind < 0.55 ? document.createComment("note") : build(pick(["span", "b", "p"])));
    }
    const nodes = treeNodes(root);
    const ranges = Array.from({ length: 6 }, () => {
      const range = document.createRange();
      const node = pick(nodes);
      range.setStart(node, Math.floor(random() * (length(node) + 1)));
      const end = pick(nodes);
      range.setEnd(end, Math.floor(random() * (length(end) + 1)));
      return range;
    });
    const before = ranges.map((range) => [[range.startContainer, range.startOffset], [range.endContainer, range.endOffset]]);
    const elements = nodes.filter((node) => node.nodeType === 1);
    const characterData = nodes.filter((node) => node.nodeType === 3 || node.nodeType === 8);
    const texts = nodes.filter((node) => node.nodeType === 3 && node.parentNode !== null);
    const operation = Math.floor(random() * 6);
    let expected;
    let label;
    if (operation === 0 || operation === 1) {
      const parent = pick(elements);
      const moved = random() < 0.5 ? build("em") : pick(nodes.filter((node) => node !== root && !contains(node, parent)));
      if (moved === undefined) continue;
      const children = [...parent.childNodes];
      const reference = operation === 0 ? null : children.length === 0 ? null : pick(children);
      label = `insertBefore(${moved.nodeName}, ${reference?.nodeName})`;
      expected = before.map((pair) => pair.map((point) => forRemove(moved, point)));
      parent.insertBefore(moved, reference);
      expected = expected.map((pair) => pair.map((point) => forInsert(moved, point)));
    } else if (operation === 2) {
      const removed = pick(nodes.filter((node) => node !== root));
      if (removed === undefined) continue;
      label = `removeChild(${removed.nodeName})`;
      expected = before.map((pair) => pair.map((point) => forRemove(removed, point)));
      removed.remove();
    } else if (operation === 3) {
      const replaced = pick(nodes.filter((node) => node !== root));
      if (replaced === undefined) continue;
      const replacement = build("u");
      label = `replaceChild(${replaced.nodeName})`;
      expected = before.map((pair) => pair.map((point) => forRemove(replaced, point)));
      replaced.parentNode.replaceChild(replacement, replaced);
      expected = expected.map((pair) => pair.map((point) => forInsert(replacement, point)));
    } else if (operation === 4) {
      const target = pick(characterData);
      if (target === undefined) continue;
      const offset = Math.floor(random() * (target.length + 1));
      const count = Math.floor(random() * (target.length + 2));
      const data = "xyz".slice(0, Math.floor(random() * 4));
      label = `replaceData(${offset}, ${count}, ${data})`;
      const removed = Math.min(count, target.length - offset);
      expected = before.map((pair) => pair.map((point) => forReplaceData(target, offset, removed, data.length, point)));
      target.replaceData(offset, count, data);
    } else {
      const target = pick(texts);
      if (target === undefined) continue;
      const offset = Math.floor(random() * (target.length + 1));
      const parent = target.parentNode;
      const index = indexOf(target);
      label = `splitText(${offset})`;
      const split = target.splitText(offset);
      expected = before.map((pair) => pair.map(([node, value]) => {
        if (node === target && value > offset) return [split, value - offset];
        if (node === parent && value > index) return [node, value + 1];
        return [node, value];
      }));
    }
    for (const [position, range] of ranges.entries()) {
      assert.deepEqual(points(range), expected[position].flat(), `round ${round}: ${label}`);
    }
    root.remove();
  }
});

test("live ranges stop being tracked once they are collected", async () => {
  v8.setFlagsFromString("--expose-gc");
  const gc = vm.runInNewContext("gc");
  const { document } = setup();
  gc();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const baseline = liveRangeCount();
  (() => {
    for (let count = 0; count < 2000; count++) document.createRange().selectNodeContents(document.body);
  })();
  assert(liveRangeCount() >= baseline + 2000);
  for (let attempt = 0; attempt < 20 && liveRangeCount() > baseline + 100; attempt++) {
    gc();
    await new Promise((resolve) => setTimeout(resolve, 10));
    document.body.append("tick");
  }
  assert(liveRangeCount() <= baseline + 100, `expected collected ranges to leave the live set, ${liveRangeCount()} remain`);
});

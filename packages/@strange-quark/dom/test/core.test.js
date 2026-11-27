// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import test from "node:test";
import {
  Comment, createDocument, Document, DocumentFragment, DOMException,
  Element, Event, EventTarget, installDOM, Node, Text,
} from "@strange-quark/dom";
import { Journal, ListenerFlags, Opcode, RECORD_BYTES } from "@strange-quark/dom/journal";

function records(journal) {
  const batch = new DataView(journal.snapshot());
  const end = 16 + batch.getUint32(0, true);
  const result = [];
  for (let offset = 16; offset < end; offset += RECORD_BYTES) {
    result.push([batch.getUint8(offset), ...[4, 8, 12].map((step) => batch.getUint32(offset + step, true))]);
  }
  return result;
}

test("document.cookie presents an empty cookie jar and ignores writes", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const before = journal.snapshot();
  assert.equal(document.cookie.includes("excplus-auth"), false);
  document.cookie = "excplus-auth=1; Path=/";
  assert.equal(document.cookie, "");
  assert.deepEqual(journal.snapshot(), before);
});

test("document.scripts is a live collection of HTML script elements", () => {
  const document = createDocument(new Journal());
  const head = document.createElement("head");
  const root = document.createElement("html");
  document.append(root);
  root.append(head);
  const scripts = document.scripts;
  const script = document.createElement("script");
  script.id = "boot";
  head.append(script, document.createElementNS("http://www.w3.org/2000/svg", "script"));
  assert.deepEqual([...scripts], [script]);
  assert.equal(scripts.namedItem("boot"), script);
  script.remove();
  assert.equal(scripts.length, 0);
});

test("innerText renders whitespace, blocks and br and replaces text on assignment", () => {
  const document = createDocument(new Journal());
  const root = document.createElement("main");
  document.append(root);
  const hidden = document.createElement("span");
  hidden.hidden = true;
  hidden.textContent = "hidden";
  const paragraph = document.createElement("p");
  paragraph.append("  three ", document.createElement("br"), "  four  ");
  const pre = document.createElement("pre");
  pre.textContent = "  five  \n six";
  root.append(" one  ", hidden, "  two ", paragraph, pre);
  assert.equal(root.innerText, "one two\n\nthree\nfour\n\n  five  \n six");
  assert.equal(hidden.innerText, "hidden");
  root.innerText = "a\r\nb\rc\n";
  assert.equal(root.innerText, "a\nb\nc\n");
  assert.equal(root.innerHTML, "a<br>b<br>c<br>");
  assert.equal(root.textContent, "abc");
});

test("listener storage is isolated and blocking flags follow registration lifetime", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const form = document.createElement("form");
  const sibling = document.createElement("div");
  let calls = 0;
  form.addEventListener("wheel", () => calls++, { once: true, passive: false });
  sibling.addEventListener("wheel", () => calls += 10, { passive: true });
  form.dispatchEvent(new Event("wheel"));
  form.dispatchEvent(new Event("wheel"));
  sibling.dispatchEvent(new Event("wheel"));
  assert.equal(calls, 11);
  assert.deepEqual(records(journal).filter(([op]) => op === Opcode.Listeners), [
    [Opcode.Listeners, 1, ListenerFlags.NonPassiveWheel, 0],
    [Opcode.Listeners, 1, 0, 0],
  ]);
});

test("mutations maintain live child collections, identity and journal order", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const parent = document.createElement("main");
  const first = document.createElement("b");
  const fragment = document.createDocumentFragment();
  const text = document.createTextNode("a\ud800");
  const comment = document.createComment("unrendered");
  assert(parent instanceof Element && parent instanceof Node);
  assert(text instanceof Text && comment instanceof Comment && fragment instanceof DocumentFragment);

  parent.appendChild(first);
  const children = parent.childNodes;
  assert.equal(parent.childNodes, children);
  assert.equal(children[0], first);
  fragment.append(text, comment);
  const initialCount = records(journal).length;
  assert.equal(parent.insertBefore(fragment, first), fragment);
  assert.equal(fragment.firstChild, null);
  assert.deepEqual([...children], [text, comment, first]);
  assert.equal(text.parentElement, parent);
  assert.equal(text.nextSibling, comment);
  assert.equal(first.previousSibling, comment);
  assert.equal(children.item(4), null);
  assert.equal(parent.textContent, "a\ud800");
  assert.deepEqual(records(journal).slice(initialCount), [
    [Opcode.Remove, 3, 4, 0], [Opcode.Remove, 3, 5, 0],
    [Opcode.Insert, 1, 4, 2], [Opcode.Insert, 1, 5, 2],
  ]);

  assert.equal(parent.replaceChild(document.createTextNode("new"), first), first);
  assert.equal(first.parentNode, null);
  assert.equal(children.length, 3);
  parent.removeChild(comment);
  parent.normalize();
  assert.equal(children.length, 1);
  assert.equal(text.data, "a\ud800new");
  const tail = text.splitText(2);
  assert.equal(tail.data, "new");
  assert.equal(tail.previousSibling, text);
  assert.equal(text.wholeText, "a\ud800new");
  parent.textContent = null;
  assert.equal(parent.hasChildNodes(), false);
});

test("document hierarchy errors leave the tree and journal intact", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  assert(document instanceof Document);
  const root = document.appendChild(document.createElement("root"));
  const other = document.createElement("other");
  const fragment = document.createDocumentFragment();
  fragment.append(other, document.createTextNode("invalid child for a document"));
  const before = journal.snapshot();
  const expected = (name) => (error) => error instanceof DOMException && error.name === name;
  assert.throws(() => document.appendChild(fragment), expected("HierarchyRequestError"));
  assert.throws(() => root.appendChild(document), expected("HierarchyRequestError"));
  assert.throws(() => root.insertBefore(other, other), expected("NotFoundError"));
  assert.throws(() => root.removeChild(other), expected("NotFoundError"));
  assert.deepEqual(journal.snapshot(), before);
  assert.equal(document.documentElement, root);
  assert.equal(other.parentNode, fragment);
  assert.equal(root.getRootNode(), document);
  assert.equal(root.isConnected, true);
  assert.equal(other.isConnected, false);
  assert.equal(document.compareDocumentPosition(root),
    Node.DOCUMENT_POSITION_FOLLOWING | Node.DOCUMENT_POSITION_CONTAINED_BY);
  assert.equal(root.compareDocumentPosition(document),
    Node.DOCUMENT_POSITION_PRECEDING | Node.DOCUMENT_POSITION_CONTAINS);
  const disconnected = Node.DOCUMENT_POSITION_DISCONNECTED | Node.DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC;
  assert.equal(root.compareDocumentPosition(other) & disconnected, disconnected);
  assert.equal(root.compareDocumentPosition(other) ^ other.compareDocumentPosition(root),
    Node.DOCUMENT_POSITION_PRECEDING | Node.DOCUMENT_POSITION_FOLLOWING);
});

test("classList and dataset are live views of journaled attributes", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const element = document.createElement("DIV");
  const list = element.classList;
  const data = element.dataset;

  element.setAttribute("CLASS", "  alpha beta alpha ");
  assert.deepEqual([...list], ["alpha", "beta"]);
  assert.equal(list[1], "beta");
  assert.equal(list.replace("alpha", "other"), true);
  assert.equal(list.toggle("beta", false), false);
  assert.equal(element.className, "other");
  assert.throws(() => list.add("valid", "two words"), { name: "InvalidCharacterError" });
  assert.equal(element.className, "other");

  data.userId = 42;
  assert.equal(element.getAttribute("data-user-id"), "42");
  element.setAttribute("data-flag", "");
  assert.deepEqual(Object.keys(data), ["userId", "flag"]);
  assert.equal(data.flag, "");
  assert.throws(() => { data["a-b"] = "x"; }, { name: "SyntaxError" });
  delete data.userId;
  assert.equal(element.hasAttribute("data-user-id"), false);
  assert.equal(element.dataset, data);
  assert.equal(element.classList, list);
  assert(records(journal).some(([op]) => op === Opcode.RemoveAttr));
});

test("namespaced attribute value updates keep the qualified name used in the journal", () => {
  const journal = new Journal();
  const document = createDocument(journal);
  const element = document.createElementNS("urn:element", "e:item");
  assert.equal(element.tagName, "e:item");
  element.setAttributeNS("urn:attribute", "a:name", "first");
  element.setAttributeNS("urn:attribute", "b:name", "updated");
  assert.equal(element.getAttribute("a:name"), "updated");
  assert.equal(element.getAttribute("b:name"), null);
  assert.equal(element.attributes.length, 1);
  element.removeAttributeNS("urn:attribute", "name");
  const attributes = records(journal).filter(([op]) => op === Opcode.SetAttr || op === Opcode.RemoveAttr);
  assert.equal(attributes[0][2], attributes[1][2]);
  assert.equal(attributes[0][2], attributes[2][2]);
});

test("dispatch snapshots the capture path and applies cancellation and listener lifetime", () => {
  const window = {};
  const document = installDOM(window);
  const parent = document.body.appendChild(document.createElement("div"));
  const child = parent.appendChild(document.createTextNode("child"));
  const controller = new AbortController();
  const log = [];
  const track = (label) => (event) => { log.push([label, event.eventPhase, event.currentTarget]); };
  window.addEventListener("x", track("window"), true);
  document.addEventListener("x", track("document"), true);
  parent.addEventListener("x", (event) => {
    assert.deepEqual(event.composedPath(), [child, parent, document.body, document.documentElement, document, window]);
    child.remove();
    controller.abort();
    track("parent capture")(event);
  }, { capture: true, once: true });
  child.addEventListener("x", (event) => {
    event.preventDefault();
    assert.equal(event.defaultPrevented, false);
    track("passive target")(event);
  }, { passive: true });
  child.addEventListener("x", (event) => event.preventDefault(), { signal: controller.signal });
  parent.addEventListener("x", (event) => {
    track("bubble")(event);
    event.preventDefault();
  });
  const event = new Event("x", { bubbles: true, cancelable: true });
  assert.equal(child.dispatchEvent(event), false);
  assert.deepEqual(log, [
    ["window", Event.CAPTURING_PHASE, window], ["document", Event.CAPTURING_PHASE, document],
    ["parent capture", Event.CAPTURING_PHASE, parent], ["passive target", Event.AT_TARGET, child],
    ["bubble", Event.BUBBLING_PHASE, parent],
  ]);
  assert.equal(event.target, child);
  assert.equal(event.currentTarget, null);
  assert.deepEqual(event.composedPath(), []);
  assert.equal(child.parentNode, null);
  assert.equal(child.dispatchEvent(new Event("x")), true);
});

test("active listener removal, duplicates and stopImmediatePropagation follow dispatch order", () => {
  const target = new EventTarget();
  const log = [];
  const skipped = () => log.push("skipped");
  const action = {
    handleEvent(event) {
      assert.equal(this, action);
      log.push("action");
      target.removeEventListener("x", skipped);
      event.stopImmediatePropagation();
    },
  };
  target.addEventListener("x", action, { once: true });
  target.addEventListener("x", action);
  target.addEventListener("x", skipped);
  target.addEventListener("x", () => log.push("last"));
  target.dispatchEvent(new Event("x"));
  target.dispatchEvent(new Event("x"));
  assert.deepEqual(log, ["action", "last"]);
});

test("DOMException reports the legacy codes of named errors", () => {
  for (const [name, code] of [["SyntaxError", 12], ["SecurityError", 18], ["NetworkError", 19], ["AbortError", 20], ["DataCloneError", 25], ["OperationError", 0]]) {
    assert.equal(new DOMException("", name).code, code, name);
  }
});
